//! Lane A identifier battery (2026-08-05): measures whether a BM25-fused
//! literal-hit ranking beats the current live cosine-only reorder, on SHORT
//! identifier-style queries (function/config names, file paths, commands,
//! flags, error/quoted strings) - the query shape `lookup`'s own
//! crate-level doc comment says it is built for ("an explicit search/key
//! request"), and the shape the 199-question native battery could not
//! exercise at all (0/199 literal hits there, see LANE-A-RESULTS.md). Every
//! query in THIS battery has >=1 literal hit by construction (see
//! LANE-A-IDENTIFIER-BATTERY.md for how the battery and its gold were built,
//! and why gold is not circular with the mechanism under test).
//!
//! Read-only. Point IDB_DB at a COPY of the store, never the live one.
//!
//! Battery JSON (IDB_BATTERY): array of {query, category, gold, hit_count,
//! hit_ids}. `gold` is the ONE entity id a human (reading the corpus,
//! preferring the sole distinct-content atomic Rule/Orientation fact over a
//! Report mention) judged the genuine answer. `hit_ids` were recorded at
//! battery-build time; this harness RE-DERIVES the literal-hit set from the
//! live corpus at run time and reports if it ever disagrees with the
//! recorded set (the corpus copy should be identical, but never assumed).
//!
//! WHY THIS HARNESS NEVER COMPUTES SEMANTIC EXTRAS (unlike recall_native.rs):
//! `rank_literal_and_extras`'s own contract guarantees a literal hit is
//! never dropped, and an extra (a semantic-only candidate that is NOT
//! already a literal hit) always ranks strictly AFTER every literal hit in
//! the final order (`lit_order.into_iter().chain(extra_order)` in
//! `search_best_effort_cached`). Every query in this battery has its gold
//! WITHIN its own literal-hit set (hit_count <= 8 for the whole battery) by
//! construction, so gold's final rank is exactly its rank among the
//! LITERAL-hit reordering, whatever that reordering is - extras can only
//! ever fill ranks beyond hit_count and can never lift gold into a higher
//! slot, nor could they ever push it down (a literal hit is never
//! re-sorted after an extra). This is a structural guarantee of the shared
//! ranking function's contract, not an empirical shortcut, so extras are
//! correctly out of scope for this measurement. The three arms below differ
//! ONLY in how the literal hits are reordered among themselves - exactly
//! the mechanism LANE-A-RESULTS.md's diagnosis is about.
//!
//! Arms:
//! - A (live, unchanged): `serve::lookup::rank_literal_and_extras`'s own
//!   literal-hit reorder - pure cosine to the query.
//! - B (BM25 fused): literal hits ranked by `bm_norm + lambda*max(cos,0)`, an
//!   own from-scratch BM25 (standard defaults k1=1.2, b=0.75; smoothed idf,
//!   min-max normalized) computed over the QUERY'S OWN literal-hit pool -
//!   the only pool available without a second corpus-wide lexical index, and
//!   the natural reading of LANE-A-RESULTS.md's "own smoothed idf, min-max
//!   normalized over the pool". Rebuilt to that spec; the original A2/A3
//!   code no longer exists to copy verbatim (implemented and reverted in an
//!   uncommitted working tree, never committed - see that file's own "A2/A3"
//!   section).
//! - C (lexical only): the same fused function at lambda=0.0, isolating
//!   whether cosine reordering is actively HARMFUL on this query shape (the
//!   review's literal claim, distinct from "BM25 fusion helps").
//!
//! Run: IDB_DB=<copy of thor.db> IDB_BATTERY=<tune-or-holdout.json> \
//!      cargo run --release --features semantic --example identifier_battery_eval

use serde::Deserialize;
use serve::embed::Embedder;
use serve::lookup::{rank_literal_and_extras, search_with_expired};
use serve::semantic_paths::{default_model_dir, default_vectors_path};
use serve::vectors::VectorStore;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use thor_core::event_store::EventStore;

// ------------------------------------------------------------- BM25 (Arm B/C)

