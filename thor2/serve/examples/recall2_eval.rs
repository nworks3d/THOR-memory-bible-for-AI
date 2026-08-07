//! Layer-2 recall measurement for THOR 2.0's surface-4 lookup (feature `semantic`).
//!
//! Reuses the private 460-query battery + its content gold texts, but scores
//! against the 2.0 FACT store and 2.0's OWN ranking. Two honest consequences of
//! the rebuild shape the method:
//!
//! 1. 2.0's lookup has NO bm25 side - it is a plain substring match plus a pure
//!    cosine sidecar. So there is no fusion lambda to tune (unlike 1.0). The one
//!    tunable knob is the silence-gate FLOOR (`MIN_SIMILARITY`, live default
//!    0.45). This harness sweeps it.
//! 2. The 460 battery was built for 1.0, which also held code/doc CHUNKS that 2.0
//!    deliberately offloads to the code index (the fact store has 0 chunks). A
//!    query whose gold is such a chunk is unanswerable by 2.0's *fact* lookup by
//!    design. So a query counts ONLY if its gold fact actually EXISTS in the 2.0
//!    store - a content-match (>= half of the gold's key terms) against the live
//!    corpus. The surviving count per category is reported: it is itself part of
//!    the answer ("how much of the old battery even applies to 2.0's fact door").
//!
//! Matching is at the CONTENT level (the gold's key terms), never the 1.0 seq/id,
//! because the migration re-typed every fact into a new id space but preserved the
//! text. Read-only; point it at a COPY of the store, never the live one.
//!
//! Run: RECALL2_DB=<copy of thor.db> \
//!      cargo run --release --features semantic --example recall2_eval

use model::item::Kind;
use serde_json::Value;
use serve::embed::Embedder;
use serve::live::live_items;
use serve::semantic_paths::{default_model_dir, default_vectors_path};
use serve::vectors::VectorStore;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// Compared against an `expires` value; an item past it is not served by lookup.
const TODAY: &str = "2026-08-03";
/// The live `MAX_SEMANTIC_EXTRA` cap in lookup.rs - matched here for faithfulness.
const CAP: usize = 10;
/// The floors swept. 0.45 is the live default (`MIN_SIMILARITY`).
const FLOORS: [f32; 6] = [0.30, 0.35, 0.40, 0.45, 0.50, 0.55];

fn key_terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 4)
        .map(|t| t.to_lowercase())
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

