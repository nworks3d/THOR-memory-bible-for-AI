//! Trigger-recall measurement for the WIDE capture-guard trigger experiment
//! (WIDE-TRIGGER.md). Same two tuning sets and same real matcher as
//! `capture_eval.rs` (`serve::capture::decide_capture` against a rulebook
//! file) - this file adds nothing new to the matching logic, only to the
//! reporting: `capture_eval.rs` already prints any-tier catch on the
//! decisions set and an AGGREGATE any-tier false-flag rate on the near-miss
//! set, but only breaks the near-miss side down by class at tier=block. For
//! designing a trigger that deliberately maximises the any-tier union (both
//! tiers "fire" - see `capture::Tier` and WIDE-TRIGGER.md part 1), the
//! any-tier breakdown BY CLASS is exactly what is needed to see which
//! near-miss class absorbs the added trigger rate, so it is added here
//! instead of reimplementing any matching.
//!
//! Pure text matching only, same as capture_eval.rs - no store is opened.
//!
//! Run (from the thor2 workspace root), pointing at a candidate rulebook:
//!   CAPTURE_EVAL_RULEBOOK=guard-capture-trigger-wide.example.json \
//!     cargo run -p serve --example capture_eval_wide
//!
//! Reads ONLY the tuning sets (capture-eval-decisions.json,
//! capture-eval-near-miss.json) by default - the fresh hold-out sets are
//! deliberately never touched by this file; capture_eval_fresh.rs is used,
//! unmodified, for the single hold-out run instead.

use serde_json::Value;
use serve::capture;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn rulebook_path() -> PathBuf {
    std::env::var("CAPTURE_EVAL_RULEBOOK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../guard-capture-rulebook.example.json")))
}

fn decisions_path() -> PathBuf {
    std::env::var("CAPTURE_EVAL_DECISIONS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/eval/capture-eval-decisions.json")))
}

fn near_miss_path() -> PathBuf {
    std::env::var("CAPTURE_EVAL_NEAR_MISS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/eval/capture-eval-near-miss.json")))
}

fn load_strings(path: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("could not parse {}: {e}", path.display()));
    v.as_array()
        .unwrap_or_else(|| panic!("{} is not a JSON array", path.display()))
        .iter()
        .map(|x| x.as_str().unwrap_or_else(|| panic!("non-string entry in {}", path.display())).to_string())
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
            text: x.get("text").and_then(|c| c.as_str()).unwrap_or("").to_string(),
        })
        .collect()
}

fn main() {
    let rulebook_path = rulebook_path();
    let rulebook_text = std::fs::read_to_string(&rulebook_path)
        .unwrap_or_else(|e| panic!("could not read rulebook at {}: {e}", rulebook_path.display()));
    let rule_count = capture::parse_rulebook(&rulebook_text).len();

    let decisions = load_strings(&decisions_path());
    let near_miss = load_near_miss(&near_miss_path());

    println!("WIDE TRIGGER measurement - tuning sets only (decisions.json / near-miss.json)");
    println!("rulebook   : {} ({rule_count} rule(s))", rulebook_path.display());
    println!("decisions  : {} (n={})", decisions_path().display(), decisions.len());
    println!("near-miss  : {} (n={})", near_miss_path().display(), near_miss.len());
    println!();

    // --- trigger recall on the decisions set: ANY tier counts as "fires" ---
    let mut caught = 0usize;
    let mut missed: Vec<&str> = Vec::new();
    for prompt in &decisions {
        if capture::decide_capture(Some(&rulebook_text), prompt).is_some() {
            caught += 1;
        } else {
            missed.push(prompt);
        }
    }
    let n_dec = decisions.len().max(1);
    println!(
        "trigger recall (any tier) : {caught}/{} ({:.1}%)",
        decisions.len(),
        100.0 * caught as f64 / n_dec as f64
    );
    if !missed.is_empty() {
        println!("  missed (trigger did not fire at all):");
        for m in &missed {
            println!("    - {m}");
        }
    }
    println!();

    // --- trigger rate on the near-miss set, overall AND by class ----------
    let mut fired_total = 0usize;
    let mut by_class: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // class -> (fired, total)
    let mut fired_examples: Vec<(String, String)> = Vec::new();
    for nm in &near_miss {
        let entry = by_class.entry(nm.class.clone()).or_insert((0, 0));
        entry.1 += 1;
        if capture::decide_capture(Some(&rulebook_text), &nm.text).is_some() {
            fired_total += 1;
            entry.0 += 1;
            fired_examples.push((nm.class.clone(), nm.text.clone()));
        }
    }
    let n_nm = near_miss.len().max(1);
    println!(
        "trigger rate on near-miss (any tier) : {fired_total}/{} ({:.1}%)",
        near_miss.len(),
        100.0 * fired_total as f64 / n_nm as f64
    );
    println!();
    println!("trigger rate by near-miss class:");
    for (class, (fired, total)) in &by_class {
        println!("  {class:<24} {fired}/{total} ({:.1}%)", 100.0 * *fired as f64 / *total as f64);
    }
    if !fired_examples.is_empty() {
        println!();
        println!("triggered near-miss prompts:");
        for (class, text) in &fired_examples {
            println!("  [{class}] {text}");
        }
    }
}
