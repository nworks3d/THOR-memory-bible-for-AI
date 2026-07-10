//! `thor consolidate` - the metabolism pass: surface what the store should
//! digest. Three passes over the live memory heads (repo chunks are managed by
//! ingest and excluded; diverged entities need a `resolve` first and are never
//! guessed about):
//!
//!   1. duplicates - live entities whose normalized body prefix
//!      (`recall::dedup_prefix`, the SAME key the remember/import gates refuse
//!      on) collides: the legacy twins that predate those gates. The only
//!      mechanically-applied pass (`--apply-dedup`).
//!   2. decay candidates - untyped notes with non-positive usage strength
//!      (crate::strength: recency-weighted echoes + capped reads - noise
//!      marks) and long inactive. The log has no wall clock (timestamps are
//!      not canonical content), so age = events behind the tip. Candidates
//!      ONLY - an agent confirms each via retract.
//!   3. same-topic clusters - groups likely about one subject (shared prefix
//!      band, plus cosine neighbors when the vectors sidecar is readable), as
//!      input for agent judgement: contradiction or distillation via
//!      revise/supersede/resolve. This is clustering, NOT a contradiction
//!      detector - a cluster is a lead, not a verdict.
//!
//! Lossless by construction: the only write this module can do is
//! fact_retracted events; nothing is ever deleted from the log.

use crate::event_store::{Event, EventKind, EventStore};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Default decay age floor, in EVENTS behind the log tip.
pub const DEFAULT_MIN_AGE_EVENTS: i64 = 2000;

/// Same-topic prefix band: normalized bodies sharing this many leading chars
/// are about the same thing without being byte-twins (those collide on the
/// full dedup prefix and land in the duplicates pass instead).
const TOPIC_BAND_CHARS: usize = 40;

/// Cosine floor for the semantic same-topic band (paraphrase twins the lexical
/// band can never catch). Conservative start; tune against live dry-runs.
#[cfg(feature = "semantic")]
const COSINE_BAND: f32 = 0.86;

/// A cluster bigger than this is not an actionable contradiction/distillation
/// lead - it is a batch/template family (sequential imports) or a union-find
/// chain. Skipped, but COUNTED in the report: silent truncation would read as
/// "reviewed everything" when it was not.
const MAX_CLUSTER_MEMBERS: usize = 6;

/// One retract target, citing the exact head rev the report saw: apply passes
/// it as parent_rev, so ANY concurrent head change (a revise landing between
/// report and apply) comes back as a CAS conflict instead of a wrong retract.
pub struct RetractTarget {
    pub entity_id: String,
    pub parent_rev: String,
}

pub struct DupGroup {
    pub keep: String,
    pub retract: Vec<RetractTarget>,
    pub first_line: String,
}

pub struct DecayCandidate {
    pub entity_id: String,
    pub first_line: String,
    pub events_behind_tip: i64,
}

pub struct Cluster {
    pub reason: String,
    pub members: Vec<String>,
}

#[derive(Default)]
pub struct Report {
    pub dups: Vec<DupGroup>,
    pub decay: Vec<DecayCandidate>,
    pub clusters: Vec<Cluster>,
    /// Clusters dropped for being over MAX_CLUSTER_MEMBERS (batch families,
    /// union-find chains) - counted so the cap is never silent.
    pub broad_clusters_skipped: usize,
    /// false = the cosine pass contributed nothing (non-semantic build, or the
    /// vectors sidecar was absent/unreadable) - the report is lexical-only.
    pub cosine_ran: bool,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.dups.is_empty() && self.decay.is_empty() && self.clusters.is_empty()
    }
}

pub struct Options {
    pub min_age_events: i64,
}

/// One live (non-retracted), single-headed memory head.
struct LiveHead {
    entity_id: String,
    head_rev: String,
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    head_seq: i64,
    create_seq: i64,
    last_seq: i64,
    /// Unified usage strength (crate::strength: recency-weighted echoes +
    /// capped reads - noise marks). <= 0 = never useful on balance.
    strength: f64,
    prefix: String,
    first_line: String,
    typed: bool,
    /// Footer carries a source-store reference: this head arrived via the
    /// one-time source seeding. Historical marker only - the stores are
    /// isolated (imports are guarded by SEEDED.flag), so these heads live and
    /// die in THOR like any native fact.
    imported: bool,
    /// Another live head's body cites this entity id (e.g. an "m:01K..." or
    /// "mimir:01K..." link in prose). Retracting it would break that
    /// reference for recall, so decay never suggests it - the same rationale
    /// the dup keep-priority uses to prefer the seeded copy.
    referenced: bool,
    pinned: bool,
}

