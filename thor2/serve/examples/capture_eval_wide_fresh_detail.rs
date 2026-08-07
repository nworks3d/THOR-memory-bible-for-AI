//! Per-item detail for the WIDE trigger's any-tier fire on the FROZEN fresh
//! near-miss hold-out (`serve/eval/capture-eval-fresh-nearmiss.json`, n=70).
//!
//! WIDE-TRIGGER.md Part 2 already reports the aggregate number for this run
//! (31/70 = 44.3%, any tier) but does not print which ids fired - it reused
//! `capture_eval_fresh.rs` unmodified, which only lists FALSE-BLOCK examples
//! (tier=block) since that is the number the shipped rulebook cares about.
//! The wide trigger's rules are all tier="warn" (see
//! `guard-capture-trigger-wide.example.json` and WIDE-TRIGGER.md Part 1), so
//! that block-tier list is empty for it and the per-item any-tier detail was
//! never recorded anywhere on disk.
//!
//! This file adds nothing new to the matching logic - same real matcher
//! (`serve::capture::decide_capture`), same frozen fresh near-miss file, same
//! wide rulebook - only the reporting is new: it prints, for every item, its
//! id/class/any-tier-fired flag, so a downstream measurement (the wide
//! trigger's false-trigger population, WIDE-TRIGGER-POLLUTION.md) can name
//! exactly which ids to run through forced reconsideration.
//!
//! Reads ONLY the frozen fresh near-miss file; never writes to it or to any
//! other eval set/rulebook/source file. No store is opened.
//!
//! Run (from the thor2 workspace root):
//!   CAPTURE_EVAL_FRESH_RULEBOOK=../guard-capture-trigger-wide.example.json \
//!     cargo run --release -p serve --example capture_eval_wide_fresh_detail

use serde_json::Value;
use serve::capture;
use std::path::PathBuf;

fn rulebook_path() -> PathBuf {
    std::env::var("CAPTURE_EVAL_FRESH_RULEBOOK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../guard-capture-rulebook.example.json")))
}

fn near_miss_path() -> PathBuf {
    std::env::var("CAPTURE_EVAL_FRESH_NEAR_MISS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/eval/capture-eval-fresh-nearmiss.json")))
}

struct NearMiss {
    id: String,
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
            id: x.get("id").and_then(|c| c.as_str()).unwrap_or("(no id)").to_string(),
            class: x.get("class").and_then(|c| c.as_str()).unwrap_or("(no class)").to_string(),
            text: x.get("prompt").and_then(|c| c.as_str()).unwrap_or("").to_string(),
        })
        .collect()
}

fn main() {
    let rulebook_path = rulebook_path();
    let rulebook_text = std::fs::read_to_string(&rulebook_path)
        .unwrap_or_else(|e| panic!("could not read rulebook at {}: {e}", rulebook_path.display()));

    let near_miss = load_near_miss(&near_miss_path());

    println!("rulebook  : {}", rulebook_path.display());
    println!("near-miss : {} (n={})", near_miss_path().display(), near_miss.len());
    println!();

    let mut fired_total = 0usize;
    println!("id\tclass\tfired\ttier\trule_id");
    for nm in &near_miss {
        match capture::decide_capture(Some(&rulebook_text), &nm.text) {
            Some(d) => {
                fired_total += 1;
                println!("{}\t{}\tFIRED\t{:?}\t{}", nm.id, nm.class, d.tier, d.rule_id);
            }
            None => {
                println!("{}\t{}\tno\t-\t-", nm.id, nm.class);
            }
        }
    }
    println!();
    println!(
        "trigger rate on near-miss (any tier) : {fired_total}/{} ({:.1}%)",
        near_miss.len(),
        100.0 * fired_total as f64 / near_miss.len().max(1) as f64
    );
}
