//! STEP 1 of the resident-cache gate: CORRECTNESS ONLY, no timing.
//!
//! The cache was parked because a stale cache is a CORRECTNESS regression (an
//! old head surviving a revise/retract), and THOR's rule is quality over speed.
//! So the gate is: the cached path must return BYTE-IDENTICAL hits to the cold
//! path over a large, diverse query set INCLUDING the cases built to break a
//! cache - mid-session writes, new events, diverged heads, project scoping.
//!
//! This harness is adversarial on purpose: it does not merely check the happy
//! path, it MUTATES the store under a live cache and demands the cached path
//! still match. A single mismatch fails the gate.
//!
//! Run: cargo run --release --features semantic --example cache_correctness

use std::path::PathBuf;
use thor::event_store::{EventKind, EventStore};
use thor::recall::{recall_fused_scoped, recall_fused_scoped_cached, RecallScope, ResidentCache};

fn local(sub: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"));
    p.push("thor");
    for s in sub {
        p.push(s);
    }
    p
}

/// Full identity of a hit list: order, entity, rev, rank, scope, flags and body
/// must all match. Anything less would let a reordering, a swapped head or a
/// changed score slip through - `rev` is the sharp one, since serving a
/// superseded revision is exactly the stale-cache failure being tested for.
fn fingerprint(hits: &[thor::recall::RecallHit]) -> String {
    hits.iter()
        .map(|h| {
            format!(
                "{}|{}|{:?}|{}|{}|{:?}|{}",
                h.entity_id,
                h.rev,
                h.kind,
                h.is_diverged,
                h.rank,
                h.project,
                h.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n##\n")
}

struct Case {
    query: &'static str,
    scope: RecallScope,
    limit: usize,
    lambda: f64,
}

fn main() -> anyhow::Result<()> {
    let db = local(&["thor.db"]);
    let store = EventStore::new(&db)?;
    let vecs = thor::vectors::VectorStore::open(&thor::vectors::default_vectors_path(&db))?;
    let symbols = thor::symbols::SymbolStore::open(&thor::symbols::default_symbols_path(&db)).ok();
    let mut emb = thor::embed::Embedder::load_default()?;

    // Diverse query set: the real battery (every category), plus shapes chosen
    // to hit different code paths - empty/stopword-only queries (no lexical
    // leg), pure-code identifiers, natural language, Dutch, punctuation, a very
    // long query, and a term that matches nothing.
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
    queries.extend(
        [
            "",
            "the",
            "of the and to",
            "recall_fused_scoped",
            "ResidentCache",
            "hoe werkt de courier precies",
            "waar staat de stripe webhook",
            "!@#$%^&*()",
            "zzzzqqqxxx_nonexistent_term_9876",
            "a",
            "THOR_TS_CHUNK env flag chunker",
            "wat is de afspraak rond em dashes en typografie in alle projecten",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    queries.dedup();

    // Cap the battery: every comparison runs the recall path TWICE (cold+warm),
    // and coverage comes from the diversity of shapes, not from repeating the
    // same shape hundreds of times. QUERY_CAP keeps the gate inside a sane
    // runtime while still spanning every category and edge shape.
    const QUERY_CAP: usize = 90;
    if queries.len() > QUERY_CAP {
        // Keep a spread, not a prefix: the batteries are grouped by category, so
        // a prefix would silently test only the first few categories.
        let step = queries.len() / QUERY_CAP + 1;
        let spread: Vec<String> = queries.iter().step_by(step).cloned().collect();
        let tail: Vec<String> = queries.iter().rev().take(12).cloned().collect();
        queries = spread.into_iter().chain(tail).collect();
        queries.dedup();
    }

    // Scope / limit / lambda variations: scoping and the fusion weight read
    // different parts of the cached state (projects map, head sets), so the gate
    // must cover them, not just the default. Embedding is done ONCE per query
    // and shared across its variants - the embedder is not under test here.
    let variants: Vec<(RecallScope, usize, f64)> = vec![
        (RecallScope::everything(), 5, 1.0),
        (RecallScope::everything(), 1, 2.0),
        (RecallScope::everything(), 20, 0.5),
        (RecallScope::current(Some("The-AI-memory-bible".into())), 5, 1.0),
        (RecallScope::current(Some("acme-shop".into())), 5, 1.0),
        (RecallScope::current(None), 5, 1.0),
    ];
    let mut cases: Vec<Case> = Vec::new();
    let mut qvecs: Vec<(&'static str, Vec<f32>)> = Vec::new();
    for q in &queries {
        let q: &'static str = Box::leak(q.clone().into_boxed_str());
        qvecs.push((q, emb.embed_one(q)?));
        for (scope, limit, lambda) in &variants {
            cases.push(Case { query: q, scope: scope.clone(), limit: *limit, lambda: *lambda });
        }
    }
    let qvec_of = |q: &str| -> &[f32] {
        &qvecs.iter().find(|(k, _)| *k == q).expect("embedded above").1
    };

    println!("queries: {}, cases: {}", queries.len(), cases.len());

    let mut cache = ResidentCache::build(&store, &vecs)?;
    let mut checked = 0usize;
    let mut mismatches: Vec<String> = Vec::new();

    let mut run = |cache: &ResidentCache,
                   c: &Case,
                   qvec: &[f32],
                   checked: &mut usize,
                   mismatches: &mut Vec<String>|
     -> anyhow::Result<()> {
        let cold = recall_fused_scoped(
            &store, c.query, qvec, &vecs, c.limit, c.lambda, &c.scope, true, symbols.as_ref(),
        )?;
        let warm = recall_fused_scoped_cached(
            &store,
            c.query,
            qvec,
            &vecs,
            c.limit,
            c.lambda,
            &c.scope,
            true,
            symbols.as_ref(),
            Some(cache),
        )?;
        *checked += 1;
        if fingerprint(&cold) != fingerprint(&warm) {
            mismatches.push(format!(
                "query={:?} limit={} lambda={} cold={} hits warm={} hits",
                c.query,
                c.limit,
                c.lambda,
                cold.len(),
                warm.len()
            ));
        }
        Ok(())
    };

    // GUARD AGAINST A VACUOUS GATE. "0 mismatches" proves nothing if the cache
    // was never actually used - a cache that always reports itself stale would
    // silently make every comparison cold-vs-cold and pass trivially. So assert
    // the cache IS live before trusting pass 1, and assert it goes DEAD after a
    // write. Without these two, the whole harness is theatre.
    assert!(
        cache.is_valid_for(&store, &vecs),
        "VACUOUS GATE: the freshly built cache reports itself stale, so every \
         comparison below would be cold-vs-cold and prove nothing"
    );
    println!("cache live before writes: {}", cache.is_valid_for(&store, &vecs));

    // PASS 1: static store. The baseline claim - identical when nothing moves.
    println!("\n[pass 1] static store, cache built once");
    for c in &cases {
        run(&cache, c, qvec_of(c.query), &mut checked, &mut mismatches)?;
    }
    assert!(
        cache.is_valid_for(&store, &vecs),
        "cache went stale during a read-only pass - recall must not write"
    );
    println!("  checked {checked}, mismatches {} (cache was LIVE throughout)", mismatches.len());

    // PASS 2: THE ONE THAT MATTERS. Write to the store while the cache is live
    // and keep querying with the SAME (now stale) cache. This is the parked
    // risk: a revise/retract that the cache never saw. The cached path must
    // detect its own staleness and fall back - a mismatch here is the bug that
    // kept this feature parked.
    println!("\n[pass 2] MID-SESSION WRITES against a live (stale) cache");
    let mut store_w = EventStore::new(&db)?;
    let probe_id = "The-AI-memory-bible:mem-cache-probe-DELETEME";
    let before = mismatches.len();

    // 2a. create a new fact the cache has never seen
    let rev1 = store_w.append_event(
        "cache-probe",
        "cache-probe",
        "cache-correctness-harness",
        EventKind::FactCreated,
        probe_id,
        None,
        "CACHE PROBE FACT: zzprobe unique token quokka rebar. Created by the resident-cache correctness harness.",
    )?;
    for c in cases.iter().take(60) {
        run(&cache, c, qvec_of(c.query), &mut checked, &mut mismatches)?;
    }
    let probe_q = "zzprobe unique token quokka rebar";
    let qvec = emb.embed_one(probe_q)?;
    let c = Case { query: probe_q, scope: RecallScope::everything(), limit: 5, lambda: 1.0 };
    run(&cache, &c, &qvec, &mut checked, &mut mismatches)?;
    // The write MUST have invalidated the cache. If this fires, the cache is
    // serving state that no longer matches the store - the exact bug that kept
    // this feature parked.
    assert!(
        !cache.is_valid_for(&store, &vecs),
        "STALE CACHE NOT DETECTED: a fact was appended and the cache still \
         reports itself valid - it would serve the pre-write head"
    );
    // The probe fact must actually be reachable on the cold path, else the
    // write did not land where recall looks and passes 2a-2c test nothing.
    let cold_probe = recall_fused_scoped(
        &store, probe_q, &qvec, &vecs, 5, 1.0, &RecallScope::everything(), true, symbols.as_ref(),
    )?;
    assert!(
        cold_probe.iter().any(|h| h.entity_id == probe_id),
        "probe fact not retrievable - pass 2 would be vacuous"
    );
    println!(
        "  after CREATE: mismatches {} (cache correctly went STALE, probe retrievable)",
        mismatches.len() - before
    );

    // 2b. revise it - the classic stale-head case
    let before_rev = mismatches.len();
    let rev2 = store_w.append_event(
        "cache-probe",
        "cache-probe",
        "cache-correctness-harness",
        EventKind::FactRevised,
        probe_id,
        Some(&rev1.this_hash),
        "CACHE PROBE FACT v2 REVISED: zzprobe unique token quokka rebar. The head moved; a stale cache would serve v1.",
    )?;
    run(&cache, &c, &qvec, &mut checked, &mut mismatches)?;
    for case in cases.iter().take(30) {
        run(&cache, case, qvec_of(case.query), &mut checked, &mut mismatches)?;
    }
    println!("  after REVISE: mismatches {}", mismatches.len() - before_rev);

    // 2c. retract it - the head must disappear from both paths
    let before_ret = mismatches.len();
    store_w.append_event(
        "cache-probe",
        "cache-probe",
        "cache-correctness-harness",
        EventKind::FactRetracted,
        probe_id,
        Some(&rev2.this_hash),
        "[retracted: cache probe cleanup]",
    )?;
    run(&cache, &c, &qvec, &mut checked, &mut mismatches)?;
    for case in cases.iter().take(30) {
        run(&cache, case, qvec_of(case.query), &mut checked, &mut mismatches)?;
    }
    println!("  after RETRACT: mismatches {}", mismatches.len() - before_ret);

    // PASS 3: an explicitly refreshed cache must also stay identical - the
    // normal daemon loop (refresh, then serve).
    println!("\n[pass 3] refreshed cache after the writes");
    let before_r = mismatches.len();
    cache = cache.refreshed(&store, &vecs)?;
    assert!(
        cache.is_valid_for(&store, &vecs),
        "refreshed cache still reports stale - refresh is broken"
    );
    // The refreshed cache must see the RETRACTED probe, i.e. the cached path
    // must NOT surface it. This is the head-moved case the whole gate exists
    // for: a snapshot cache would still serve the live v1/v2 body here.
    let warm_probe = recall_fused_scoped_cached(
        &store,
        probe_q,
        &qvec,
        &vecs,
        5,
        1.0,
        &RecallScope::everything(),
        true,
        symbols.as_ref(),
        Some(&cache),
    )?;
    let stale_body = warm_probe
        .iter()
        .any(|h| h.entity_id == probe_id && !h.body.contains("[retracted"));
    assert!(
        !stale_body,
        "STALE HEAD SERVED: the refreshed cache surfaced a live body for a \
         RETRACTED entity - this is the regression the park was about"
    );
    for c in cases.iter().take(120) {
        run(&cache, c, qvec_of(c.query), &mut checked, &mut mismatches)?;
    }
    println!(
        "  checked so far {checked}, new mismatches {} (cache LIVE, retracted head not served)",
        mismatches.len() - before_r
    );

    // VERDICT
    println!("\n{}", "=".repeat(70));
    println!("GATE: {} comparisons, {} mismatches", checked, mismatches.len());
    if mismatches.is_empty() {
        println!("RESULT: PASS - cached path byte-identical to cold path everywhere");
    } else {
        println!("RESULT: FAIL - the cache changes answers. Do NOT adopt.");
        for m in mismatches.iter().take(20) {
            println!("  {m}");
        }
    }
    println!("\nNOTE: probe events remain in the store as retracted history");
    println!("      (entity {probe_id}) - append-only, nothing is deleted.");
    Ok(())
}
