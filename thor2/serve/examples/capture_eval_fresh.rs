//! Fresh-holdout run of the same acceptance measurement as `capture_eval.rs`
//! (SPEC-ENFORCEMENT.md section 4), against a NEW, previously-unseen prompt
//! pair written blind (before the rulebook was read) specifically to give an
//! uncontaminated number after the post-calibration consistency fix:
//! `serve/eval/capture-eval-fresh-decisions.json` (prompts that DO state a
//! durable decision) and `serve/eval/capture-eval-fresh-nearmiss.json`
//! (prompts that must NOT be flagged at tier=block, labelled by near-miss
//! class).
//!
//! This does not change what is measured versus capture_eval.rs - same
//! matcher (`serve::capture::decide_capture`), same shipped rulebook, same
//! catch-rate / false-block-rate / per-class computations. The only
//! difference is the input file SHAPE: the fresh sets are arrays of
//! `{"id", "prompt", "class"}` objects (set A: class is always "decision")
//! rather than the older plain-string / {class,text} shapes, so the loaders
//! below read "prompt" instead of "text" and ignore "id".
//!
//! Pure text matching only - no store is opened at any point, so there is
//! nothing here that could touch the live store or a copy of it.
//!
//! Run (from the `thor2` workspace root):
//!   cargo run --release -p serve --example capture_eval_fresh
//!
//! All three paths can be overridden via env vars (CAPTURE_EVAL_FRESH_DECISIONS
//! / CAPTURE_EVAL_FRESH_NEAR_MISS / CAPTURE_EVAL_FRESH_RULEBOOK) for pointing
//! this at a different set without editing this file.

use serde_json::Value;
use serve::capture;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn rulebook_path() -> PathBuf {
    std::env::var("CAPTURE_EVAL_FRESH_RULEBOOK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../guard-capture-rulebook.example.json")))
}

fn decisions_path() -> PathBuf {
    std::env::var("CAPTURE_EVAL_FRESH_DECISIONS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/eval/capture-eval-fresh-decisions.json")))
}

fn near_miss_path() -> PathBuf {
    std::env::var("CAPTURE_EVAL_FRESH_NEAR_MISS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/eval/capture-eval-fresh-nearmiss.json")))
}

/// Reads an array of `{"id", "prompt", "class"}` objects and returns just the
/// prompt text (set A: every entry's class is "decision", so it is not
/// tracked separately here - mirrors capture_eval.rs's plain-string loader).
fn load_prompts(path: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("could not parse {}: {e}", path.display()));
    v.as_array()
        .unwrap_or_else(|| panic!("{} is not a JSON array", path.display()))
        .iter()
        .map(|x| {
            x.get("prompt")
                .and_then(|p| p.as_str())
                .unwrap_or_else(|| panic!("entry missing string 'prompt' in {}", path.display()))
                .to_string()
        })
        .collect()
}

struct NearMiss {
    class: String,
    text: String,
}

fn load_near_miss(path: &std::path::Path) -> Vec<NearMiss> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("could not parse {}: {e}", path.display()));
    v.as_array()
        .unwrap_or_else(|| panic!("{} is not a JSON array", path.display()))
        .iter()
        .map(|x| NearMiss {
            class: x.get("class").and_then(|c| c.as_str()).unwrap_or("(no class)").to_string(),
            text: x.get("prompt").and_then(|c| c.as_str()).unwrap_or("").to_string(),
        })
        .collect()
}

fn main() {
    let rulebook_path = rulebook_path();
    let rulebook_text = std::fs::read_to_string(&rulebook_path)
        .unwrap_or_else(|e| panic!("could not read rulebook at {}: {e}", rulebook_path.display()));
    let rule_count = capture::parse_rulebook(&rulebook_text).len();

    let decisions = load_prompts(&decisions_path());
    let near_miss = load_near_miss(&near_miss_path());

    println!("Capture guard FRESH HOLD-OUT measurement (SPEC-ENFORCEMENT.md section 4)");
    println!("rulebook   : {} ({rule_count} rule(s))", rulebook_path.display());
    println!("decisions  : {} (n={})", decisions_path().display(), decisions.len());
    println!("near-miss  : {} (n={})", near_miss_path().display(), near_miss.len());
    println!();

    // --- catch rate --------------------------------------------------
    let mut caught_any_tier = 0usize;
    let mut caught_block_tier = 0usize;
    let mut missed: Vec<&str> = Vec::new();
    for prompt in &decisions {
        match capture::decide_capture(Some(&rulebook_text), prompt) {
            Some(d) => {
                caught_any_tier += 1;
                if d.tier == capture::Tier::Block {
                    caught_block_tier += 1;
                }
            }
            None => missed.push(prompt),
        }
    }
    let n_dec = decisions.len().max(1);
    println!(
        "catch rate (any tier)   : {caught_any_tier}/{} ({:.1}%)",
        decisions.len(),
        100.0 * caught_any_tier as f64 / n_dec as f64
    );
    println!(
        "catch rate (block tier) : {caught_block_tier}/{} ({:.1}%)",
        decisions.len(),
        100.0 * caught_block_tier as f64 / n_dec as f64
    );
    if !missed.is_empty() {
        println!("  missed (not flagged at all):");
        for m in &missed {
            println!("    - {m}");
        }
    }
    println!();

    // --- false-block rate (overall + per near-miss class) ------------
    let mut false_block_total = 0usize;
    let mut false_flag_any_tier_total = 0usize;
    let mut by_class: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // class -> (false_block, total)
    let mut false_block_examples: Vec<(String, String, String)> = Vec::new();
    for nm in &near_miss {
        let entry = by_class.entry(nm.class.clone()).or_insert((0, 0));
        entry.1 += 1;
        match capture::decide_capture(Some(&rulebook_text), &nm.text) {
            Some(d) if d.tier == capture::Tier::Block => {
                false_block_total += 1;
                false_flag_any_tier_total += 1;
                entry.0 += 1;
                false_block_examples.push((nm.class.clone(), nm.text.clone(), d.rule_id.clone()));
            }
            Some(_) => {
                // warn tier: noise, not a turn-ending false BLOCK, but still
                // worth counting separately - see the "any tier" line below.
                false_flag_any_tier_total += 1;
            }
            None => {}
        }
    }
    let n_nm = near_miss.len().max(1);
    println!(
        "false-block rate (tier=block) : {false_block_total}/{} ({:.1}%)  <- the operationally meaningful number: this is what would have ended a turn",
        near_miss.len(),
        100.0 * false_block_total as f64 / n_nm as f64
    );
    println!(
        "false-flag rate (any tier)    : {false_flag_any_tier_total}/{} ({:.1}%)  <- includes warn-tier noise, never a block",
        near_miss.len(),
        100.0 * false_flag_any_tier_total as f64 / n_nm as f64
    );
    println!();
    println!("false-block rate by near-miss class:");
    for (class, (fb, total)) in &by_class {
        println!("  {class:<24} {fb}/{total} ({:.1}%)", 100.0 * *fb as f64 / *total as f64);
    }
    if !false_block_examples.is_empty() {
        println!();
        println!("false-blocked near-miss prompts:");
        for (class, text, rule_id) in &false_block_examples {
            println!("  [{class}] (rule: {rule_id}) {text}");
        }
    }
}