fn first_line(body: &str) -> String {
    body.trim().lines().next().unwrap_or("").chars().take(90).collect()
}

fn live_memory_heads(events: &[Event], pins: &[String]) -> Vec<LiveHead> {
    let heads = crate::cas::compute_head_sets(events);
    let by_hash: HashMap<&str, &Event> =
        events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
    let mut create_seq: HashMap<&str, i64> = HashMap::new();
    let mut last_seq: HashMap<&str, i64> = HashMap::new();
    for e in events {
        create_seq.entry(&e.entity_id).or_insert(e.seq);
        // The decay clock measures CONTENT/engagement age. A reproject is pure
        // scope administration (head-neutral per the cas fold) - a bulk
        // backfill must not reset the staleness of every touched note.
        if !matches!(e.kind, EventKind::FactReprojected) {
            let l = last_seq.entry(&e.entity_id).or_insert(e.seq);
            *l = (*l).max(e.seq);
        }
    }
    let mut out = Vec::new();
    let mut bodies: Vec<String> = Vec::new(); // aligned with `out` until the sort
    for (id, hs) in &heads {
        if crate::repo::is_chunk_id(id) || hs.is_diverged || hs.heads.len() != 1 {
            continue;
        }
        let rev = hs.heads.iter().next().expect("single head checked above");
        let Some(head) = by_hash.get(rev.as_str()) else { continue };
        if matches!(head.kind, EventKind::FactRetracted) {
            continue;
        }
        out.push(LiveHead {
            entity_id: id.clone(),
            head_rev: head.this_hash.clone(),
            head_seq: head.seq,
            create_seq: *create_seq.get(id.as_str()).unwrap_or(&head.seq),
            last_seq: *last_seq.get(id.as_str()).unwrap_or(&head.seq),
            strength: 0.0, // filled by build_report via crate::strength
            prefix: crate::recall::dedup_prefix(&head.body),
            first_line: first_line(&head.body),
            typed: crate::footer::fact_type(&head.body).is_some(),
            imported: crate::footer::has_source_ref(&head.body),
            referenced: false, // filled below, once every body is known
            pinned: pins.iter().any(|p| p == id),
        });
        bodies.push(head.body.clone());
    }
    // Mark heads whose id is cited inside ANOTHER live head's body (a fact's
    // own footer cites its own id - that self-reference does not count).
    for i in 0..out.len() {
        let id = out[i].entity_id.clone();
        out[i].referenced = bodies.iter().enumerate().any(|(j, b)| j != i && b.contains(&id));
    }
    out.sort_by_key(|h| h.create_seq);
    out
}

fn dup_groups(heads: &[LiveHead]) -> Vec<DupGroup> {
    let mut by_prefix: HashMap<&str, Vec<&LiveHead>> = HashMap::new();
    for h in heads {
        if h.prefix.is_empty() {
            continue;
        }
        by_prefix.entry(&h.prefix).or_default().push(h);
    }
    let mut out = Vec::new();
    for group in by_prefix.values() {
        if group.len() < 2 {
            continue;
        }
        // Keep-priority: pinned > seeded copy (its entity id IS the source id
        // that fact bodies cross-reference, e.g. "m:01K..." links - keeping it
        // preserves those references) > typed > proven-useful (positive
        // strength) > oldest. Typed/strength rank above age for the same
        // reason decay protects them: those signals say "this copy is the
        // curated one". A pinned twin is never a retract target.
        let keep = group
            .iter()
            .max_by_key(|h| {
                (h.pinned, h.imported, h.typed, h.strength > 0.0, std::cmp::Reverse(h.create_seq))
            })
            .expect("group.len() >= 2");
        let mut retract: Vec<RetractTarget> = group
            .iter()
            .filter(|h| h.entity_id != keep.entity_id && !h.pinned)
            .map(|h| RetractTarget {
                entity_id: h.entity_id.clone(),
                parent_rev: h.head_rev.clone(),
            })
            .collect();
        if retract.is_empty() {
            continue;
        }
        retract.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
        out.push(DupGroup {
            keep: keep.entity_id.clone(),
            retract,
            first_line: keep.first_line.clone(),
        });
    }
    out.sort_by(|a, b| a.keep.cmp(&b.keep));
    out
}

