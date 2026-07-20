//! Speed prototype: baseline `recall_fused_scoped` (rebuilds the log-fold +
//! vector matrix every query) vs `recall_fused_scoped_cached` with a resident
//! `ResidentCache` built ONCE (matrix-patch + materialized heads = what a warm
//! daemon would hold). Proves the warm-daemon latency win, and checks the two
//! paths return identical hits.
//!
//! Run: cargo run --release --features semantic --example resident_bench

use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;
use thor::embed::Embedder;
use thor::event_store::EventStore;
use thor::recall::{recall_fused_scoped, recall_fused_scoped_cached, RecallScope, ResidentCache};
use thor::vectors::{default_vectors_path, VectorStore};

fn local(sub: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"));
    p.push("thor");
    for s in sub {
        p.push(s);
    }
    p
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn main() -> anyhow::Result<()> {
    let queries: Vec<Value> =
        serde_json::from_reader(std::fs::File::open(local(&["eval", "percategory_queries.json"]))?)?;
    let qs: Vec<String> = queries
        .iter()
        .filter_map(|q| q.get("query").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .take(20)
        .collect();

    let db = local(&["thor.db"]);
    let store = EventStore::new(&db)?;
    let vecs = VectorStore::open(&default_vectors_path(&db))?;
    let symbols = thor::symbols::SymbolStore::open(&thor::symbols::default_symbols_path(&db)).ok();
    let mut emb = Embedder::load_default()?;
    let qvecs: Vec<Vec<f32>> = qs.iter().map(|q| emb.embed_one(q)).collect::<Result<_, _>>()?;
    let scope = RecallScope::everything();
    let (lim, lam) = (5usize, 1.5f64);

    // warmup (page-ins, model already loaded)
    let _ = recall_fused_scoped(&store, &qs[0], &qvecs[0], &vecs, lim, lam, &scope, true, symbols.as_ref())?;

    // BASELINE: rebuild fold + vectors every query
    let mut base = Vec::new();
    for (q, qv) in qs.iter().zip(&qvecs) {
        let t = Instant::now();
        let _ = recall_fused_scoped(&store, q, qv, &vecs, lim, lam, &scope, true, symbols.as_ref())?;
        base.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // RESIDENT: build the cache ONCE, then reuse it
    let tb = Instant::now();
    let cache = ResidentCache::build(&store, &vecs)?;
    let build_ms = tb.elapsed().as_secs_f64() * 1000.0;
    let mut res = Vec::new();
    for (q, qv) in qs.iter().zip(&qvecs) {
        let t = Instant::now();
        let _ = recall_fused_scoped_cached(&store, q, qv, &vecs, lim, lam, &scope, true, symbols.as_ref(), Some(&cache))?;
        res.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // Correctness: baseline vs resident must return the SAME hit ids on every query
    let mut mismatches = 0;
    for (q, qv) in qs.iter().zip(&qvecs) {
        let a: Vec<String> = recall_fused_scoped(&store, q, qv, &vecs, lim, lam, &scope, true, symbols.as_ref())?
            .into_iter().map(|h| h.entity_id).collect();
        let b: Vec<String> = recall_fused_scoped_cached(&store, q, qv, &vecs, lim, lam, &scope, true, symbols.as_ref(), Some(&cache))?
            .into_iter().map(|h| h.entity_id).collect();
        if a != b {
            mismatches += 1;
        }
    }

    let bm = median(base.clone());
    let rm = median(res.clone());
    println!("resident-cache prototype ({} queries, store folded once)", qs.len());
    println!("  baseline  (rebuild/query)  median {:6.1} ms", bm);
    println!("  resident  (cache reused)   median {:6.1} ms   [one-time build {:.0} ms]", rm, build_ms);
    println!("  per-query saving           {:6.1} ms  ({:.0}% faster)", bm - rm, 100.0 * (bm - rm) / bm);
    println!("  correctness: {}/{} queries identical hits (mismatches {})", qs.len() - mismatches, qs.len(), mismatches);
    Ok(())
}
