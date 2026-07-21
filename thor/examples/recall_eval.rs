//! Honest production measurement of `recall::recall_fused` (feature `semantic`).
//!
//! Runs the REAL recall path - not a python re-implementation - over the private
//! 52-query battery and reports recall@3/@5 per category, fused vs bm25. The bm25
//! baseline goes through the SAME `recall_fused` with an all-zero query vector, so
//! the cosine term vanishes and the order is pure bm25 (identical candidate pool,
//! head projection and dedup) - isolating exactly what fusion adds. Matching is at
//! the ENTITY level (the courier injects an entity's current head, so a hit on the
//! gold's entity IS a correct recall, even if the gold seq was later revised).
//!
//! Run: cargo run --release --features semantic --example recall_eval
//!
//! The battery is swappable, because a 52-question corpus can no longer separate
//! these arms: measured 2026-07-21, bm25 and every fused lambda agree on 46-52 of
//! the 52 items, so a paired test returns p >= 0.5 for every comparison. A bigger
//! corpus is the only way back to a decidable measurement.
//!
//!   --queries <p>        query list      (default eval/percategory_queries.json)
//!   --golds <p>          id -> gold seq  (default eval/golds52.json)
//!   --golds-content <p>  frozen gold text(default eval/golds_content52.json)
//!   --out <p>            per-item outcomes for every arm, as JSON. Totals cannot
//!                        support a claim on a battery this size; paired per-item
//!                        results can, via McNemar on the discordant items.
//!
//! Items that cannot be scored (no id, no gold seq, gold seq absent from this
//! store) are counted and reported on stderr rather than silently dropped.

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use thor::embed::{Embedder, DIM};
use thor::event_store::EventStore;
use thor::recall::recall_fused_scoped;
use thor::vectors::{default_vectors_path, VectorStore};

fn local(sub: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"));
    p.push("thor");
    for s in sub {
        p.push(s);
    }
    p
}

/// Key terms of a gold text: lowercase alphanumeric tokens >= 4 chars, deduped
/// (same recipe as drift_eval's). Content match = a hit body carrying >= half
/// of them - survives revision/distillation of the gold, unlike the entity id.
fn key_terms(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 4)
        .map(|t| t.to_lowercase())
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

