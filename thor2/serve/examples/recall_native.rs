//! 2.0-NATIVE recall measurement for THOR 2.0's surface-4 lookup (feature
//! `semantic`). Unlike `recall2_eval` (which reused the 1.0 460-query battery
//! and a CONTENT proxy against 1.0 gold text - a severe undercount, see
//! LAYER2-RECALL-FINDINGS.md), this scores against a battery whose gold is a
//! real 2.0 ENTITY ID: each query is a natural-language question written
//! against a fact that actually lives in the 2.0 store, and a hit is that
//! fact's id appearing in the top-k. No content proxy, no 1.0 seq remap.
//!
//! Read-only. Point it at a COPY of the store, never the live one.
//!
//! Battery JSON (RECALL2_BATTERY, default <eval>/battery_native.json): an array
//! of {id, category, query, gold}. `gold` is the 2.0 entity id the question is
//! about. A query whose gold is not a live, non-Lookup, non-expired item is
//! reported and skipped (it cannot be answered by the fact door at all).
//!
//! NEAR-DUPLICATE GOLD EXPANSION. The store holds paraphrase clusters (the same
//! fact re-typed under several ids). A question written from one cluster member
//! is answered just as correctly by any other, so the gold set is expanded to
//! every item whose fact-vector cosine to the seed gold is >= `DUP` (0.92,
//! near-identical). Both the strict (seed-only) and expanded recall are printed
//! so the honest range is visible.
//!
//! THE DIVERGENCE TRAP (why this file has almost no ranking logic of its own,
//! 2026-08-05): an earlier version of this harness carried its OWN copy of
//! the literal-hit ranking (filter + reorder), mirroring live by hand. That is
//! exactly the trap a BM25 lexical-fusion leg was meant to close (A2/A3):
//! literal-hit ranking here calls `serve::lookup::rank_literal_and_extras`
//! directly - the SAME function `search_best_effort_cached` calls live - via
//! the small index<->id adapters below (`literal_filter`, `literal_order`,
//! `rank_live`), so this harness carries exactly one literal-hit ranking
//! algorithm, the live one. A BM25 leg was tried in that shared function and
//! REVERTED (see `lookup.rs`'s own doc comment and `LANE-A-RESULTS.md`): this
//! battery's 199 natural-language questions produce ZERO literal hits against
//! the fact corpus (0/199), so the shared function - live and here alike -
//! still runs the plain cosine-only reorder from the 2026-08-03 fix. Only the
//! EXTRAS-gate *exploration* below (the silence-gate band knob, the
//! margin-vs-field sweep) stays local: neither shape exists in live at all
//! (live is a flat floor + cap), so there is no live function to share - see
//! LAYER3-RECALL-2.0-NATIVE.md ("measured and BEATEN by the bare floor" / "a
//! product call, not shipped").
//!
//! Run: RECALL2_DB=<copy of thor.db> \
//!      cargo run --release --features semantic --example recall_native
//! Optional: RECALL2_FLOOR (MIN_SIMILARITY, default 0.45) overrides the
//! "recall at the LIVE gate" table below, for the A4 floor recheck without a
//! rebuild.

use model::item::Kind;
use serde::Deserialize;
use serde_json::Value;
use serve::embed::Embedder;
use serve::live::live_items;
use serve::lookup::rank_literal_and_extras;
use serve::semantic_paths::{default_model_dir, default_vectors_path};
use serve::vectors::VectorStore;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// Compared against an `expires` value; an item past it is not served by lookup.
const TODAY: &str = "2026-08-03";
/// The live `MAX_SEMANTIC_EXTRA` cap in lookup.rs - matched here for faithfulness.
const CAP: usize = 10;
/// Fact-vector cosine at/above which two facts count as the same answer.
const DUP: f32 = 0.92;

fn is_expired(expires: Option<&str>) -> bool {
    matches!(expires, Some(d) if d.len() == 10 && d < TODAY)
}

