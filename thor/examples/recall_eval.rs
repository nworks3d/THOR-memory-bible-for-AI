//! Honest production measurement of `recall::recall_fused` (feature `semantic`).
//!
//! Runs the REAL recall path - not a python re-implementation - over a private
//! battery, fused vs bm25. The printed table is recall@5 per category with @3/@5
//! totals; @1 and the gold's rank go to the `--out` dump, because a hit rate at
//! rank 1 is too coarse to read off a table but is where the arms differ most.
//! The bm25
//! baseline goes through the SAME `recall_fused` with an all-zero query vector, so
//! the cosine term vanishes and the order is pure bm25 (identical candidate pool,
//! head projection and dedup) - isolating exactly what fusion adds.
//!
//! A hit is the gold's ENTITY being served (the courier injects an entity's current
//! head, so that IS a correct recall even if the gold seq was later revised), OR a
//! served body carrying the gold's content after it moved elsewhere. The two legs
//! are reported separately: they answer different questions and merging them hides
//! which one is working.
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
//!
//! Freeze gold texts at the body of their SOURCE seq, not at the entity's current
//! head. A head is mutable, and re-chunking has been observed to replace a
//! 93-key-term chunk with a 12-key-term one-line comment; the shortest gold in a
//! head-frozen 460-item corpus had 3 key terms against 15 for the same corpus
//! frozen at source, and one of them was satisfied by 2256 of 5433 unrelated live
//! heads. Nothing in this file can detect that - it just quietly inflates
//! every arm.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
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

/// How deep to look when recording where the gold landed.
///
/// A binary hit@5 throws away almost everything an arm did: moving a gold from
/// rank 40 to rank 6 scores exactly the same as not moving it at all. That is why
/// small strata cannot be decided - McNemar needs at least 8 items where the arms
/// DISAGREE, and on 53 memory facts they disagree about 4 times. The rank is the
/// same measurement without the information thrown away, and a paired rank test
/// uses every item instead of only the discordant ones.
const RANK_LIMIT: usize = 50;
/// Share of live heads a term may appear in before it stops being evidence.
const MAX_DF: f64 = 0.10;
/// Share of a gold's key terms a body must carry to count as the same content.
const CONTENT_THRESHOLD: f64 = 0.60;

/// Lowercase alphanumeric tokens >= 4 chars, deduped.
fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 4)
        .map(|t| t.to_lowercase())
        .collect()
}

/// Key terms of a gold text: tokens that are actually evidence for THIS gold.
///
/// The earlier recipe took every token >= 4 chars and matched it as a SUBSTRING,
/// with no frequency filter. Both halves of that leak. Substring matching makes
/// "test" fire on "latest"; the missing frequency filter makes "repo", "file" and
/// "chunk" - present in over 90% of stored bodies - count as evidence, so a body
/// could clear the bar on boilerplate alone. Measured consequence on this store:
/// one gold was satisfied by 2256 of 5433 unrelated live heads.
///
/// Both filters are needed and neither subsumes the other. I assumed the frequency
/// cut alone would do, because function words are everywhere; measured on this
/// store, 19 of the shipped stopwords sit BELOW 10% document frequency ("what"
/// 9.1%, "which" 6.9%, "zijn" 6.2%) and would still have counted as evidence.
/// Conversely "repo" at 89% is no stopword and only the frequency cut catches it.
fn key_terms(text: &str, df: &HashMap<String, usize>, heads: usize) -> Vec<String> {
    let cap = (heads as f64 * MAX_DF) as usize;
    let mut v: Vec<String> = tokens(text)
        .into_iter()
        .filter(|t| !thor::recall::STOPWORDS.contains(&t.as_str()))
        .filter(|t| df.get(t).copied().unwrap_or(0) <= cap)
        .collect();
    v.sort();
    v
}