fn decay_candidates(
    heads: &[LiveHead],
    tip_seq: i64,
    min_age_events: i64,
) -> Vec<DecayCandidate> {
    let mut out: Vec<DecayCandidate> = heads
        .iter()
        .filter(|h| {
            // Seeded (imported) heads are NOT excluded: the stores are isolated
            // (one-time seeding, SEEDED.flag guards re-imports), so a stale
            // seeded note decays like any native one - nothing resurrects it.
            // A head cited by another live fact's body IS excluded: retracting
            // it would break that reference for recall.
            !h.typed
                && !h.pinned
                && !h.referenced
                // never useful on balance: no (recency-weighted) echo or read
                // outweighs its noise marks - the ONE strength concept
                && h.strength <= 0.0
                && tip_seq - h.last_seq >= min_age_events
        })
        .map(|h| DecayCandidate {
            entity_id: h.entity_id.clone(),
            first_line: h.first_line.clone(),
            events_behind_tip: tip_seq - h.last_seq,
        })
        .collect();
    // stalest first, id as tiebreak so the report is deterministic
    out.sort_by(|a, b| {
        b.events_behind_tip
            .cmp(&a.events_behind_tip)
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });
    out
}

fn prefix_band_clusters(heads: &[LiveHead]) -> (Vec<Cluster>, usize) {
    let mut by_band: HashMap<String, Vec<&LiveHead>> = HashMap::new();
    for h in heads {
        if h.prefix.chars().count() < TOPIC_BAND_CHARS {
            continue; // too short to band on reliably
        }
        let band: String = h.prefix.chars().take(TOPIC_BAND_CHARS).collect();
        by_band.entry(band).or_default().push(h);
    }
    let mut out = Vec::new();
    let mut skipped = 0;
    for group in by_band.values() {
        // At least two DISTINCT full prefixes: identical-prefix twins belong to
        // the duplicates pass, not here.
        let distinct: HashSet<&str> = group.iter().map(|h| h.prefix.as_str()).collect();
        if group.len() < 2 || distinct.len() < 2 {
            continue;
        }
        if group.len() > MAX_CLUSTER_MEMBERS {
            skipped += 1;
            continue;
        }
        let mut members: Vec<String> = group.iter().map(|h| h.entity_id.clone()).collect();
        members.sort();
        out.push(Cluster { reason: "prefix-band".to_string(), members });
    }
    out.sort_by(|a, b| a.members.cmp(&b.members));
    (out, skipped)
}