/// Lowercase alphanumeric tokens - the same simple split THOR 1.0's own
/// `tokens()` used (see `thor/src/recall.rs`), minus the FTS5-escaping this
/// in-memory scorer never needs.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Standard BM25 (Robertson/Sparck-Jones), k1=1.2, b=0.75, smoothed idf
/// (the "+1" form: always positive, never lets a term that occurs in every
/// pool document swing the score negative). Computed over `pool_docs` (the
/// literal-hit pool for one query) - see this file's own doc comment on why
/// that is the pool this scorer's idf/avgdl are drawn from.
fn bm25_raw_scores(pool_docs: &[Vec<String>], query_tokens: &[String]) -> Vec<f64> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let n = pool_docs.len();
    if n == 0 {
        return Vec::new();
    }
    let doc_len: Vec<usize> = pool_docs.iter().map(|d| d.len()).collect();
    let avgdl: f64 = doc_len.iter().sum::<usize>() as f64 / n as f64;

    // Document frequency per distinct query token, over the pool only.
    let mut df: HashMap<&str, usize> = HashMap::new();
    let distinct_terms: Vec<&str> = {
        let mut seen = HashSet::new();
        query_tokens.iter().map(String::as_str).filter(|t| seen.insert(*t)).collect()
    };
    for t in &distinct_terms {
        df.insert(t, pool_docs.iter().filter(|d| d.iter().any(|w| w == t)).count());
    }
    let idf: HashMap<&str, f64> = distinct_terms
        .iter()
        .map(|&t| {
            let dfi = df[t] as f64;
            let v = (((n as f64 - dfi + 0.5) / (dfi + 0.5)) + 1.0).ln();
            (t, v)
        })
        .collect();

    pool_docs
        .iter()
        .enumerate()
        .map(|(i, doc)| {
            let mut tf: HashMap<&str, usize> = HashMap::new();
            for w in doc {
                if let Some(slot) = distinct_terms.iter().find(|t| *t == w) {
                    *tf.entry(*slot).or_insert(0) += 1;
                }
            }
            let dl = doc_len[i] as f64;
            distinct_terms
                .iter()
                .map(|&t| {
                    let f = *tf.get(t).unwrap_or(&0) as f64;
                    if f == 0.0 {
                        return 0.0;
                    }
                    idf[t] * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * dl / avgdl))
                })
                .sum()
        })
        .collect()
}

/// Min-max normalize to [0,1] over the pool; an all-equal pool (max==min,
/// including the empty/all-zero case) carries no discriminating signal, so
/// every score is set to 0.0 rather than an arbitrary constant.
fn min_max_normalize(scores: &[f64]) -> Vec<f64> {
    let max = scores.iter().cloned().fold(f64::MIN, f64::max);
    let min = scores.iter().cloned().fold(f64::MAX, f64::min);
    if !(max > min) {
        return vec![0.0; scores.len()];
    }
    scores.iter().map(|s| (s - min) / (max - min)).collect()
}

/// Arm B/C's shared literal-hit ranking: `bm_norm + lambda*max(cos,0)`,
/// descending, ties broken by id ascending (house convention). `lambda=0.0`
/// is Arm C (lexical only, cosine never consulted for ranking - though a
/// missing vector still means cos=0.0, which is what lambda=0 already makes
/// irrelevant).
fn rank_bm25_fused(
    literal_ids: &[String],
    pool_docs: &[Vec<String>],
    query_tokens: &[String],
    qvec: &[f32],
    vectors: &HashMap<String, Vec<f32>>,
    lambda: f64,
) -> Vec<String> {
    let raw = bm25_raw_scores(pool_docs, query_tokens);
    let bm_norm = min_max_normalize(&raw);
    let cos = |id: &str| -> f64 {
        vectors.get(id).map(|v| fastembed::similarity::cosine_similarity(qvec, v) as f64).unwrap_or(0.0).max(0.0)
    };
    let mut scored: Vec<(f64, &str)> =
        literal_ids.iter().enumerate().map(|(i, id)| (bm_norm[i] + lambda * cos(id), id.as_str())).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().map(|(_, id)| id.to_string()).collect()
}

// --------------------------------------------------------------- battery I/O

#[derive(Deserialize, Clone)]
struct BatteryQ {
    query: String,
    category: String,
    gold: String,
    hit_count: usize,
    #[allow(dead_code)]
    hit_ids: Vec<String>,
}

fn hit_at(ranked: &[String], k: usize, gold: &str) -> bool {
    ranked.iter().take(k).any(|id| id == gold)
}