/// The source file behind a chunk entity id (`<project>:<path>#<n>`), if any.
///
/// Chunks of one file share most of their vocabulary, so a sibling chunk clears
/// any content threshold without carrying the answer: 37% of a chunk gold's
/// satisfiers were measured to be other chunks of its own file. A sibling is
/// neither the gold nor an honest miss, so it is never allowed to score.
fn chunk_file(entity: &str) -> Option<&str> {
    entity.rsplit_once('#').map(|(file, _)| file)
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
    // Content-addressed golds (optional sidecar): id -> frozen gold TEXT. A hit
    // counts if its entity matches OR its body carries enough of the gold's key
    // terms - the metabolism-proof measure; ids stay the continuity measure. The
    // terms themselves need the store's document frequencies, so they are derived
    // once the events are loaded, below.
    let gold_text: HashMap<String, String> = match std::fs::File::open(&gc_path) {
        Ok(f) => {
            let raw: HashMap<String, Value> = serde_json::from_reader(f)?;
            raw.into_iter()
                .filter_map(|(k, v)| {
                    v.get("gold_text").and_then(|t| t.as_str()).map(|t| (k, t.to_string()))
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

    // Document frequency over the LIVE heads - the bodies a recall can actually
    // serve. Retracted entities are excluded: a term is not common because it
    // survives in text nobody will ever be shown.
    let mut head_of: HashMap<&str, &thor::event_store::Event> = HashMap::new();
    for e in &events {
        head_of.insert(e.entity_id.as_str(), e);
    }
    let live: Vec<&thor::event_store::Event> = head_of
        .values()
        .copied()
        .filter(|e| !matches!(e.kind, thor::event_store::EventKind::FactRetracted))
        .collect();
    let mut df: HashMap<String, usize> = HashMap::new();
    for e in &live {
        for t in tokens(&e.body) {
            *df.entry(t).or_insert(0) += 1;
        }
    }
    let gold_terms: HashMap<String, Vec<String>> =
        gold_text.iter().map(|(k, t)| (k.clone(), key_terms(t, &df, live.len()))).collect();
    let degenerate = gold_terms.values().filter(|t| t.len() < 8).count();
    eprintln!(
        "battery: {} queries from {}, {} golds, {} frozen gold texts",
        queries.len(),
        q_path.display(),
        golds.len(),
        gold_terms.len()
    );
    eprintln!(
        "scoring: {} live heads, terms above df {:.0}% dropped, content threshold {:.0}%, \
         {} gold(s) left with fewer than 8 key terms",
        live.len(),
        MAX_DF * 100.0,
        CONTENT_THRESHOLD * 100.0,
        degenerate
    );

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
        let sibling_of = chunk_file(&gent).map(|f| f.to_string());
        let gold_is_chunk = sibling_of.is_some();
        // Scores the two legs separately. The entity leg says "the courier would
        // have injected this entity's head"; the content leg says "some served
        // body carries the answer, wherever it now lives". They disagree often
        // enough that one merged number hides which one is doing the work.
        let score = move |hits: &[thor::recall::RecallHit], k: usize| -> (bool, bool) {
            let (mut ent, mut cont) = (false, false);
            for h in hits.iter().take(k) {
                if h.entity_id == gent {
                    ent = true;
                    continue;
                }
                // A sibling chunk of the gold's own file shares its vocabulary
                // without carrying the answer. Never let it score either way.
                if let (Some(want), Some(got)) = (&sibling_of, chunk_file(&h.entity_id)) {
                    if want == got {
                        continue;
                    }
                }
                if terms.is_empty() {
                    continue;
                }
                let body = tokens(&h.body);
                let got = terms.iter().filter(|t| body.contains(t.as_str())).count();
                if got as f64 / terms.len() as f64 >= CONTENT_THRESHOLD {
                    cont = true;
                }
            }
            (ent, cont)
        };
        let count = |hits: &[thor::recall::RecallHit], k: usize| -> bool {
            let (e, c) = score(hits, k);
            e || c
        };
        // 1-based rank of the first hit, or None when the gold is nowhere in the
        // first RANK_LIMIT. Censored items are kept and handled by the analysis;
        // dropping them would bias every arm toward whatever it already finds.
        let rank_of = |hits: &[thor::recall::RecallHit]| -> Option<usize> {
            (1..=hits.len().min(RANK_LIMIT)).find(|&k| count(hits, k))
        };

        // bm25 baseline: a zero query vector -> cosine 0 -> pure bm25 order.
        let base = recall_fused_scoped(&store, query, &zero, &vecs, RANK_LIMIT, 1.0,
            &thor::recall::RecallScope::everything(), true, symbols.as_ref())?;
        // @1 as well as @3/@5: the measured discordance between arms is several
        // times larger at rank 1 than at rank 5, so a report that starts at @3
        // looks at the metric where the arms agree most.
        let arm_json = |hits: &[thor::recall::RecallHit]| -> Value {
            let (e5, c5) = score(hits, 5);
            serde_json::json!({
                "at1": count(hits, 1),
                "at3": count(hits, 3),
                "at5": e5 || c5,
                "entity_at5": e5,
                "content_at5": c5,
                "rank": rank_of(hits),
                "served": hits.len(),
            })
        };
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
        item_arms.insert("bm25".into(), arm_json(&base));

        // fused, one accumulator per lambda.
        for (i, &lam) in lambdas.iter().enumerate() {
            let hits = recall_fused_scoped(&store, query, &qvec, &vecs, RANK_LIMIT, lam,
                &thor::recall::RecallScope::everything(), true, symbols.as_ref())?;
            let (h3, h5) = (count(&hits, 3), count(&hits, 5));
            item_arms.insert(format!("fused_L{lam}"), arm_json(&hits));
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
            "gold_entity_is_chunk": gold_is_chunk,
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

/// The scoring rule is the measurement. Every recall figure this project
/// publishes is whatever these four functions say it is, so a silent change here
/// moves published numbers with nothing to catch it - and the looseness they
/// replaced did exactly that for months before it was found.
#[cfg(test)]
mod tests {
    use super::*;

    /// Document frequencies for `heads` documents, from (term, count) pairs.
    fn df_of(pairs: &[(&str, usize)]) -> HashMap<String, usize> {
        pairs.iter().map(|(t, n)| (t.to_string(), *n)).collect()
    }

    #[test]
    fn tokens_lowercases_dedupes_and_drops_short_words() {
        let t = tokens("Fsck FSCK fsck an is of Rebuild-FTS 42 seventeen");
        assert!(t.contains("fsck"), "case is folded and the three collapse to one");
        assert!(t.contains("rebuild") && t.contains("seventeen"));
        for short in ["an", "is", "of", "42"] {
            assert!(!t.contains(short), "{short} is under 4 chars and must not be a term");
        }
    }

    #[test]
    fn key_terms_drops_stopwords_that_the_frequency_cut_would_keep() {
        // "which" sits below 10% document frequency on the real store, so the
        // frequency cut alone leaves it in. This is the case that made me put
        // both filters back after assuming one would do.
        let df = df_of(&[("which", 5), ("rebuild", 5)]);
        let terms = key_terms("which rebuild", &df, 1000);
        assert_eq!(terms, vec!["rebuild".to_string()], "a rare stopword is still a stopword");
    }

    #[test]
    fn key_terms_drops_common_words_that_are_not_stopwords() {
        // "repo" is no stopword but appears in ~90% of stored bodies, so it is
        // evidence for nothing in particular.
        let df = df_of(&[("repo", 900), ("directional", 3)]);
        let terms = key_terms("repo directional", &df, 1000);
        assert_eq!(terms, vec!["directional".to_string()], "the frequency cut catches it");
    }

    #[test]
    fn key_terms_cut_is_inclusive_at_the_boundary() {
        // cap = 10% of 1000 = 100. A term AT the cap is kept; one above is not.
        let df = df_of(&[("atcap", 100), ("overcap", 101)]);
        let terms = key_terms("atcap overcap", &df, 1000);
        assert_eq!(terms, vec!["atcap".to_string()]);
    }

    #[test]
    fn key_terms_of_an_unseen_word_treats_it_as_maximally_rare() {
        // A gold frozen at its source seq can contain words no live head has any
        // more. Absent from the df map must mean "rare", never "unknown, drop it".
        let terms = key_terms("vanished", &df_of(&[]), 1000);
        assert_eq!(terms, vec!["vanished".to_string()]);
    }

    #[test]
    fn chunk_file_identifies_siblings_and_leaves_plain_facts_alone() {
        assert_eq!(chunk_file("proj:src/recall.rs#12"), Some("proj:src/recall.rs"));
        assert_eq!(
            chunk_file("proj:src/recall.rs#12"),
            chunk_file("proj:src/recall.rs#3"),
            "two chunks of one file must compare equal, that is what suppresses them"
        );
        assert_ne!(chunk_file("proj:src/recall.rs#1"), chunk_file("proj:src/cli.rs#1"));
        assert_eq!(chunk_file("proj:mem-abc"), None, "a hand-written fact has no siblings");
    }

    #[test]
    fn a_content_match_needs_more_than_half_the_terms() {
        // The threshold moved from 50% to 60% after the loose rule was measured
        // to fire on unrelated bodies; pin it so it cannot drift back silently.
        assert!(CONTENT_THRESHOLD > 0.5, "50% was measured too loose");
        let terms = key_terms("alpha bravo charlie delta echo", &df_of(&[]), 1000);
        assert_eq!(terms.len(), 5);
        let carried = |body: &str| {
            let b = tokens(body);
            terms.iter().filter(|t| b.contains(t.as_str())).count() as f64 / terms.len() as f64
        };
        // 3 of 5 is exactly 60%, and the comparison is >=, so the boundary counts
        // as a match. Pinned explicitly because it is the kind of off-by-one that
        // moves every published figure by a question or two without any error.
        assert!(carried("alpha bravo") < CONTENT_THRESHOLD, "2 of 5 is not a match");
        assert!(carried("alpha bravo charlie") >= CONTENT_THRESHOLD, "3 of 5 sits ON the bar");
        assert!(carried("bravo charlie delta echo") >= CONTENT_THRESHOLD, "4 of 5 clears it");
    }

    #[test]
    fn terms_match_whole_tokens_not_substrings() {
        // The old rule matched substrings, so "test" scored against "latest".
        let terms = key_terms("test", &df_of(&[]), 1000);
        assert_eq!(terms, vec!["test".to_string()]);
        assert!(!tokens("the latest greatest").contains("test"), "must not fire on 'latest'");
        assert!(tokens("a test case").contains("test"));
    }
}