fn content_match(item_lower: &str, gold_terms: &[String]) -> bool {
    if gold_terms.is_empty() {
        return false;
    }
    let got = gold_terms.iter().filter(|t| item_lower.contains(t.as_str())).count();
    got as f64 / gold_terms.len() as f64 >= 0.5
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len().min(b.len()) {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn is_expired(expires: Option<&str>) -> bool {
    matches!(expires, Some(d) if d.len() == 10 && d < TODAY)
}

fn eval_dir() -> PathBuf {
    std::env::var("RECALL2_EVAL").map(PathBuf::from).unwrap_or_else(|_| {
        let mut p = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"));
        p.push("thor");
        p.push("eval");
        p
    })
}

/// One item's precomputed scoring inputs.
struct Item {
    text: String,
    lower: String,
    vec: Option<Vec<f32>>,
}

/// Rank the corpus for one query exactly the way `search_best_effort_cached`
/// does: literal substring hits first (ordered by cosine - the recall@1 fix),
/// then the semantic-only extras above `floor`, capped. Returns item indices,
/// best first.
fn rank(items: &[Item], qlower: &str, qvec: &[f32], floor: f32) -> Vec<usize> {
    let mut literal: Vec<usize> = (0..items.len()).filter(|&i| items[i].lower.contains(qlower)).collect();
    let lit_set: HashSet<usize> = literal.iter().copied().collect();
    let sc = |i: usize| items[i].vec.as_deref().map(|v| cosine(qvec, v)).unwrap_or(f32::MIN);
    literal.sort_by(|&a, &b| sc(b).total_cmp(&sc(a)).then(a.cmp(&b)));

    let mut sem: Vec<(f32, usize)> = (0..items.len())
        .filter(|i| !lit_set.contains(i))
        .filter_map(|i| items[i].vec.as_deref().map(|v| (cosine(qvec, v), i)))
        .filter(|(s, _)| *s >= floor)
        .collect();
    sem.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    sem.truncate(CAP);

    literal.into_iter().chain(sem.into_iter().map(|(_, i)| i)).collect()
}

fn hit_at(ranked: &[usize], k: usize, gold: &HashSet<usize>) -> bool {
    ranked.iter().take(k).any(|i| gold.contains(i))
}

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("RECALL2_DB").expect("set RECALL2_DB to a COPY of thor.db"));
    let vpath = default_vectors_path(&db);
    let model_dir = default_model_dir().expect("no per-user model dir resolved");
    let store = EventStore::new(&db)?;

    // The live fact corpus surface-4 can serve: non-Lookup, non-expired.
    let raw: Vec<(String, String)> = live_items(&store)
        .into_iter()
        .filter(|li| li.item.kind != Kind::Lookup && !is_expired(li.item.expires.as_deref()))
        .map(|li| (li.id, li.item.text))
        .collect();
    let ids: Vec<String> = raw.iter().map(|(id, _)| id.clone()).collect();
    let vs = VectorStore::open(&vpath)?;
    let vectors = vs.get_many(&ids)?;
    let items: Vec<Item> = raw
        .iter()
        .map(|(id, text)| Item { text: text.clone(), lower: text.to_lowercase(), vec: vectors.get(id).cloned() })
        .collect();
    let n_vec = items.iter().filter(|i| i.vec.is_some()).count();
    eprintln!(
        "live fact items: {}   with a current vector: {} ({:.0}% coverage)   sidecar model_id: {:?}",
        items.len(),
        n_vec,
        100.0 * n_vec as f64 / items.len() as f64,
        vs.model_id()
    );

    let mut emb = Embedder::load(&model_dir)?;

    let queries: Vec<Value> =
        serde_json::from_reader(std::fs::File::open(eval_dir().join("percategory_queries460.json"))?)?;
    let golds: HashMap<String, Value> =
        serde_json::from_reader(std::fs::File::open(eval_dir().join("golds_content460.json"))?)?;

    // ---- Build the VALID 2.0 battery: only queries whose gold fact exists here.
    struct Q {
        category: String,
        query: String,
        goldtext: String,
        qlower: String,
        qvec: Vec<f32>,
        gold: HashSet<usize>,
    }
    let mut battery: Vec<Q> = Vec::new();
    let mut excluded_by_cat: HashMap<String, usize> = HashMap::new();
    let mut kept_by_cat: HashMap<String, usize> = HashMap::new();

    for q in &queries {
        let id = q.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let query = q.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let category = q.get("category").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        let gold_text = golds
            .get(id)
            .and_then(|g| g.get("gold_text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let terms = key_terms(gold_text);
        let gold: HashSet<usize> =
            (0..items.len()).filter(|&i| content_match(&items[i].lower, &terms)).collect();
        if gold.is_empty() {
            *excluded_by_cat.entry(category).or_default() += 1;
            continue;
        }
        let qvec = emb.embed_one(query)?;
        *kept_by_cat.entry(category.clone()).or_default() += 1;
        battery.push(Q {
            category,
            query: query.to_string(),
            goldtext: gold_text.chars().take(140).collect(),
            qlower: query.to_lowercase(),
            qvec,
            gold,
        });
    }

    let n = battery.len();
    println!("\n=== VALID 2.0 battery: {} of {} queries have their gold fact in the store ===", n, queries.len());
    let mut cats: Vec<&String> = kept_by_cat.keys().chain(excluded_by_cat.keys()).collect();
    cats.sort();
    cats.dedup();
    for c in cats {
        println!(
            "  {:16} kept {:3}   excluded {:3} (gold is a code/doc chunk or not migrated)",
            c,
            kept_by_cat.get(c).copied().unwrap_or(0),
            excluded_by_cat.get(c).copied().unwrap_or(0)
        );
    }

    // ---- Floor sweep: recall@1/@3/@5 and mean results returned.
    println!("\n=== silence-gate floor sweep (n={}) ===", n);
    println!("floor  | recall@1 | recall@3 | recall@5 | mean #results");
    println!("-------|----------|----------|----------|--------------");
    for &floor in &FLOORS {
        let (mut r1, mut r3, mut r5, mut total_results) = (0usize, 0usize, 0usize, 0usize);
        for q in &battery {
            let ranked = rank(&items, &q.qlower, &q.qvec, floor);
            total_results += ranked.len();
            if hit_at(&ranked, 1, &q.gold) {
                r1 += 1;
            }
            if hit_at(&ranked, 3, &q.gold) {
                r3 += 1;
            }
            if hit_at(&ranked, 5, &q.gold) {
                r5 += 1;
            }
        }
        let pct = |x: usize| 100.0 * x as f64 / n as f64;
        let live = if (floor - 0.45).abs() < 1e-6 { "  <- live" } else { "" };
        println!(
            "{:.2}   |  {:3}/{:<3} |  {:3}/{:<3} |  {:3}/{:<3} |   {:5.1}     {}",
            floor,
            r1,
            n,
            r3,
            n,
            r5,
            n,
            total_results as f64 / n as f64,
            live
        );
        let _ = (pct(r1), pct(r5));
    }

    // ---- Per-category recall@5 at the live floor 0.45.
    println!("\n=== per-category recall@5 at the live floor 0.45 ===");
    let mut by_cat: HashMap<String, (usize, usize)> = HashMap::new();
    for q in &battery {
        let ranked = rank(&items, &q.qlower, &q.qvec, 0.45);
        let e = by_cat.entry(q.category.clone()).or_insert((0, 0));
        e.1 += 1;
        if hit_at(&ranked, 5, &q.gold) {
            e.0 += 1;
        }
    }
    let mut ck: Vec<&String> = by_cat.keys().collect();
    ck.sort();
    for c in ck {
        let (h, t) = by_cat[c];
        println!("  {:16} {:3}/{:<3}  {:3.0}%", c, h, t, 100.0 * h as f64 / t as f64);
    }

    // ---- Diagnostic: are the recall@5 misses genuinely wrong, or is the
    // 1.0-gold-text proxy simply not recognizing the (distilled) 2.0 answer?
    if std::env::var("RECALL2_DIAG").is_ok() {
        println!("\n=== recall@5 MISSES at floor 0.45 (query | 1.0 gold | top-3 retrieved 2.0 items) ===");
        let mut shown = 0;
        for q in &battery {
            let ranked = rank(&items, &q.qlower, &q.qvec, 0.45);
            if hit_at(&ranked, 5, &q.gold) {
                continue;
            }
            shown += 1;
            if shown > 15 {
                break;
            }
            println!("\n[{}] Q: {}", q.category, q.query);
            println!("    GOLD: {}", q.goldtext);
            for (r, &i) in ranked.iter().take(3).enumerate() {
                let t: String = items[i].text.chars().take(110).collect();
                println!("    top{}: {}", r + 1, t);
            }
        }
    }

    // ---- Noise set: results returned at each floor (precision / silence gate).
    // A good gate returns FEW or ZERO for a query with no real answer.
    if let Ok(f) = std::fs::File::open(eval_dir().join("noise").join("noise_queries.json")) {
        let noise: Vec<Value> = serde_json::from_reader(f)?;
        println!("\n=== noise set ({} queries): mean #results returned per floor ===", noise.len());
        print!("floor  ");
        for &fl in &FLOORS {
            print!("| {:.2} ", fl);
        }
        println!();
        let mut nqs: Vec<(String, Vec<f32>)> = Vec::new();
        for q in &noise {
            if let Some(query) = q.get("query").and_then(|v| v.as_str()) {
                let qv = emb.embed_one(query)?;
                nqs.push((query.to_lowercase(), qv));
            }
        }
        print!("result ");
        for &floor in &FLOORS {
            let mut total = 0usize;
            for (ql, qv) in &nqs {
                total += rank(&items, ql, qv, floor).len();
            }
            print!("| {:.1} ", total as f64 / nqs.len().max(1) as f64);
        }
        println!("\n(lower is better - these queries have no fact answer; every returned item is padding)");
    }

    Ok(())
}
