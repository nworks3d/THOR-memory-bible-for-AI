//! Blind acceptance measurement for the Response Guard (`serve::respond`).
//!
//! Runs two eval sets written BEFORE the rulebook or matcher source was ever
//! read (only the six plain-language rule intents were known at authoring
//! time) through the REAL matcher (`serve::respond::block_reason`) against
//! the REAL live rulebook (named by `RESPONSE_GUARD_EVAL_RULEBOOK`, read
//! directly - never copied, never modified):
//!
//!   - `serve/eval/response-guard-should-block.json` (60 replies, each
//!     genuinely breaking one of the six rules, labelled with the rule
//!     number it breaks) -> reports the catch rate, overall and per rule.
//!   - `serve/eval/response-guard-should-pass.json` (60 replies that must
//!     NOT be blocked, labelled by class) -> reports the false-block rate,
//!     overall and per class, plus every false block quoted in full.
//!
//! Pure text matching only - no store is opened, nothing here writes to the
//! live rulebook, the live db, or any settings file.
//!
//! Run (from the `thor2` workspace root):
//!   cargo run --example response_guard_eval
//!
//! Both JSON paths, and the rulebook path, can be overridden via env vars
//! (RESPONSE_GUARD_EVAL_BLOCK / RESPONSE_GUARD_EVAL_PASS /
//! RESPONSE_GUARD_EVAL_RULEBOOK) for pointing this at a different set
//! without editing this file.

use serde_json::Value;
use serve::respond;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn rulebook_path() -> PathBuf {
    std::env::var("RESPONSE_GUARD_EVAL_RULEBOOK")
        .map(PathBuf::from)
        .map(PathBuf::from)
        .expect("set RESPONSE_GUARD_EVAL_RULEBOOK to the live rulebook this measurement is about - there is deliberately no default, because a real path here is one machine's and this repository is public")
}

fn should_block_path() -> PathBuf {
    std::env::var("RESPONSE_GUARD_EVAL_BLOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/eval/response-guard-should-block.json"))
        })
}

fn should_pass_path() -> PathBuf {
    std::env::var("RESPONSE_GUARD_EVAL_PASS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/eval/response-guard-should-pass.json"))
        })
}

struct Case {
    id: String,
    reply: String,
    rule: Option<u64>,
    class: String,
}

fn load_cases(path: &std::path::Path) -> Vec<Case> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("could not parse {}: {e}", path.display()));
    v.as_array()
        .unwrap_or_else(|| panic!("{} is not a JSON array", path.display()))
        .iter()
        .map(|x| Case {
            id: x.get("id").and_then(|c| c.as_str()).unwrap_or("(no id)").to_string(),
            reply: x.get("reply").and_then(|c| c.as_str()).unwrap_or("").to_string(),
            rule: x.get("rule").and_then(|c| c.as_u64()),
            class: x.get("class").and_then(|c| c.as_str()).unwrap_or("(no class)").to_string(),
        })
        .collect()
}

fn main() {
    let rulebook_path = rulebook_path();
    let rulebook_text = std::fs::read_to_string(&rulebook_path)
        .unwrap_or_else(|e| panic!("could not read rulebook at {}: {e}", rulebook_path.display()));
    let rule_count = respond::parse_rules(&rulebook_text).len();

    let should_block = load_cases(&should_block_path());
    let should_pass = load_cases(&should_pass_path());

    println!("Response Guard blind acceptance measurement");
    println!("rulebook     : {} ({rule_count} rule(s))", rulebook_path.display());
    println!("should-block : {} (n={})", should_block_path().display(), should_block.len());
    println!("should-pass  : {} (n={})", should_pass_path().display(), should_pass.len());
    println!();

    // --- Set A: catch rate, overall and per rule ----------------------
    let mut caught_total = 0usize;
    let mut by_rule: BTreeMap<u64, (usize, usize)> = BTreeMap::new(); // rule -> (caught, total)
    let mut missed: Vec<&Case> = Vec::new();

    let mut caught_reasons: Vec<(String, u64, String)> = Vec::new(); // (id, rule, reason)
    for case in &should_block {
        let rule_key = case.rule.unwrap_or(0);
        let entry = by_rule.entry(rule_key).or_insert((0, 0));
        entry.1 += 1;
        match respond::block_reason(Some(&rulebook_text), &case.reply) {
            Some(reason) => {
                caught_total += 1;
                entry.0 += 1;
                caught_reasons.push((case.id.clone(), rule_key, reason));
            }
            None => missed.push(case),
        }
    }
    let n_a = should_block.len().max(1);
    println!(
        "catch rate (overall) : {caught_total}/{} ({:.1}%)",
        should_block.len(),
        100.0 * caught_total as f64 / n_a as f64
    );
    println!("catch rate by rule:");
    for (rule, (caught, total)) in &by_rule {
        println!("  rule {rule:<2} {caught}/{total} ({:.1}%)", 100.0 * *caught as f64 / *total as f64);
    }
    if !missed.is_empty() {
        println!();
        println!("missed (rule violation NOT caught):");
        for m in &missed {
            println!("  [rule {:?} | {}] {}", m.rule, m.class, m.id);
        }
    }
    if !caught_reasons.is_empty() {
        println!();
        println!("caught (id, rule, firing reason id line):");
        for (id, rule, reason) in &caught_reasons {
            println!("  [{id} | rule {rule}] {reason}");
        }
    }
    println!();

    // --- Set B: false-block rate, overall and per class ----------------
    let mut false_block_total = 0usize;
    let mut by_class: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // class -> (false_block, total)
    let mut false_block_examples: Vec<(String, String, String)> = Vec::new(); // (id, class, reason)

    for case in &should_pass {
        let entry = by_class.entry(case.class.clone()).or_insert((0, 0));
        entry.1 += 1;
        if let Some(reason) = respond::block_reason(Some(&rulebook_text), &case.reply) {
            false_block_total += 1;
            entry.0 += 1;
            false_block_examples.push((case.id.clone(), case.class.clone(), reason));
        }
    }
    let n_b = should_pass.len().max(1);
    println!(
        "false-block rate (overall) : {false_block_total}/{} ({:.1}%)",
        should_pass.len(),
        100.0 * false_block_total as f64 / n_b as f64
    );
    println!("false-block rate by class:");
    for (class, (fb, total)) in &by_class {
        println!("  {class:<24} {fb}/{total} ({:.1}%)", 100.0 * *fb as f64 / *total as f64);
    }
    if !false_block_examples.is_empty() {
        println!();
        println!("false blocks (full text + firing reason):");
        for (id, class, reason) in &false_block_examples {
            let text = should_pass.iter().find(|c| &c.id == id).map(|c| c.reply.as_str()).unwrap_or("");
            println!("  --- [{id}] class={class} ---");
            println!("  reply  : {text}");
            println!("  reason : {reason}");
            println!();
        }
    }
}