#[cfg(feature = "semantic")]
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for i in 0..n {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Union-find cosine pass over the head vectors already in the sidecar. `None`
/// = sidecar unavailable or untrusted (the caller reports lexical-only). Heads
/// whose vector is missing (sidecar behind the tip) are simply skipped -
/// fail-open.
#[cfg(feature = "semantic")]
fn cosine_clusters(
    db: &Path,
    heads: &[LiveHead],
    existing: &[Cluster],
) -> Option<(Vec<Cluster>, usize)> {
    let vpath = crate::vectors::default_vectors_path(db);
    // Never MATERIALIZE the sidecar from a report-only command: open() creates
    // an empty db for a missing path, and an empty sidecar is not a ran pass.
    if !vpath.exists() {
        return None;
    }
    let vs = crate::vectors::VectorStore::open(&vpath).ok()?;
    // Same convention as the courier and the embed daemon: a sidecar embedded
    // by a different model is stale until rebuilt - degrade, never trust.
    if vs.model_id().as_deref() != Some(crate::embed::MODEL_ID) {
        return None;
    }
    let seqs: Vec<i64> = heads.iter().map(|h| h.head_seq).collect();
    let vecs = vs.get_many(&seqs).ok()?;

    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut root = i;
        while parent[root] != root {
            root = parent[root];
        }
        let mut cur = i;
        while parent[cur] != root {
            let next = parent[cur];
            parent[cur] = root;
            cur = next;
        }
        root
    }

    let with_vec: Vec<usize> =
        (0..heads.len()).filter(|&i| vecs.contains_key(&heads[i].head_seq)).collect();
    let mut parent: Vec<usize> = (0..heads.len()).collect();
    for (pos, &a) in with_vec.iter().enumerate() {
        for &b in &with_vec[pos + 1..] {
            if heads[a].prefix == heads[b].prefix {
                continue; // byte-twin territory - the duplicates pass owns it
            }
            let (va, vb) = (&vecs[&heads[a].head_seq], &vecs[&heads[b].head_seq]);
            if cosine(va, vb) >= COSINE_BAND {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                if ra != rb {
                    parent[ra] = rb;
                }
            }
        }
    }

    let mut groups: HashMap<usize, Vec<String>> = HashMap::new();
    for &i in &with_vec {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(heads[i].entity_id.clone());
    }
    let known: HashSet<&[String]> = existing.iter().map(|c| c.members.as_slice()).collect();
    let mut out = Vec::new();
    let mut skipped = 0;
    for mut members in groups.into_values() {
        if members.len() < 2 {
            continue;
        }
        if members.len() > MAX_CLUSTER_MEMBERS {
            skipped += 1;
            continue;
        }
        members.sort();
        if known.contains(members.as_slice()) {
            continue; // the lexical band already reported exactly this set
        }
        out.push(Cluster { reason: format!("cosine>={COSINE_BAND}"), members });
    }
    out.sort_by(|a, b| a.members.cmp(&b.members));
    Some((out, skipped))
}

pub fn build_report(store: &EventStore, db: &Path, events: &[Event], opts: &Options) -> Report {
    let pins = crate::ledger::read_pins(db);
    let mut heads = live_memory_heads(events, &pins);
    // The unified usage strength (crate::strength), computed once for every
    // live head: decay eligibility and dup keep-priority read the same number
    // the courier's promotion does.
    let ids: Vec<String> = heads.iter().map(|h| h.entity_id.clone()).collect();
    let strengths = crate::strength::strength_for(store, db, &ids);
    for h in &mut heads {
        h.strength = strengths.get(&h.entity_id).copied().unwrap_or(0.0);
    }
    let tip_seq = events.iter().map(|e| e.seq).max().unwrap_or(0);

    let dups = dup_groups(&heads);
    let decay = decay_candidates(&heads, tip_seq, opts.min_age_events);
    #[allow(unused_mut)]
    let (mut clusters, mut broad_clusters_skipped) = prefix_band_clusters(&heads);
    #[allow(unused_mut)]
    let mut cosine_ran = false;
    #[cfg(feature = "semantic")]
    if let Some((cc, skipped)) = cosine_clusters(db, &heads, &clusters) {
        clusters.extend(cc);
        broad_clusters_skipped += skipped;
        cosine_ran = true;
    }
    Report { dups, decay, clusters, broad_clusters_skipped, cosine_ran }
}

#[derive(Default)]
pub struct ApplyStats {
    pub retracted: usize,
    pub skipped: usize,
}

