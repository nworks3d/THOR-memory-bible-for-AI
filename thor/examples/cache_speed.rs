//! STEP 2 of the resident-cache gate: the speed measurement, run ONLY after
//! step 1 (cache_correctness) passes.
//!
//! Measures the recall path alone - the embedder is excluded (every query is
//! embedded once, up front) because it is identical on both sides and would
//! dilute the very number under test. Wall-clock, per query, median + p90 over
//! the whole battery, cold vs warm.
//!
//! Doubles as the independent check that the cache is REALLY used: if warm is
//! not materially faster, the cached path is silently falling back to cold and
//! the correctness gate was vacuous.
//!
//! Run: cargo run --release --features semantic --example cache_speed

use std::path::PathBuf;
use std::time::Instant;
use thor::event_store::EventStore;
use thor::recall::{recall_fused_scoped, recall_fused_scoped_cached, RecallScope, ResidentCache};

fn local(sub: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"));
    p.push("thor");
    for s in sub {
        p.push(s);
    }
    p
}

fn stats(mut v: Vec<f64>) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = v[v.len() / 2];
    let p90 = v[((v.len() as f64 * 0.9) as usize).min(v.len() - 1)];
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    (median, p90, mean)
}

fn main() -> anyhow::Result<()> {
    let db = local(&["thor.db"]);
    let store = EventStore::new(&db)?;
    let vecs = thor::vectors::VectorStore::open(&thor::vectors::default_vectors_path(&db))?;
    let symbols = thor::symbols::SymbolStore::open(&thor::symbols::default_symbols_path(&db)).ok();
    let mut emb = thor::embed::Embedder::load_default()?;

    let mut queries: Vec<String> = Vec::new();
    for f in ["percategory_queries.json", "queries_full.json"] {
        if let Ok(file) = std::fs::File::open(local(&["eval", f])) {
            if let Ok(items) = serde_json::from_reader::<_, Vec<serde_json::Value>>(file) {
                queries.extend(
                    items
                        .iter()
                        .filter_map(|q| q.get("query").and_then(|v| v.as_str()).map(String::from)),
                );
            }
        }
    }
    queries.truncate(60);
    println!("battery: {} queries (embedded up front, excluded from timings)", queries.len());

    let qvecs: Vec<(String, Vec<f32>)> =
        queries.iter().map(|q| Ok((q.clone(), emb.embed_one(q)?))).collect::<anyhow::Result<_>>()?;

    let scope = RecallScope::everything();
    let cache = ResidentCache::build(&store, &vecs)?;
    assert!(
        cache.is_valid_for(&store, &vecs),
        "cache reports stale on a static store - the timings below would be meaningless"
    );

    // Warm-up: first touch pulls pages into the OS cache. Timing that would
    // measure disk warmth, not the code path.
    for (q, v) in qvecs.iter().take(5) {
        recall_fused_scoped(&store, q, v, &vecs, 5, 1.0, &scope, true, symbols.as_ref())?;
        recall_fused_scoped_cached(
            &store, q, v, &vecs, 5, 1.0, &scope, true, symbols.as_ref(), Some(&cache),
        )?;
    }

    // Interleaved A/B: cold and warm alternate per query, so any drift (thermal,
    // background load) hits both arms equally instead of one block.
    let mut cold_ms = Vec::new();
    let mut warm_ms = Vec::new();
    for (q, v) in &qvecs {
        let t0 = Instant::now();
        let a = recall_fused_scoped(&store, q, v, &vecs, 5, 1.0, &scope, true, symbols.as_ref())?;
        cold_ms.push(t0.elapsed().as_secs_f64() * 1000.0);

        let t1 = Instant::now();
        let b = recall_fused_scoped_cached(
            &store, q, v, &vecs, 5, 1.0, &scope, true, symbols.as_ref(), Some(&cache),
        )?;
        warm_ms.push(t1.elapsed().as_secs_f64() * 1000.0);

        // Same answer, every query - a fast wrong answer is not a win.
        assert_eq!(a.len(), b.len(), "hit count differs on {q:?}");
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.entity_id, y.entity_id, "order/entity differs on {q:?}");
            assert_eq!(x.rev, y.rev, "REV differs on {q:?} - stale head served");
        }
    }

    let (cm, cp, cmean) = stats(cold_ms.clone());
    let (wm, wp, wmean) = stats(warm_ms.clone());
    println!("\n{:<8} {:>10} {:>10} {:>10}", "arm", "median", "p90", "mean");
    println!("{:<8} {:>9.1}ms {:>9.1}ms {:>9.1}ms", "cold", cm, cp, cmean);
    println!("{:<8} {:>9.1}ms {:>9.1}ms {:>9.1}ms", "warm", wm, wp, wmean);
    println!(
        "\nmedian: {:.1}ms -> {:.1}ms  =  {:.0}% faster ({:.1}x)",
        cm,
        wm,
        100.0 * (cm - wm) / cm,
        cm / wm
    );

    // The cache is only worth holding if validation is cheap relative to the
    // fold it replaces - that asymmetry IS the design.
    let t = Instant::now();
    for _ in 0..100 {
        std::hint::black_box(cache.is_valid_for(&store, &vecs));
    }
    let valid_ms = t.elapsed().as_secs_f64() * 1000.0 / 100.0;
    let t = Instant::now();
    let rebuilt = ResidentCache::build(&store, &vecs)?;
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    std::hint::black_box(&rebuilt);
    println!(
        "\nvalidation: {:.3}ms per query   |   full rebuild: {:.0}ms   |   ratio 1:{:.0}",
        valid_ms,
        build_ms,
        build_ms / valid_ms.max(0.0001)
    );
    println!(
        "  -> a write costs one rebuild ({:.0}ms) and every unchanged query saves {:.0}ms",
        build_ms,
        cm - wm
    );
    Ok(())
}
