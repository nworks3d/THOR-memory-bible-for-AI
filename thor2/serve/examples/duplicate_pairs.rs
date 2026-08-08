//! Which pairs already in the store would the write gate have refused as the
//! same fact told twice?
//!
//! WHY THIS EXISTS. `model::store::declare` refuses a near-duplicate on the
//! way IN, and has done so for a while. Nothing ever looked at the pairs that
//! were already there when that rule arrived, or that entered before it. They
//! cost twice: two items say one thing, and a block shows at most a handful,
//! so a duplicate takes a slot a different fact could have had. Measured
//! 2026-08-08: 66 of 225 anchors have more applying than the block can show,
//! and 257 items are eligible at their own anchor and never once shown.
//!
//! ONE DEFINITION, NOT A SECOND ONE. It calls `model::store`'s own
//! `normalize_for_comparison`, `word_set` and `jaccard_similarity`, at the
//! same `NEAR_DUPLICATE_JACCARD_THRESHOLD` the gate uses, and applies the
//! same two exemptions: a different kind is never a duplicate, and neither is
//! a different project (two repositories can genuinely carry the same
//! constraint, and each needs its own).
//!
//! TWO BANDS, deliberately separated. At or above the threshold is what the
//! gate would have refused outright - those are the merge candidates. Between
//! `LOOK_BAND` and the threshold is NOT a duplicate by this store's own rule;
//! it is only close enough that a person might see something the word overlap
//! cannot. Never merge from that band without reading both.
//!
//! Read-only: `open_existing`, SELECT-only reads, no writes anywhere.
//!
//!   DUP_DB=<copy> cargo run --release -p serve --example duplicate_pairs

use model::store::{jaccard_similarity, normalize_for_comparison, word_set, NEAR_DUPLICATE_JACCARD_THRESHOLD};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// The floor of the "worth a human look" band. Not a duplicate rule: a
/// second, weaker question asked of the same numbers.
const LOOK_BAND: f64 = 0.55;

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("DUP_DB").expect("set DUP_DB - point it at a COPY"));
    let store = EventStore::open_existing(&db)?;

    let items: Vec<_> = serve::live::live_items(&store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire())
        .collect();
    let prepared: Vec<(String, String, String, String)> = items
        .iter()
        .map(|li| {
            (
                li.id.clone(),
                li.item.project.clone().unwrap_or_default(),
                format!("{:?}", li.item.kind),
                normalize_for_comparison(&li.item.text),
            )
        })
        .collect();

    println!("comparing {} fireable item(s) pairwise\n", prepared.len());

    let mut refusable = Vec::new();
    let mut worth_a_look = Vec::new();
    for i in 0..prepared.len() {
        let a_words = word_set(&prepared[i].3);
        for j in (i + 1)..prepared.len() {
            // The gate's own two exemptions, not a re-invention of them.
            if prepared[i].2 != prepared[j].2 || prepared[i].1 != prepared[j].1 {
                continue;
            }
            let score = jaccard_similarity(&a_words, &word_set(&prepared[j].3));
            if score < LOOK_BAND {
                continue;
            }
            let row = serde_json::json!({
                "a": prepared[i].0, "b": prepared[j].0,
                "project": prepared[i].1, "kind": prepared[i].2,
                "score": (score * 100.0).round() / 100.0,
                "a_text": items[i].item.text, "b_text": items[j].item.text,
            });
            if score >= NEAR_DUPLICATE_JACCARD_THRESHOLD {
                refusable.push(row);
            } else {
                worth_a_look.push(row);
            }
        }
    }

    println!("the gate would have REFUSED these as the same fact twice : {}", refusable.len());
    println!("close enough that a person should read both              : {}", worth_a_look.len());

    let mut per_project: BTreeMap<String, usize> = Default::default();
    for r in &refusable {
        *per_project.entry(r["project"].as_str().unwrap_or("(global)").to_string()).or_default() += 1;
    }
    if !per_project.is_empty() {
        println!("\nrefusable pairs per project:");
        for (p, n) in &per_project {
            println!("  {:<24} {n}", if p.is_empty() { "(global)" } else { p });
        }
    }

    std::fs::write("duplicate-refusable.json", serde_json::to_string_pretty(&refusable)?)?;
    std::fs::write("duplicate-worth-a-look.json", serde_json::to_string_pretty(&worth_a_look)?)?;
    println!("\nwrote duplicate-refusable.json and duplicate-worth-a-look.json");
    Ok(())
}
