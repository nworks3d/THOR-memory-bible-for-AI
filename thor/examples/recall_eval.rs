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

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use thor::embed::{Embedder, DIM};
use thor::event_store::EventStore;
use thor::recall::recall_fused;
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
    let queries: Vec<Value> =
        serde_json::from_reader(std::fs::File::open(local(&["eval", "percategory_queries.json"]))?)?;
    let golds_raw: HashMap<String, Value> =
        serde_json::from_reader(std::fs::File::open(local(&["eval", "golds52.json"]))?)?;
    let golds: HashMap<String, i64> =
        golds_raw.into_iter().filter_map(|(k, v)| v.as_i64().map(|s| (k, s))).collect();
    // Content-addressed golds (optional sidecar): id -> frozen gold TEXT. When
    // present, a hit counts if its entity matches OR its body carries >= half
    // of the gold's key terms - the metabolism-proof measure; ids stay the
    // continuity measure.
    let gold_terms: HashMap<String, Vec<String>> =
        match std::fs::File::open(local(&["eval", "golds_content52.json"])) {
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

    let db = local(&["thor.db"]);
    let store = EventStore::new(&db)?;
    let events = store.get_all_events()?;
    let seq_to_entity: HashMap<i64, String> =
        events.iter().map(|e| (e.seq, e.entity_id.clone())).collect();
    let vecs = VectorStore::open(&default_vectors_path(&db))?;
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

    for q in &queries {
        let id = match q.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => continue,
        };
        let query = q.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let category = q.get("category").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let gseq = match golds.get(&id) {
            Some(s) => *s,
            None => continue,
        };
        let gent = match seq_to_entity.get(&gseq) {
            Some(e) => e.clone(),
            None => continue,
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
        let base = recall_fused(&store, query, &zero, &vecs, 5, 1.0)?;
        let bc = bm.entry(category.clone()).or_insert([0, 0, 0]);
        bc[2] += 1;
        if count(&base, 3) {
            bc[0] += 1;
        }
        if count(&base, 5) {
            bc[1] += 1;
        }

        // fused, one accumulator per lambda.
        for (i, &lam) in lambdas.iter().enumerate() {
            let hits = recall_fused(&store, query, &qvec, &vecs, 5, lam)?;
            let fc = fu[i].entry(category.clone()).or_insert([0, 0, 0]);
            fc[2] += 1;
            if count(&hits, 3) {
                fc[0] += 1;
            }
            if count(&hits, 5) {
                fc[1] += 1;
            }
        }
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