/// True cosine, identical result to the live `fastembed::similarity`
/// (both normalise), kept local so the example needs no extra dependency edge.
fn cos(a: &[f32], b: &[f32]) -> f32 {
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

fn eval_dir() -> PathBuf {
    std::env::var("RECALL2_EVAL").map(PathBuf::from).unwrap_or_else(|_| {
        let mut p = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"));
        p.push("thor");
        p.push("eval");
        p
    })
}

/// One corpus item's precomputed scoring inputs.
struct Item {
    lower: String,
    tags_lower: Vec<String>,
    vec: Option<Vec<f32>>,
}

/// One battery question, resolved against the live corpus.
struct Q {
    category: String,
    qlower: String,
    qvec: Vec<f32>,
    gold_seed: usize,
    gold_expanded: HashSet<usize>,
}

/// The literal side: every item whose text OR a tag contains the whole query
/// as a substring - a pure FILTER, in arbitrary (index) order. Ordering is
/// never done here any more (see this file's own doc comment): it is always
/// delegated to `serve::lookup::rank_literal_and_extras` via `literal_order`
/// below, so this harness carries exactly one literal-hit ranking algorithm,
/// the live one.
fn literal_filter(items: &[Item], qlower: &str) -> Vec<usize> {
    (0..items.len()).filter(|&i| items[i].lower.contains(qlower) || items[i].tags_lower.iter().any(|t| t.contains(qlower))).collect()
}

/// `literal_idx` (from `literal_filter`) reordered by the live fused
/// (BM25+cosine) score - a thin index<->id adapter over
/// `serve::lookup::rank_literal_and_extras`, called with an impossible
/// similarity floor and a zero cap so it returns no extras, only the
/// literal reorder. This is the ONE place ranking intuition is allowed to
/// enter this file: an index/id lookup, not a scoring decision. `texts` is
/// id -> lowercased text+tags (already lowercased is fine: BM25 tokenizes
/// case-insensitively regardless).
fn literal_order(
    vectors: &HashMap<String, Vec<f32>>,
    texts: &HashMap<String, String>,
    id2idx: &HashMap<String, usize>,
    ids: &[String],
    literal_idx: &[usize],
    qlower: &str,
    qvec: &[f32],
) -> Vec<usize> {
    let literal_ids: Vec<String> = literal_idx.iter().map(|&i| ids[i].clone()).collect();
    let (lit_order, _extras) = rank_literal_and_extras(qlower, qvec, &literal_ids, &[], vectors, texts, f32::INFINITY, 0);
    lit_order.iter().map(|id| id2idx[id]).collect()
}

/// The full live-gate ranking: literal hits in fused (BM25+cosine) order
/// (shared code) plus the semantic-only extras, flat floor + cap - exactly
/// `search_best_effort_cached`'s own shape, because it is the SAME function.
fn rank_live(
    vectors: &HashMap<String, Vec<f32>>,
    texts: &HashMap<String, String>,
    id2idx: &HashMap<String, usize>,
    ids: &[String],
    items: &[Item],
    qlower: &str,
    qvec: &[f32],
    min_similarity: f32,
    max_extra: usize,
) -> Vec<usize> {
    let literal_idx = literal_filter(items, qlower);
    let literal_ids: Vec<String> = literal_idx.iter().map(|&i| ids[i].clone()).collect();
    let (lit_order, extra_order) =
        rank_literal_and_extras(qlower, qvec, &literal_ids, ids, vectors, texts, min_similarity, max_extra);
    lit_order.into_iter().chain(extra_order).map(|id| id2idx[&id]).collect()
}

/// The semantic side: every non-literal candidate with a vector, scored and
/// sorted best-first, BEFORE any floor or gate. `lit_set` is what the literal
/// side already took. EXPLORATORY ONLY (see this file's own doc comment):
/// feeds the band/margin gate sweeps below, neither of which live implements.
fn semantic_scored(items: &[Item], qvec: &[f32], lit_set: &HashSet<usize>) -> Vec<(f32, usize)> {
    let mut sem: Vec<(f32, usize)> = (0..items.len())
        .filter(|i| !lit_set.contains(i))
        .filter_map(|i| items[i].vec.as_deref().map(|v| (cos(qvec, v), i)))
        .collect();
    sem.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    sem
}

/// A silence gate over the semantic extras. `open_floor`: the gate returns NO
/// semantic extras unless the best one is at least this high (a peaked answer,
/// not a flat mediocre plateau). `band`: once open, keep only extras within
/// `band` of the top (a margin rule that trims the tail). `base_floor`: the
/// hard minimum an extra must clear regardless. Live default = gate(0.45, INF,
/// 0.45), i.e. keep every extra >= 0.45. EXPLORATORY ONLY: the `band` knob has
/// no live counterpart (live is a flat floor); see this file's own doc comment.
fn gated_extras(sem: &[(f32, usize)], open_floor: f32, band: f32, base_floor: f32) -> Vec<usize> {
    let Some(&(top, _)) = sem.first() else { return Vec::new() };
    if top < open_floor {
        return Vec::new();
    }
    let dyn_floor = base_floor.max(top - band);
    sem.iter().filter(|(s, _)| *s >= dyn_floor).take(CAP).map(|(_, i)| *i).collect()
}

/// A MARGIN-vs-field silence gate. A real answer is a PEAK (its cosine stands
/// clearly above the rest of the field); a query with no answer is a flat
/// mediocre plateau. So the gate opens only when `top - field[ref_rank]` (the
/// margin between the best score and a reference rank deeper in the field) is at
/// least `margin`; otherwise it returns nothing. When open, it keeps extras
/// within `band` of the top and above `base_floor`, capped. This is the answer
/// to "a bare cosine floor is too weak" - a floor cannot tell a peak from a
/// plateau, a margin can. EXPLORATORY ONLY: live has no margin gate at all
/// (see LAYER3-RECALL-2.0-NATIVE.md: measured and beaten by the bare floor).
fn gated_extras_margin(sem: &[(f32, usize)], margin: f32, ref_rank: usize, band: f32, base_floor: f32) -> Vec<usize> {
    let Some(&(top, _)) = sem.first() else { return Vec::new() };
    let field = sem.get(ref_rank).map(|(s, _)| *s).unwrap_or(0.0);
    if top - field < margin {
        return Vec::new(); // flat field: no clear peak, stay silent
    }
    let dyn_floor = base_floor.max(top - band);
    sem.iter().filter(|(s, _)| *s >= dyn_floor).take(CAP).map(|(_, i)| *i).collect()
}

fn hit_at(ranked: &[usize], k: usize, gold: &HashSet<usize>) -> bool {
    ranked.iter().take(k).any(|i| gold.contains(i))
}

fn pct(x: usize, n: usize) -> f64 {
    if n == 0 {
        0.0
    } else {
        100.0 * x as f64 / n as f64
    }
}

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("RECALL2_DB").expect("set RECALL2_DB to a COPY of thor.db"));
    let vpath = default_vectors_path(&db);
    let model_dir = default_model_dir().expect("no per-user model dir resolved");
    let store = EventStore::new(&db)?;

    // A4 knob, overridable without a rebuild - see this file's own doc
    // comment. Default is the PRE-A4 floor (0.45); pass RECALL2_FLOOR=0.50 to
    // recheck the A4 change on this exact battery.
    let live_floor: f32 = std::env::var("RECALL2_FLOOR").ok().and_then(|s| s.parse().ok()).unwrap_or(0.45);
    println!("MIN_SIMILARITY (floor) = {live_floor}  (RECALL2_FLOOR to override)");

    // The live fact corpus surface-4 can serve: non-Lookup, non-expired.
    let raw: Vec<(String, String, Vec<String>)> = live_items(&store)
        .into_iter()
        .filter(|li| li.item.kind != Kind::Lookup && !is_expired(li.item.expires.as_deref()))
        .map(|li| (li.id, li.item.text, li.item.tags))
        .collect();
    let ids: Vec<String> = raw.iter().map(|(id, _, _)| id.clone()).collect();
    let id2idx: HashMap<String, usize> = ids.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();

    let vs = VectorStore::open(&vpath)?;
    let vectors = vs.get_many(&ids)?;
    let items: Vec<Item> = raw
        .iter()
        .map(|(id, text, tags)| Item {
            lower: text.to_lowercase(),
            tags_lower: tags.iter().map(|t| t.to_lowercase()).collect(),
            vec: vectors.get(id).cloned(),
        })
        .collect();
    let n_vec = items.iter().filter(|i| i.vec.is_some()).count();
    eprintln!(
        "live fact items: {}   with a current vector: {} ({:.0}% coverage)   sidecar model_id: {:?}",
        items.len(),
        n_vec,
        100.0 * n_vec as f64 / items.len().max(1) as f64,
        vs.model_id()
    );
    // id -> lowercased TEXT ONLY (not tags - matches lookup.rs's own
    // `texts` construction exactly, see that function's doc comment on why
    // tags are excluded), for the BM25 leg of `rank_literal_and_extras`
    // (already-lowercased is fine: BM25 tokenizes case-insensitively anyway).
    let texts: HashMap<String, String> = ids.iter().enumerate().map(|(i, id)| (id.clone(), items[i].lower.clone())).collect();

    let mut emb = Embedder::load(&model_dir)?;

    // ---- Load the 2.0-native battery and resolve every gold to a live item.
    #[derive(Deserialize)]
    struct RawQ {
        #[serde(default)]
        category: String,
        query: String,
        gold: String,
    }
    let battery_path =
        std::env::var("RECALL2_BATTERY").map(PathBuf::from).unwrap_or_else(|_| eval_dir().join("battery_native.json"));
    let raw_qs: Vec<RawQ> = serde_json::from_reader(std::fs::File::open(&battery_path)?)?;

    let mut battery: Vec<Q> = Vec::new();
    let mut missing_gold = 0usize;
    let mut expand_total = 0usize;
    for rq in &raw_qs {
        let Some(&gi) = id2idx.get(&rq.gold) else {
            missing_gold += 1;
            continue;
        };
        let mut gold = HashSet::new();
        gold.insert(gi);
        if let Some(gv) = items[gi].vec.as_deref() {
            for (j, it) in items.iter().enumerate() {
                if j != gi {
                    if let Some(v) = it.vec.as_deref() {
                        if cos(gv, v) >= DUP {
                            gold.insert(j);
                        }
                    }
                }
            }
        }
        expand_total += gold.len() - 1;
        let qvec = emb.embed_one(&rq.query)?;
        battery.push(Q {
            category: if rq.category.is_empty() { "?".into() } else { rq.category.clone() },
            qlower: rq.query.to_lowercase(),
            qvec,
            gold_seed: gi,
            gold_expanded: gold,
        });
    }
    let n = battery.len();
    println!("=== 2.0-native battery: {} queries ({} skipped, gold not a live fact) ===", n, missing_gold);
    println!("battery file: {}", battery_path.display());
    println!("near-duplicate gold expansion: {:.2} extra facts credited per query on average (cosine >= {DUP})", expand_total as f64 / n.max(1) as f64);

    // Per-category counts.
    let mut by_cat: HashMap<String, usize> = HashMap::new();
    for q in &battery {
        *by_cat.entry(q.category.clone()).or_default() += 1;
    }
    let mut cats: Vec<&String> = by_cat.keys().collect();
    cats.sort();
    print!("categories:");
    for c in &cats {
        print!("  {}={}", c, by_cat[*c]);
    }
    println!();

    // Literal-hit order, precomputed ONCE per query (not once per grid cell
    // below): the shared ranking core is called here just once per query and
    // reused everywhere below.
    let battery_lit: Vec<Vec<usize>> = battery
        .iter()
        .map(|q| {
            let idx = literal_filter(&items, &q.qlower);
            literal_order(&vectors, &texts, &id2idx, &ids, &idx, &q.qlower, &q.qvec)
        })
        .collect();

    // How many battery queries produce a literal hit at all - the finding
    // behind the A2/A3 BM25 revert (see this file's own doc comment): 0/199
    // on this battery, so literal-hit RANKING (by any scheme) never has more
    // than one candidate to reorder.
    let lit_counts = (
        battery_lit.iter().filter(|l| l.is_empty()).count(),
        battery_lit.iter().filter(|l| l.len() == 1).count(),
        battery_lit.iter().filter(|l| l.len() >= 2).count(),
    );
    println!(
        "literal hits per query: {} with 0, {} with 1, {} with >=2 (out of {} - see this file's doc comment on the A2/A3 revert)",
        lit_counts.0, lit_counts.1, lit_counts.2, n
    );

    // ---- Strict vs expanded recall at the LIVE gate.
    let seed_set = |q: &Q| -> HashSet<usize> {
        let mut s = HashSet::new();
        s.insert(q.gold_seed);
        s
    };
    let mut strict = [0usize; 3];
    let mut expanded = [0usize; 3];
    for q in &battery {
        let ranked = rank_live(&vectors, &texts, &id2idx, &ids, &items, &q.qlower, &q.qvec, live_floor, CAP);
        let ss = seed_set(q);
        for (slot, k) in [(0usize, 1usize), (1, 3), (2, 5)] {
            if hit_at(&ranked, k, &ss) {
                strict[slot] += 1;
            }
            if hit_at(&ranked, k, &q.gold_expanded) {
                expanded[slot] += 1;
            }
        }
    }
    println!("\n=== recall at the LIVE gate (floor {live_floor}), n={} ===", n);
    println!("             recall@1        recall@3        recall@5");
    println!(
        "strict    | {:3}/{:<3} {:4.0}% | {:3}/{:<3} {:4.0}% | {:3}/{:<3} {:4.0}%",
        strict[0], n, pct(strict[0], n), strict[1], n, pct(strict[1], n), strict[2], n, pct(strict[2], n)
    );
    println!(
        "expanded  | {:3}/{:<3} {:4.0}% | {:3}/{:<3} {:4.0}% | {:3}/{:<3} {:4.0}%",
        expanded[0], n, pct(expanded[0], n), expanded[1], n, pct(expanded[1], n), expanded[2], n, pct(expanded[2], n)
    );

    // Per-category recall@5 (expanded gold) - the number the gate cares about
    // for the "report" category specifically.
    let mut cat_n: HashMap<&str, usize> = HashMap::new();
    let mut cat_hit5: HashMap<&str, usize> = HashMap::new();
    for q in &battery {
        let ranked = rank_live(&vectors, &texts, &id2idx, &ids, &items, &q.qlower, &q.qvec, live_floor, CAP);
        *cat_n.entry(q.category.as_str()).or_default() += 1;
        if hit_at(&ranked, 5, &q.gold_expanded) {
            *cat_hit5.entry(q.category.as_str()).or_default() += 1;
        }
    }
    print!("per-category recall@5 (expanded):");
    for c in &cats {
        let n_c = cat_n.get(c.as_str()).copied().unwrap_or(0);
        let h_c = cat_hit5.get(c.as_str()).copied().unwrap_or(0);
        print!("  {}={:.0}%({}/{})", c, pct(h_c, n_c), h_c, n_c);
    }
    println!();

    // Optional: dump each battery query's top-5 (at the live gate) for a blind
    // judge to score ANSWER-LEVEL recall - whether a CORRECT answer (not only
    // the exact seed fact) appears at each rank. The misses show many "misses"
    // are a sibling fact that answers just as well, so exact-id recall is a
    // conservative floor; this dump lets that be quantified.
    if let Ok(dump_path) = std::env::var("RECALL2_BATTERY_DUMP") {
        let mut dump: Vec<Value> = Vec::new();
        for q in &battery {
            let ranked = rank_live(&vectors, &texts, &id2idx, &ids, &items, &q.qlower, &q.qvec, live_floor, CAP);
            let top: Vec<Value> = ranked
                .iter()
                .take(5)
                .enumerate()
                .map(|(r, &i)| {
                    let score = items[i].vec.as_deref().map(|v| cos(&q.qvec, v)).unwrap_or(0.0);
                    serde_json::json!({
                        "rank": r + 1,
                        "id": ids[i],
                        "score": score,
                        "text": raw[i].1,
                        "is_seed": q.gold_expanded.contains(&i),
                    })
                })
                .collect();
            dump.push(serde_json::json!({
                "category": q.category,
                "query": q.qlower,
                "gold_id": ids[q.gold_seed],
                "gold_text": raw[q.gold_seed].1,
                "top": top,
            }));
        }
        std::fs::write(&dump_path, serde_json::to_string_pretty(&dump)?)?;
        println!("battery top-5 dump written to {}", dump_path);
    }

    // ---- Load the noise set once (queries with no fact answer).
    let noise_path = std::env::var("RECALL2_NOISE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| eval_dir().join("noise").join("noise_queries.json"));
    // (raw query, lowercased query, embedding)
    let noise_vecs: Vec<(String, String, Vec<f32>)> = match std::fs::File::open(&noise_path) {
        Ok(f) => {
            let noise: Vec<Value> = serde_json::from_reader(f)?;
            let mut v = Vec::new();
            for q in &noise {
                if let Some(query) = q.get("query").and_then(|x| x.as_str()) {
                    v.push((query.to_string(), query.to_lowercase(), emb.embed_one(query)?));
                }
            }
            v
        }
        Err(_) => Vec::new(),
    };
    println!("\nnoise set: {} queries loaded ({})", noise_vecs.len(), noise_path.display());

    let noise_lit: Vec<Vec<usize>> = noise_vecs
        .iter()
        .map(|(_, ql, qv)| {
            let idx = literal_filter(&items, ql);
            literal_order(&vectors, &texts, &id2idx, &ids, &idx, ql, qv)
        })
        .collect();

    // Optional: dump each noise query's top-5 retrieved items (ungated) for a
    // blind judge to confirm the query is truly unanswered by the store.
    if let Ok(dump_path) = std::env::var("RECALL2_NOISE_DUMP") {
        let mut dump: Vec<Value> = Vec::new();
        for (ni, (nraw, _ql, qv)) in noise_vecs.iter().enumerate() {
            let lit = &noise_lit[ni];
            let lit_set: HashSet<usize> = lit.iter().copied().collect();
            let sem = semantic_scored(&items, qv, &lit_set);
            let order: Vec<usize> = lit.iter().copied().chain(sem.iter().map(|(_, i)| *i)).collect();
            let top: Vec<Value> = order
                .iter()
                .take(5)
                .map(|&i| {
                    let score = items[i].vec.as_deref().map(|v| cos(qv, v)).unwrap_or(0.0);
                    serde_json::json!({ "id": ids[i], "score": score, "text": raw[i].1 })
                })
                .collect();
            dump.push(serde_json::json!({ "query": nraw, "top": top }));
        }
        std::fs::write(&dump_path, serde_json::to_string_pretty(&dump)?)?;
        println!("noise top-5 dump written to {}", dump_path);
    }

    // Noise top-semantic-score distribution: informs where open_floor should sit.
    if !noise_vecs.is_empty() {
        let mut tops: Vec<f32> = noise_vecs
            .iter()
            .enumerate()
            .map(|(ni, (_, _, qv))| {
                let lit_set: HashSet<usize> = noise_lit[ni].iter().copied().collect();
                semantic_scored(&items, qv, &lit_set).first().map(|(s, _)| *s).unwrap_or(0.0)
            })
            .collect();
        tops.sort_by(|a, b| a.total_cmp(b));
        let q = |p: f64| tops[((p * (tops.len() - 1) as f64).round() as usize).min(tops.len() - 1)];
        println!(
            "noise top-semantic-score: min {:.3}  p25 {:.3}  p50 {:.3}  p75 {:.3}  p90 {:.3}  max {:.3}",
            tops[0], q(0.25), q(0.50), q(0.75), q(0.90), tops[tops.len() - 1]
        );
    }

    // The HONEST floor number, straight from the shared, live-identical
    // ranking function - NOT the "silence-gate grid" below. That grid holds
    // its per-item retention floor fixed at 0.45 (`gated_extras(&sem, open,
    // band, 0.45)` - the THIRD argument, never `open`) regardless of which
    // `open` value is being swept, so its rows only equal a true uniform
    // MIN_SIMILARITY sweep at the one row where `open` already IS 0.45; it
    // is a peak/gate-open exploration (does the TOP score clear `open`?),
    // not a floor sweep, and its "0.50/0.55/0.60" rows read HIGHER than a
    // real uniform floor of that value would (they still admit anything
    // down to 0.45 once the gate opens). This block instead calls
    // `rank_live` - the exact live code path - at `live_floor`, so it is the
    // number that actually answers A4's question.
    if !noise_vecs.is_empty() {
        let mut noise_res_live = 0usize;
        for (_, ql, qv) in &noise_vecs {
            let ranked = rank_live(&vectors, &texts, &id2idx, &ids, &items, ql, qv, live_floor, CAP);
            noise_res_live += ranked.len();
        }
        println!(
            "mean #results on noise at the LIVE gate (floor {live_floor}, uniform, shared code): {:.2}",
            noise_res_live as f64 / noise_vecs.len().max(1) as f64
        );
    }

    // ---- The gate grid: battery recall retention vs noise padding. The
    // `band` knob is EXPLORATORY ONLY (see this file's own doc comment) - no
    // live counterpart - so its cells use the local `gated_extras`. Literal
    // order is always the shared, precomputed `battery_lit`/`noise_lit`.
    let open_floors = [0.45f32, 0.50, 0.55, 0.60];
    let bands = [(f32::INFINITY, "off"), (0.15, "0.15"), (0.10, "0.10"), (0.07, "0.07")];
    println!("\n=== silence-gate grid (base_floor FIXED at 0.45 regardless of `open` - a peak/gate exploration, NOT a floor sweep; see 'mean #results on noise at the LIVE gate' above for the true floor number) ===");
    println!("open  band | R@1  R@3  R@5  (expanded) | mean #res battery | mean #res NOISE");
    println!("-----------|----------------------------|-------------------|----------------");
    for &open in &open_floors {
        for &(band, blabel) in &bands {
            let (mut r, mut res_b) = ([0usize; 3], 0usize);
            for (qi, q) in battery.iter().enumerate() {
                let lit = &battery_lit[qi];
                let lit_set: HashSet<usize> = lit.iter().copied().collect();
                let sem = semantic_scored(&items, &q.qvec, &lit_set);
                let extras = gated_extras(&sem, open, band, 0.45);
                let ranked: Vec<usize> = lit.iter().copied().chain(extras).collect();
                res_b += ranked.len();
                for (slot, k) in [(0usize, 1usize), (1, 3), (2, 5)] {
                    if hit_at(&ranked, k, &q.gold_expanded) {
                        r[slot] += 1;
                    }
                }
            }
            let mut res_noise = 0usize;
            for (ni, _) in noise_vecs.iter().enumerate() {
                let lit = &noise_lit[ni];
                let lit_set: HashSet<usize> = lit.iter().copied().collect();
                let sem = semantic_scored(&items, &noise_vecs[ni].2, &lit_set);
                res_noise += lit.len() + gated_extras(&sem, open, band, 0.45).len();
            }
            let tag = if (open - live_floor).abs() < 1e-6 && !band.is_finite() { " <- live" } else { "" };
            println!(
                "{:.2}  {:>4} | {:4.0}%{:4.0}%{:4.0}%              | {:8.2}          | {:8.2}{}",
                open,
                blabel,
                pct(r[0], n),
                pct(r[1], n),
                pct(r[2], n),
                res_b as f64 / n.max(1) as f64,
                res_noise as f64 / noise_vecs.len().max(1) as f64,
                tag
            );
        }
    }

    // ---- The MARGIN-vs-field gate sweep (base_floor 0.45, band off): does a
    // peak/plateau margin separate real answers from no-answer noise better
    // than a bare floor? ref_rank = the field rank the top is compared against.
    // EXPLORATORY ONLY (see this file's own doc comment) - no live counterpart.
    println!("\n=== margin-vs-field silence gate (base_floor 0.45, band off) ===");
    println!("ref  margin | R@1  R@3  R@5  (expanded) | mean #res battery | mean #res NOISE");
    println!("------------|----------------------------|-------------------|----------------");
    for &ref_rank in &[3usize, 5usize] {
        for &margin in &[0.03f32, 0.05, 0.08, 0.10, 0.12] {
            let (mut r, mut res_b) = ([0usize; 3], 0usize);
            for (qi, q) in battery.iter().enumerate() {
                let lit = &battery_lit[qi];
                let lit_set: HashSet<usize> = lit.iter().copied().collect();
                let sem = semantic_scored(&items, &q.qvec, &lit_set);
                let extras = gated_extras_margin(&sem, margin, ref_rank, f32::INFINITY, 0.45);
                let ranked: Vec<usize> = lit.iter().copied().chain(extras).collect();
                res_b += ranked.len();
                for (slot, k) in [(0usize, 1usize), (1, 3), (2, 5)] {
                    if hit_at(&ranked, k, &q.gold_expanded) {
                        r[slot] += 1;
                    }
                }
            }
            let mut res_noise = 0usize;
            for (ni, _) in noise_vecs.iter().enumerate() {
                let lit = &noise_lit[ni];
                let lit_set: HashSet<usize> = lit.iter().copied().collect();
                let sem = semantic_scored(&items, &noise_vecs[ni].2, &lit_set);
                res_noise += lit.len() + gated_extras_margin(&sem, margin, ref_rank, f32::INFINITY, 0.45).len();
            }
            println!(
                "{:3}  {:.2}   | {:4.0}%{:4.0}%{:4.0}%              | {:8.2}          | {:8.2}",
                ref_rank,
                margin,
                pct(r[0], n),
                pct(r[1], n),
                pct(r[2], n),
                res_b as f64 / n.max(1) as f64,
                res_noise as f64 / noise_vecs.len().max(1) as f64
            );
        }
    }

    // ---- Diagnostic: recall@5 misses at the live gate (are they real, or a
    // gold-cluster gap?). Prints the seed gold text and the top-3 retrieved.
    if std::env::var("RECALL2_DIAG").is_ok() {
        println!("\n=== recall@5 MISSES at the live gate (expanded gold) ===");
        let mut shown = 0;
        for q in &battery {
            let ranked = rank_live(&vectors, &texts, &id2idx, &ids, &items, &q.qlower, &q.qvec, live_floor, CAP);
            if hit_at(&ranked, 5, &q.gold_expanded) {
                continue;
            }
            shown += 1;
            if shown > 25 {
                break;
            }
            println!("\n[{}] Q: {}", q.category, q.qlower);
            let g: String = raw[q.gold_seed].1.chars().take(120).collect();
            println!("    GOLD[{}]: {}", ids[q.gold_seed], g);
            for (r, &i) in ranked.iter().take(3).enumerate() {
                let t: String = raw[i].1.chars().take(100).collect();
                println!("    top{}: {}", r + 1, t);
            }
        }
    }

    Ok(())
}