/// Retract the duplicate twins from the report (the ONLY mechanical apply).
/// Everything is re-validated against the LIVE store, never the report
/// snapshot alone: each retract cites the exact head rev the report saw (a
/// concurrent revise = CAS conflict = skip), pins are re-read (pinned since
/// the report = skip), and a group whose keep is no longer a live single head
/// is skipped whole - the mechanical pass must never zero out every copy of a
/// fact. The keep re-check itself is a narrow read-then-write window, not a
/// transaction; the rev-cited CAS on each retract is the hard guarantee.
pub fn apply_dedup(db: &Path, store: &mut EventStore, report: &Report) -> anyhow::Result<ApplyStats> {
    let pins = crate::ledger::read_pins(db);
    let events = store.get_all_events()?;
    let heads = crate::cas::compute_head_sets(&events);
    let by_hash: HashMap<&str, &Event> =
        events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
    let keep_is_live = |id: &str| {
        heads.get(id).is_some_and(|hs| {
            hs.heads.len() == 1
                && by_hash
                    .get(hs.heads.iter().next().expect("len checked").as_str())
                    .is_some_and(|e| !matches!(e.kind, EventKind::FactRetracted))
        })
    };

    let mut stats = ApplyStats::default();
    for group in &report.dups {
        if !keep_is_live(&group.keep) {
            println!("  skip group: keep {} is no longer a live single head", group.keep);
            stats.skipped += group.retract.len();
            continue;
        }
        for target in &group.retract {
            if pins.iter().any(|p| p == &target.entity_id) {
                println!("  skip {}: pinned since the report was built", target.entity_id);
                stats.skipped += 1;
                continue;
            }
            match store.append_mutate_checked(
                "consolidate",
                "consolidate",
                "consolidate",
                EventKind::FactRetracted,
                &target.entity_id,
                Some(&target.parent_rev),
                &format!("[retracted by consolidate: duplicate of {}]", group.keep),
            ) {
                Ok(_) => stats.retracted += 1,
                Err(e) if e.downcast_ref::<crate::event_store::MutateConflict>().is_some() => {
                    println!("  skip {}: changed since the report was built", target.entity_id);
                    stats.skipped += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(stats)
}

pub fn print_report(report: &Report) {
    println!("THOR consolidate - metabolism report");
    if report.is_clean() {
        println!("clean: nothing to digest");
        return;
    }
    if !report.dups.is_empty() {
        println!(
            "\n{} duplicate group(s) (same normalized body prefix; --apply-dedup retracts the twins):",
            report.dups.len()
        );
        for g in &report.dups {
            let ids: Vec<&str> = g.retract.iter().map(|t| t.entity_id.as_str()).collect();
            println!("  keep {}  retract {}  | {}", g.keep, ids.join(" "), g.first_line);
        }
    }
    if !report.decay.is_empty() {
        println!(
            "\n{} decay candidate(s) (untyped, never marked, never read, long inactive) - confirm each via retract:",
            report.decay.len()
        );
        for d in &report.decay {
            println!("  {} ({} events behind tip) | {}", d.entity_id, d.events_behind_tip, d.first_line);
        }
    }
    if !report.clusters.is_empty() {
        println!(
            "\n{} same-topic cluster(s) - review for contradiction/distillation (revise/supersede/resolve); a cluster is a lead, not a verdict:",
            report.clusters.len()
        );
        for c in &report.clusters {
            println!("  [{}] {}", c.reason, c.members.join(" "));
        }
    }
    if report.broad_clusters_skipped > 0 {
        println!(
            "\n({} broad cluster(s) over {MAX_CLUSTER_MEMBERS} members skipped: batch/template families and union-find chains are not actionable leads)",
            report.broad_clusters_skipped
        );
    }
    if !report.cosine_ran {
        println!("\n(cosine pass skipped: vectors sidecar unavailable - lexical bands only)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventStore;

    fn opts(min_age_events: i64) -> Options {
        Options { min_age_events }
    }

    /// A store on disk (the ledger/vectors sidecars live next to the db path).
    fn store_at(dir: &Path) -> (EventStore, std::path::PathBuf) {
        let db = dir.join("thor.db");
        (EventStore::new(&db).unwrap(), db)
    }

    fn create(store: &mut EventStore, id: &str, body: &str) {
        store.append_event("s", "l", "a", EventKind::FactCreated, id, None, body).unwrap();
    }

    const LONG_A: &str = "the deploy pipeline always tars the crate and ships it to the build host over scp";

    #[test]
    fn dup_groups_prefer_imported_copy_and_apply_retracts_twins() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        // native twin FIRST (oldest) - the source-ref copy must still win
        create(&mut store, "mem-native", LONG_A);
        create(
            &mut store,
            "01KIMPORT",
            &format!("{LONG_A}\n\n[memory/note | tags: | project: global | mimir:01KIMPORT]"),
        );
        create(&mut store, "mem-native2", LONG_A);
        create(&mut store, "mem-other", "a completely unrelated fact about the courier snippet cap");

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        assert_eq!(report.dups.len(), 1, "one duplicate group");
        let g = &report.dups[0];
        assert_eq!(g.keep, "01KIMPORT", "the import-synced copy wins over the older native twin");
        let ids: Vec<&str> = g.retract.iter().map(|t| t.entity_id.as_str()).collect();
        assert_eq!(ids, vec!["mem-native", "mem-native2"]);

        let stats = apply_dedup(&db, &mut store, &report).unwrap();
        assert_eq!((stats.retracted, stats.skipped), (2, 0));
        let events = store.get_all_events().unwrap();
        let report2 = build_report(&store, &db, &events, &opts(i64::MAX));
        assert!(report2.dups.is_empty(), "apply is idempotent: a re-run reports no twins");
    }

    #[test]
    fn dup_keep_priority_prefers_typed_twin_over_older_raw() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        // older RAW twin first; the newer twin carries a typed footer (no
        // mimir ref, so not "imported") - the curated copy must win anyway
        create(&mut store, "mem-old-raw", LONG_A);
        create(&mut store, "mem-new-typed", &format!("{LONG_A}\n\n[memory/gotcha | tags: x | project: P]"));

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        assert_eq!(report.dups.len(), 1);
        assert_eq!(report.dups[0].keep, "mem-new-typed", "typed beats older raw");
        assert_eq!(report.dups[0].retract[0].entity_id, "mem-old-raw");
    }

    #[test]
    fn apply_revalidates_against_the_live_store() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        // group A: the retract target gets REVISED after the report is built
        create(&mut store, "mem-a-keep", &format!("{LONG_A}\n\n[memory/note | tags: | project: global | mimir:01KA]"));
        let a_twin = store
            .append_event("s", "l", "a", EventKind::FactCreated, "mem-a-twin", None, LONG_A)
            .unwrap();
        // group B: the KEEP dies after the report is built
        const LONG_B: &str = "the courier promotes one typed fact into slot three when the pool has no echo hit";
        create(&mut store, "mem-b-keep", &format!("{LONG_B}\n\n[memory/note | tags: | project: global | mimir:01KB]"));
        create(&mut store, "mem-b-twin", LONG_B);
        // group C: the retract target gets PINNED after the report is built
        const LONG_C: &str = "the embed daemon keeps one warm onnx session on a local tcp port for the courier";
        create(&mut store, "mem-c-keep", &format!("{LONG_C}\n\n[memory/note | tags: | project: global | mimir:01KC]"));
        create(&mut store, "mem-c-twin", LONG_C);

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        assert_eq!(report.dups.len(), 3, "three duplicate groups in the report");

        // the world changes between report and apply
        store
            .append_mutate_checked("s", "l", "a", EventKind::FactRevised, "mem-a-twin",
                Some(&a_twin.this_hash), "a legitimate concurrent edit, no longer a duplicate")
            .unwrap();
        store
            .append_mutate_checked("s", "l", "a", EventKind::FactRetracted, "mem-b-keep", None, "[gone]")
            .unwrap();
        crate::ledger::mutate_pins(&db, |mut pins| {
            pins.push("mem-c-twin".to_string());
            pins
        })
        .unwrap();

        let stats = apply_dedup(&db, &mut store, &report).unwrap();
        assert_eq!(stats.retracted, 0, "nothing may be retracted: every target was invalidated");
        assert_eq!(stats.skipped, 3, "revised twin, dead-keep group and pinned twin all skip");
        let events = store.get_all_events().unwrap();
        let heads = crate::cas::compute_head_sets(&events);
        let by_hash: std::collections::HashMap<&str, &Event> =
            events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
        for id in ["mem-a-twin", "mem-b-twin", "mem-c-twin"] {
            let hs = &heads[id];
            let head = by_hash[hs.heads.iter().next().unwrap().as_str()];
            assert!(
                !matches!(head.kind, EventKind::FactRetracted),
                "{id} must still be live after the guarded apply"
            );
        }
    }

    #[test]
    fn dup_groups_never_retract_a_pinned_twin() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        create(&mut store, "mem-pinned", LONG_A);
        create(
            &mut store,
            "01KIMPORT",
            &format!("{LONG_A}\n\n[memory/note | tags: | project: global | mimir:01KIMPORT]"),
        );
        crate::ledger::mutate_pins(&db, |mut pins| {
            pins.push("mem-pinned".to_string());
            pins
        })
        .unwrap();

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        assert_eq!(report.dups.len(), 1);
        assert_eq!(report.dups[0].keep, "mem-pinned", "pinned beats the imported copy");
        assert_eq!(report.dups[0].retract[0].entity_id, "01KIMPORT");
    }

    #[test]
    fn reproject_does_not_reset_the_decay_clock() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        create(&mut store, "mem-old", "an old scratch note that a bulk backfill later touches");
        for i in 0..10 {
            create(&mut store, &format!("Proj:pad/file.rs#{i}"), &format!("pad chunk {i}"));
        }
        // a recent ADMINISTRATIVE touch: scope moved, content untouched
        store
            .append_event("s", "l", "a", EventKind::FactReprojected, "mem-old", None,
                r#"{"project":"Proj"}"#)
            .unwrap();

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(5));
        assert!(
            report.decay.iter().any(|d| d.entity_id == "mem-old"),
            "a reproject must not reset staleness: {:?}",
            report.decay.iter().map(|d| &d.entity_id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn decay_requires_untyped_unread_unmarked_and_old_seeded_included() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        create(&mut store, "mem-stale", "an old scratch note about a temporary path nobody ever used");
        create(&mut store, "mem-typed", "a real gotcha body\n\n[memory/gotcha | tags: x | project: P]");
        create(
            &mut store,
            "01KMIRROR",
            "a mirrored source fact\n\n[memory/note | tags: | project: global | mimir:01KMIRROR]",
        );
        // an equally old seeded note that ANOTHER live fact cites by id: it
        // must never be suggested for decay (the citer is typed, so the citer
        // itself is protected too)
        create(
            &mut store,
            "01KCITED",
            "an old seeded note nobody reads directly\n\n[memory/note | tags: | project: global | mimir:01KCITED]",
        );
        create(
            &mut store,
            "mem-citer",
            "see the full trade-off in m:01KCITED before changing this\n\n[memory/gotcha | tags: x | project: P]",
        );
        create(&mut store, "mem-echoed", "a note that was marked useful once by the agent");
        store
            .append_event("s", "l", "a", EventKind::FactEchoed, "mem-echoed", None, "echo")
            .unwrap();
        create(&mut store, "mem-read", "a note that was read through mcp get at least once");
        crate::ledger::increment(&db, "access", "mem-read");
        // an echoed note DROWNED by noise marks: unified strength goes
        // negative, so it decays despite the echo
        create(&mut store, "mem-noised", "a note once echoed but repeatedly marked as noise since");
        store
            .append_event("s", "l", "a", EventKind::FactEchoed, "mem-noised", None, "echo")
            .unwrap();
        crate::ledger::increment(&db, "noise", "mem-noised");
        crate::ledger::increment(&db, "noise", "mem-noised");
        create(&mut store, "mem-pinned", "a pinned standing rule that never needs marking");
        crate::ledger::mutate_pins(&db, |mut pins| {
            pins.push("mem-pinned".to_string());
            pins
        })
        .unwrap();
        // pad the tip so the earlier entities age past the floor, then one
        // recent note that must NOT qualify
        for i in 0..10 {
            create(&mut store, &format!("Proj:pad/file.rs#{i}"), &format!("pad chunk {i}"));
        }
        create(&mut store, "mem-recent", "a brand new note right at the tip of the log");

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(5));
        let mut ids: Vec<&str> = report.decay.iter().map(|d| d.entity_id.as_str()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["01KMIRROR", "mem-noised", "mem-stale"],
            "untouched old notes decay - INCLUDING a stale seeded (imported) one, since the \
             stores are isolated and no import resurrects it; everything protected stays: \
             typed, pinned, echoed, read, recent, AND the id-cited note (01KCITED)"
        );
    }

    #[test]
    fn prefix_band_clusters_need_distinct_full_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        // same 40-char band, different tails -> a topic cluster
        create(&mut store, "mem-a", "the estimator quote flow rounds the price to the nearest cent before tax");
        create(&mut store, "mem-b", "the estimator quote flow rounds the price AFTER shipping is added, not before");
        // byte-twins (same full prefix) -> duplicates pass, NOT a cluster
        create(&mut store, "mem-c", LONG_A);
        create(&mut store, "mem-d", LONG_A);

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        assert_eq!(report.clusters.len(), 1, "one topic cluster");
        assert_eq!(report.clusters[0].members, vec!["mem-a".to_string(), "mem-b".to_string()]);
        assert_eq!(report.dups.len(), 1, "the byte-twins land in the duplicates pass");
    }

    #[test]
    fn chunks_and_diverged_entities_are_excluded_everywhere() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        create(&mut store, "Proj:src/a.rs#0", LONG_A);
        create(&mut store, "Proj:src/a.rs#1", LONG_A);
        // a diverged entity: two children of the same parent rev
        let root = store
            .append_event("s", "l", "a", EventKind::FactCreated, "mem-div", None, LONG_A)
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactRevised, "mem-div", Some(&root.this_hash), "branch one")
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactRevised, "mem-div", Some(&root.this_hash), "branch two")
            .unwrap();

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(0));
        assert!(report.dups.is_empty(), "chunk twins are ingest's business, diverged needs resolve first");
        assert!(report.decay.iter().all(|d| d.entity_id == "mem-div" || !d.entity_id.contains('#')),
            "chunks never decay");
        assert!(!report.decay.iter().any(|d| d.entity_id == "mem-div"), "diverged never decays");
    }

    #[cfg(feature = "semantic")]
    #[test]
    fn cosine_band_clusters_paraphrase_twins_with_different_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        let e1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "mem-p1", None,
                "the backup job runs nightly and verifies the restore")
            .unwrap();
        let e2 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "mem-p2", None,
                "every night a backup runs, and the restore path gets verified")
            .unwrap();
        let e3 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "mem-far", None,
                "the guard debounces file advisories per session")
            .unwrap();

        let vpath = crate::vectors::default_vectors_path(&db);
        let mut vs = crate::vectors::VectorStore::open(&vpath).unwrap();
        vs.set_model_id(crate::embed::MODEL_ID).unwrap();
        let mut near_a = vec![0.0f32; 384];
        near_a[0] = 1.0;
        let mut near_b = vec![0.0f32; 384];
        near_b[0] = 0.95;
        near_b[1] = 0.31;
        let mut far = vec![0.0f32; 384];
        far[2] = 1.0;
        vs.upsert_batch(&[(e1.seq, near_a), (e2.seq, near_b), (e3.seq, far)]).unwrap();

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        assert!(report.cosine_ran, "sidecar present: the cosine pass must run");
        let cosine: Vec<&Cluster> =
            report.clusters.iter().filter(|c| c.reason.starts_with("cosine")).collect();
        assert_eq!(cosine.len(), 1, "one cosine cluster: {:?}",
            report.clusters.iter().map(|c| (&c.reason, &c.members)).collect::<Vec<_>>());
        assert_eq!(cosine[0].members, vec!["mem-p1".to_string(), "mem-p2".to_string()]);
    }

    #[cfg(feature = "semantic")]
    #[test]
    fn absent_vectors_sidecar_means_no_cosine_pass_and_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        create(&mut store, "mem-x", LONG_A);

        let vpath = crate::vectors::default_vectors_path(&db);
        assert!(!vpath.exists(), "precondition: no sidecar");
        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        assert!(!report.cosine_ran, "no sidecar = the cosine pass did NOT run");
        assert!(!vpath.exists(), "a report-only command must not materialize the sidecar");
    }

    #[cfg(feature = "semantic")]
    #[test]
    fn stale_model_sidecar_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        create(&mut store, "mem-x", LONG_A);

        let vpath = crate::vectors::default_vectors_path(&db);
        let vs = crate::vectors::VectorStore::open(&vpath).unwrap();
        vs.set_model_id("some-other-model@v0").unwrap();
        drop(vs);

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        assert!(!report.cosine_ran, "a sidecar from another model is stale, never trusted");
    }
}