fn main() -> anyhow::Result<()> {
    // The battery is swappable so a bigger corpus can be measured without editing
    // this file. Defaults are the historical 52-question battery, so a bare run
    // still reproduces every number ever published from it.
    let (mut q_path, mut g_path, mut gc_path, mut out_path) = (None, None, None, None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--queries" => q_path = args.next().map(PathBuf::from),
            "--golds" => g_path = args.next().map(PathBuf::from),
            "--golds-content" => gc_path = args.next().map(PathBuf::from),
            // Per-item outcomes for every arm. Without this the printed table only
            // supports "arm A scored more than arm B", which on a battery this size
            // is well inside the noise; paired per-item results are what a paired
            // test (McNemar) needs to say whether a difference is real at all.
            "--out" => out_path = args.next().map(PathBuf::from),
            other => anyhow::bail!(
                "unknown argument {other}\nusage: recall_eval [--queries <p>] [--golds <p>] \
                 [--golds-content <p>] [--out <p>]"
            ),
        }
    }
    let q_path = q_path.unwrap_or_else(|| local(&["eval", "percategory_queries.json"]));
    let g_path = g_path.unwrap_or_else(|| local(&["eval", "golds52.json"]));
    let gc_path = gc_path.unwrap_or_else(|| local(&["eval", "golds_content52.json"]));

    let queries: Vec<Value> = serde_json::from_reader(std::fs::File::open(&q_path)?)?;
    let golds_raw: HashMap<String, Value> =
        serde_json::from_reader(std::fs::File::open(&g_path)?)?;
    let golds: HashMap<String, i64> =
        golds_raw.into_iter().filter_map(|(k, v)| v.as_i64().map(|s| (k, s))).collect();
    // Content-addressed golds (optional sidecar): id -> frozen gold TEXT. When
    // present, a hit counts if its entity matches OR its body carries >= half
    // of the gold's key terms - the metabolism-proof measure; ids stay the
    // continuity measure.
    let gold_terms: HashMap<String, Vec<String>> = match std::fs::File::open(&gc_path) {
        Ok(f) => {
            let raw: HashMap<String, Value> = serde_json::from_reader(f)?;
            raw.into_iter()
                .filter_map(|(k, v)| {
                    v.get("gold_text").and_then(|t| t.as_str()).map(|t| (k, key_terms(t)))
                })
                .collect()
        }
        Err(_) => HashMap::new(),
    };
    eprintln!(
        "battery: {} queries from {}, {} golds, {} frozen gold texts",
        queries.len(),
        q_path.display(),
        golds.len(),
        gold_terms.len()
    );

    let db = local(&["thor.db"]);
    let store = EventStore::new(&db)?;
    let events = store.get_all_events()?;
    let seq_to_entity: HashMap<i64, String> =
        events.iter().map(|e| (e.seq, e.entity_id.clone())).collect();
    let vecs = VectorStore::open(&default_vectors_path(&db))?;
    let symbols = thor::symbols::SymbolStore::open(&thor::symbols::default_symbols_path(&db)).ok();
    let mut emb = Embedder::load_default()?;
    let zero = vec![0.0f32; DIM];

    let cats = [
        "code-structure",
        "code-behavior",
        "doc-reference",
        "config-how",
        "gotcha",
        "decision",
    ];
    let lambdas = [0.5f64, 1.0, 1.5, 2.0, 3.0];
    // per category: [hit@3, hit@5, n]
    let mut bm: HashMap<String, [i32; 3]> = HashMap::new();
    let mut fu: Vec<HashMap<String, [i32; 3]>> = lambdas.iter().map(|_| HashMap::new()).collect();
    // Per-item outcomes, and a reason count for every item that never got scored.
    // A battery that silently drops items reads as "we measured everything".
    let mut per_item: Vec<Value> = Vec::new();
    let (mut skip_id, mut skip_gold, mut skip_entity) = (0usize, 0usize, 0usize);

    for q in &queries {
        let id = match q.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                skip_id += 1;
                continue;
            }
        };
        let query = q.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let category = q.get("category").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let gseq = match golds.get(&id) {
            Some(s) => *s,
            None => {
                skip_gold += 1;
                continue;
            }
        };
        let gent = match seq_to_entity.get(&gseq) {
            Some(e) => e.clone(),
            None => {
                skip_entity += 1;
                continue;
            }
        };

        let qvec = emb.embed_one(query)?;
        let terms = gold_terms.get(&id).cloned().unwrap_or_default();
        let count = move |hits: &[thor::recall::RecallHit], k: usize| -> bool {
            hits.iter().take(k).any(|h| {
                if h.entity_id == gent {
                    return true;
                }
                if terms.is_empty() {
                    return false;
                }
                let lower = h.body.to_lowercase();
                let got = terms.iter().filter(|t| lower.contains(t.as_str())).count();
                got as f64 / terms.len() as f64 >= 0.5
            })
        };

        // bm25 baseline: a zero query vector -> cosine 0 -> pure bm25 order.
        let base = recall_fused_scoped(&store, query, &zero, &vecs, 5, 1.0,
            &thor::recall::RecallScope::everything(), true, symbols.as_ref())?;
        let (b3, b5) = (count(&base, 3), count(&base, 5));
        let bc = bm.entry(category.clone()).or_insert([0, 0, 0]);
        bc[2] += 1;
        if b3 {
            bc[0] += 1;
        }
        if b5 {
            bc[1] += 1;
        }
        let mut item_arms = serde_json::Map::new();
        item_arms.insert("bm25".into(), serde_json::json!({ "at3": b3, "at5": b5 }));

        // fused, one accumulator per lambda.
        for (i, &lam) in lambdas.iter().enumerate() {
            let hits = recall_fused_scoped(&store, query, &qvec, &vecs, 5, lam,
                &thor::recall::RecallScope::everything(), true, symbols.as_ref())?;
            let (h3, h5) = (count(&hits, 3), count(&hits, 5));
            item_arms
                .insert(format!("fused_L{lam}"), serde_json::json!({ "at3": h3, "at5": h5 }));
            let fc = fu[i].entry(category.clone()).or_insert([0, 0, 0]);
            fc[2] += 1;
            if h3 {
                fc[0] += 1;
            }
            if h5 {
                fc[1] += 1;
            }
        }
        per_item.push(serde_json::json!({
            "id": id,
            "category": category,
            "gold_seq": gseq,
            "arms": Value::Object(item_arms),
        }));
    }

    if skip_id + skip_gold + skip_entity > 0 {
        eprintln!(
            "skipped {} item(s): {} without an id, {} without a gold seq, {} whose gold seq is \
             not in this store",
            skip_id + skip_gold + skip_entity,
            skip_id,
            skip_gold,
            skip_entity
        );
    }
    if let Some(p) = &out_path {
        std::fs::write(p, serde_json::to_string_pretty(&per_item)?)?;
        eprintln!("wrote {} per-item outcomes to {}", per_item.len(), p.display());
    }

    println!("REAL recall.rs recall_fused - normalized-fusion lambda sweep (cells = recall@5 per category)");
    let header = format!(
        "{:11} | {} | @3    | @5",
        "strategy",
        cats.iter().map(|c| format!("{:11}", &c[..c.len().min(11)])).collect::<Vec<_>>().join(" | ")
    );
    println!("{}", header);
    println!("{}", "-".repeat(header.len()));

    let mut rows: Vec<(String, &HashMap<String, [i32; 3]>)> = vec![("bm25".to_string(), &bm)];
    for (i, &lam) in lambdas.iter().enumerate() {
        rows.push((format!("fused_L{}", lam), &fu[i]));
    }
    for (name, m) in rows {
        let (mut t3, mut t5, mut tn) = (0, 0, 0);
        let cells: Vec<String> = cats
            .iter()
            .map(|c| {
                let a = m.get(*c).copied().unwrap_or([0, 0, 0]);
                t3 += a[0];
                t5 += a[1];
                tn += a[2];
                format!("{:11}", format!("{}/{}", a[1], a[2]))
            })
            .collect();
        println!(
            "{:11} | {} | {}/{} {:2.0}% | {}/{} {:2.0}%",
            name,
            cells.join(" | "),
            t3,
            tn,
            100.0 * t3 as f64 / tn as f64,
            t5,
            tn,
            100.0 * t5 as f64 / tn as f64
        );
    }
    Ok(())
}