struct Recall {
    r1: usize,
    r3: usize,
    r5: usize,
}
impl Recall {
    fn new() -> Self {
        Recall { r1: 0, r3: 0, r5: 0 }
    }
    fn record(&mut self, ranked: &[String], gold: &str) {
        if hit_at(ranked, 1, gold) {
            self.r1 += 1;
        }
        if hit_at(ranked, 3, gold) {
            self.r3 += 1;
        }
        if hit_at(ranked, 5, gold) {
            self.r5 += 1;
        }
    }
    fn line(&self, label: &str, n: usize) {
        println!(
            "{label:<22} recall@1 {:3}/{:<3} {:5.1}%   recall@3 {:3}/{:<3} {:5.1}%   recall@5 {:3}/{:<3} {:5.1}%",
            self.r1, n, pct(self.r1, n), self.r3, n, pct(self.r3, n), self.r5, n, pct(self.r5, n)
        );
    }
}
fn pct(x: usize, n: usize) -> f64 {
    if n == 0 {
        0.0
    } else {
        100.0 * x as f64 / n as f64
    }
}

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("IDB_DB").expect("set IDB_DB to a COPY of thor.db"));
    let battery_path = PathBuf::from(std::env::var("IDB_BATTERY").expect("set IDB_BATTERY to a tune/holdout json"));
    let vpath = default_vectors_path(&db);
    let model_dir = default_model_dir().expect("no per-user model dir resolved");
    let store = EventStore::new(&db)?;

    // The exact corpus `search`/`search_best_effort_cached` serve: live,
    // non-Lookup, non-expired - `search_with_expired(&store, "")` matches
    // every item (an empty pattern is a substring of every string), so this
    // is the production corpus scope with zero reimplemented filtering.
    let (hits, _withheld) = search_with_expired(&store, "");
    let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
    let text_lower: Vec<String> = hits.iter().map(|h| h.item.text.to_lowercase()).collect();
    let tags_lower: Vec<Vec<String>> = hits.iter().map(|h| h.item.tags.iter().map(|t| t.to_lowercase()).collect()).collect();
    let tokens: Vec<Vec<String>> = hits.iter().map(|h| tokenize(&h.item.text)).collect();

    let vs = VectorStore::open(&vpath)?;
    let vectors = vs.get_many(&ids)?;
    eprintln!(
        "corpus: {} live items, {} with a vector ({:.0}% coverage), sidecar model_id: {:?}",
        ids.len(),
        vectors.len(),
        100.0 * vectors.len() as f64 / ids.len().max(1) as f64,
        vs.model_id()
    );

    let mut emb = Embedder::load(&model_dir)?;

    let battery: Vec<BatteryQ> = serde_json::from_reader(std::fs::File::open(&battery_path)?)?;
    println!("=== identifier battery: {} queries ({}) ===", battery.len(), battery_path.display());

    // Re-derive the literal-hit set live and check it against what the
    // battery recorded at build time - a silent drift here would be a
    // silently wrong measurement.
    let mut drift = 0usize;
    let mut resolved: Vec<(BatteryQ, Vec<usize>)> = Vec::new();
    for q in &battery {
        let ql = q.query.to_lowercase();
        let idx: Vec<usize> =
            (0..ids.len()).filter(|&i| text_lower[i].contains(&ql) || tags_lower[i].iter().any(|t| t.contains(&ql))).collect();
        let live_ids: HashSet<&str> = idx.iter().map(|&i| ids[i].as_str()).collect();
        let recorded_ids: HashSet<&str> = q.hit_ids.iter().map(String::as_str).collect();
        if live_ids != recorded_ids {
            drift += 1;
        }
        if !live_ids.contains(q.gold.as_str()) {
            eprintln!("SKIP (gold no longer a live literal hit): query={:?} gold={}", q.query, q.gold);
            continue;
        }
        resolved.push((q.clone(), idx));
    }
    println!(
        "literal-hit set drift vs recorded battery: {}/{} queries (0 expected on an unmodified corpus copy)",
        drift,
        battery.len()
    );
    let n = resolved.len();
    println!("resolved against the live corpus: {n}/{} (see SKIP lines above for any drop)", battery.len());

    let lambdas_b = [1.0f64, 1.5, 2.5];

    let mut arm_a = Recall::new();
    let mut arm_c = Recall::new();
    let mut arm_b: HashMap<String, Recall> = lambdas_b.iter().map(|l| (format!("{l}"), Recall::new())).collect();
    let mut arm_d = Recall::new();
    let mut arm_d_mismatches = 0usize;

    let mut per_cat_a: HashMap<String, (usize, usize)> = HashMap::new(); // (hit@1, n)

    for (q, idx) in &resolved {
        let qvec = emb.embed_one(&q.query)?;
        let literal_ids: Vec<String> = idx.iter().map(|&i| ids[i].clone()).collect();
        let pool_docs: Vec<Vec<String>> = idx.iter().map(|&i| tokens[i].clone()).collect();
        let query_tokens = tokenize(&q.query);

        // Arm A: the PRE-2026-08-05 live behavior - pure cosine reorder,
        // measured here as a baseline for comparison against Arms B/C (the
        // shared fn itself is now BM25-fused live, so Arm A is reproduced by
        // passing an empty query and an empty text map: an empty query
        // tokenizes to zero terms, so the BM25 leg contributes 0 to every
        // candidate uniformly and the fused score collapses to a positive
        // scaling of cosine alone - see lookup.rs's own test module doc
        // comment for the same trick). Called with an impossible floor/zero
        // cap so it returns literal order only (no extras - see this file's
        // own doc comment on why extras are out of scope here).
        let empty_texts: HashMap<String, String> = HashMap::new();
        let (lit_a, _) =
            rank_literal_and_extras("", &qvec, &literal_ids, &[], &vectors, &empty_texts, f32::INFINITY, 0);
        arm_a.record(&lit_a, &q.gold);
        let e = per_cat_a.entry(q.category.clone()).or_insert((0, 0));
        e.1 += 1;
        if hit_at(&lit_a, 1, &q.gold) {
            e.0 += 1;
        }

        // Arm C: BM25 lexical only (lambda=0.0).
        let lit_c = rank_bm25_fused(&literal_ids, &pool_docs, &query_tokens, &qvec, &vectors, 0.0);
        arm_c.record(&lit_c, &q.gold);

        // Arm B: BM25 fused, swept lambda (this file's own from-scratch BM25,
        // kept independent of lookup.rs's copy on purpose - see Arm D below).
        for &lambda in &lambdas_b {
            let lit_b = rank_bm25_fused(&literal_ids, &pool_docs, &query_tokens, &qvec, &vectors, lambda);
            arm_b.get_mut(&format!("{lambda}")).unwrap().record(&lit_b, &q.gold);
        }

        // Arm D: cross-check ONLY - calls the ACTUAL shipped
        // `rank_literal_and_extras` (real query, real text+tags map) and
        // confirms it agrees with this file's own independent Arm B
        // lambda=2.5 reimplementation. Two separate BM25 implementations
        // landing on the exact same ranking for every query is the honest
        // evidence that lookup.rs's shipped copy is not a divergent one.
        let real_texts: HashMap<String, String> =
            idx.iter().map(|&i| (ids[i].clone(), hits[i].item.text.clone())).collect();
        let (lit_d, _) = rank_literal_and_extras(&q.query, &qvec, &literal_ids, &[], &vectors, &real_texts, f32::INFINITY, 0);
        arm_d.record(&lit_d, &q.gold);
        let lit_b25 = rank_bm25_fused(&literal_ids, &pool_docs, &query_tokens, &qvec, &vectors, 2.5);
        if lit_d != lit_b25 {
            arm_d_mismatches += 1;
            eprintln!("ARM D MISMATCH on query {:?}: shipped={:?} reimpl={:?}", q.query, lit_d, lit_b25);
        }
        let _ = q.hit_count; // used only for the drift/skip diagnostics above
    }

    println!("\n=== arms on {n} queries ===");
    arm_a.line("A (live, cosine)", n);
    arm_c.line("C (BM25 only)", n);
    for &lambda in &lambdas_b {
        arm_b[&format!("{lambda}")].line(&format!("B (fused, lambda={lambda})"), n);
    }
    arm_d.line("D (shipped fn, lambda=2.5)", n);
    println!(
        "Arm D vs this file's own lambda=2.5 reimplementation: {} / {n} queries produced a DIFFERENT literal order (0 expected)",
        arm_d_mismatches
    );

    println!("\nper-category recall@1, Arm A (live):");
    let mut cats: Vec<&String> = per_cat_a.keys().collect();
    cats.sort();
    for c in cats {
        let (h, cn) = per_cat_a[c];
        println!("  {c:<18} {h:3}/{cn:<3} {:5.1}%", pct(h, cn));
    }

    Ok(())
}
