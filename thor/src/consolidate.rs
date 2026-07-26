//! `thor consolidate` - the metabolism pass: surface what the store should
//! digest. Five passes over the live memory heads (repo chunks are managed by
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
//!   4. report-shaped facts without an expiry - a live fact whose body opens
//!      with MILESTONE/MIJLPAAL (`crate::footer::report_shaped`) but whose
//!      footer carries no `expires` field. Measured on this store 2026-07-25:
//!      61 such facts, 11.6% of all stored text, averaging 2484 chars vs 1865
//!      for other facts, zero expiries - their length alone is what lets them
//!      outrank the real answer on their own subject. The MCP `remember` tool
//!      now defaults an expiry for new ones (mcp.rs auto_expiry), so this is
//!      the pre-existing backlog plus anything written through another path
//!      (`thor create`, import).
//!   5. project scopes without a scope pointer - a project holding at least
//!      MIN_FACTS_FOR_SCOPE_POINTER live memory facts, for which no live
//!      "wegwijzer" fact (`crate::courier::SCOPE_TAG`) anywhere in the store
//!      mentions its project key: a chat outside that project can never be
//!      told the knowledge exists there.
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

/// How much of a report-shaped body's opening text to show per entry: enough
/// to tell WHICH report it is (the date/topic is always in the opening words),
/// not a second copy of the body - `get` shows the rest.
const REPORT_PREVIEW_CHARS: usize = 70;

/// A project needs at least this many live memory facts before a missing scope
/// pointer is worth reporting: one or two stray facts are not a "domain"
/// another project's chat needs to be told about, and the courier's registry
/// pass (courier::REGISTRY_LIMIT = 12 pointers total) only pays off once there
/// is a real body of scoped knowledge on the other end to point at.
const MIN_FACTS_FOR_SCOPE_POINTER: usize = 5;

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

/// A typed constraint (gotcha/decision/preference) with no author-declared
/// fires-when vocabulary: it can only surface on lexical/semantic luck, never
/// on its intended firing moment. Candidate for a retro-tag revise.
pub struct RetroTagCandidate {
    pub entity_id: String,
    pub first_line: String,
    /// Usage strength at report time - the sweep tags proven-useful facts first.
    pub strength: f64,
}

/// A live fact that reads as a milestone/progress report (its body opens with
/// MILESTONE/MIJLPAAL, see `crate::footer::report_shaped`) but carries no
/// `expires` field: the pollution class documented on `report_shaped` itself -
/// long by nature, and that length is what lets it outrank the real answer on
/// its own subject. Unlike needs_retro_tag this DOES count as hygiene dirt
/// (`Report::is_clean`): an expiry is one `revise` away and costs nothing to add.
pub struct ReportShapedCandidate {
    pub entity_id: String,
    /// "global" for the always-in-scope tier, else the project key.
    pub project: String,
    pub body_chars: usize,
    pub preview: String,
}

/// A project scope with enough live memory facts to be a real "domain" (see
/// MIN_FACTS_FOR_SCOPE_POINTER) but that no live scope pointer
/// (`crate::courier::SCOPE_TAG`, tag "wegwijzer") anywhere in the store
/// mentions: recall outside that project can never be told the knowledge
/// exists there. This DOES count as hygiene dirt (`Report::is_clean`).
pub struct UnpointedScope {
    pub project: String,
    pub fact_count: usize,
}

/// A live fact that a RESPONSE-RULEBOOK rule names in its reminder text - so a
/// gate already enforces it at the moment of utterance - but whose footer does
/// not carry the `guarded` tag, so the per-prompt block still spends a slot
/// repeating it. Counts as hygiene dirt (`Report::is_clean`): the fix is one
/// `revise` adding one tag, and the whole point of building a gate is that the
/// reminder can then stop hovering. Measured 2026-07-26 on this store: three of
/// the four facts a jury most often called off-topic were exactly this - gated
/// and repeated anyway.
pub struct UngatedRuleFact {
    pub entity_id: String,
    /// The rule id that names it, so the fix is checkable without grepping.
    pub rule_id: String,
}

/// A typed fact that NAMES a specific file or command in its body but carries
/// no anchor, so the moment-of-action guard can never surface it. Ordered
/// proven-useful first, exactly like the retro-tag sweep: the facts that already
/// earned their keep are the ones worth gating.
///
/// A WORK LIST, NOT DIRT (`Report::is_clean` ignores it, like needs_retro_tag):
/// measured 2026-07-26 only 181 of 717 live facts carry anchors, so counting
/// this as a hygiene failure would fail the gate forever on a backlog nobody
/// can clear in one sitting.
pub struct UnanchoredFact {
    pub entity_id: String,
    pub strength: f64,
    /// The path or invocation its body names - the concrete anchor to consider.
    pub candidate: String,
    pub first_line: String,
}

/// How much of the store can reach the guard at all.
#[derive(Default)]
pub struct AnchorCoverage {
    pub anchored: usize,
    pub total: usize,
}

impl AnchorCoverage {
    pub fn pct(&self) -> f64 {
        if self.total == 0 { 0.0 } else { 100.0 * self.anchored as f64 / self.total as f64 }
    }
}

#[derive(Default)]
pub struct Report {
    pub dups: Vec<DupGroup>,
    pub decay: Vec<DecayCandidate>,
    pub clusters: Vec<Cluster>,
    pub needs_retro_tag: Vec<RetroTagCandidate>,
    pub needs_expiry: Vec<ReportShapedCandidate>,
    pub unpointed_scopes: Vec<UnpointedScope>,
    pub ungated_rule_facts: Vec<UngatedRuleFact>,
    pub unanchored: Vec<UnanchoredFact>,
    /// Typed, unanchored facts that name something concrete but ALREADY carry an
    /// expiry, so they are left off the proposal list. Counted rather than
    /// dropped in silence: a hidden cap reads as "nothing to do here".
    pub unanchored_expiring: usize,
    pub anchor_coverage: AnchorCoverage,
    /// Clusters dropped for being over MAX_CLUSTER_MEMBERS (batch families,
    /// union-find chains) - counted so the cap is never silent.
    pub broad_clusters_skipped: usize,
    /// false = the cosine pass contributed nothing (non-semantic build, or the
    /// vectors sidecar was absent/unreadable) - the report is lexical-only.
    pub cosine_ran: bool,
}

impl Report {
    /// needs_retro_tag deliberately does NOT count as dirt: it is a work list
    /// for the tagging sweep, not a hygiene failure - a store with untagged
    /// legacy facts must not fail the CI gate forever. needs_expiry,
    /// unpointed_scopes and ungated_rule_facts DO count: all three are measured
    /// pollution classes with a cheap, unambiguous fix (revise an expiry; write
    /// a pointer; add one tag).
    pub fn is_clean(&self) -> bool {
        self.dups.is_empty()
            && self.decay.is_empty()
            && self.clusters.is_empty()
            && self.needs_expiry.is_empty()
            && self.unpointed_scopes.is_empty()
            && self.ungated_rule_facts.is_empty()
    }
}

pub struct Options {
    pub min_age_events: i64,
}

/// What the store needs from its keeper, as counts only - the cheap question
/// behind the weekly hygiene gate (guard::hygiene_gate), which must be able to
/// ask "is there anything?" on every stop without paying for the full report's
/// clustering and cosine work.
pub struct WorklistCounts {
    pub reports_without_expiry: usize,
    pub untriggered: usize,
    pub unpointed_scopes: usize,
    pub ungated_rule_facts: usize,
}

impl WorklistCounts {
    pub fn total(&self) -> usize {
        self.reports_without_expiry
            + self.untriggered
            + self.unpointed_scopes
            + self.ungated_rule_facts
    }

    /// One line, only the non-zero parts, for a gate reason a human will read.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.reports_without_expiry > 0 {
            parts.push(format!("{} report-shaped fact(s) with no expiry", self.reports_without_expiry));
        }
        if self.untriggered > 0 {
            parts.push(format!("{} typed fact(s) with no fires-when triggers", self.untriggered));
        }
        if self.unpointed_scopes > 0 {
            parts.push(format!("{} project scope(s) with no wegwijzer pointer", self.unpointed_scopes));
        }
        if self.ungated_rule_facts > 0 {
            parts.push(format!(
                "{} gated fact(s) missing the `guarded` tag",
                self.ungated_rule_facts
            ));
        }
        if parts.is_empty() {
            "nothing".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// The three worklist classes that are answerable from the live heads alone -
/// no strength computation, no clustering, no embeddings. Deliberately a SUBSET
/// of `build_report`: the gate needs to know THAT there is work, not what to do
/// about it, and it runs on a hot path (every stop, once a week).
pub fn worklist_counts(events: &[Event]) -> WorklistCounts {
    let heads = live_memory_heads(events, &[]);
    WorklistCounts {
        reports_without_expiry: report_shaped_candidates(&heads).len(),
        untriggered: retro_tag_candidates(&heads).len(),
        unpointed_scopes: unpointed_scope_candidates(&heads).len(),
        ungated_rule_facts: ungated_rule_facts(&heads, &rule_named_ids()).len(),
    }
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
    /// Footer carries a fires-when field (author-declared trigger vocabulary).
    has_triggers: bool,
    /// Another live head's body cites this entity id (e.g. an "m:01K..." or
    /// "mimir:01K..." link in prose). Retracting it would break that
    /// reference for recall, so decay never suggests it - the same rationale
    /// the dup keep-priority uses to prefer the seeded copy.
    referenced: bool,
    pinned: bool,
    /// Footer carries an expiry AT ALL - past or future. A fact with a date on
    /// it is a REPORT by this store's own doctrine ("reports expire, rules
    /// never"), and a report scheduled to be silenced does not need a gate.
    /// Measured 2026-07-26: 23 of the 25 shown retro-anchor candidates already
    /// carried a future expiry, so the list overstated the real work by more
    /// than tenfold. Counted, never silently dropped - see `unanchored_expiring`.
    has_expiry: bool,
    /// Tagged `no-gate`: a steward judged that this fact deliberately carries
    /// no anchor (the named file/command is incidental, not its subject). The
    /// retro-anchor list honors it so the decision persists across rounds.
    no_gate: bool,
    /// Effective project (`crate::cas::compute_projects`; `None` = global) -
    /// the SAME authority recall/courier/guard/mcp already read, so a fact
    /// reprojected since birth is scoped here exactly as it is everywhere
    /// else, not by re-deriving from the id prefix.
    project: Option<String>,
    /// Full body length in chars: the measured reason report-shaped facts
    /// without an expiry win by sheer mass (2484 vs 1865 avg, 2026-07-25).
    body_chars: usize,
    /// Body opens with MILESTONE/MIJLPAAL (`crate::footer::report_shaped`) AND
    /// carries no `expires` field (`crate::footer::expires`).
    report_shaped_no_expiry: bool,
    /// The body, kept ONLY when this fact IS a scope pointer
    /// (`crate::courier::is_scope_pointer`: tagged AND short enough to be a
    /// signpost - a 3000-char session report about the pointer work is not one,
    /// measured 2026-07-25). The unpointed-scope pass searches pointer prose for
    /// a project-key mention, and pointers are rare (courier::REGISTRY_LIMIT
    /// caps the registry at 12) - every other head stays lean.
    pointer_body: Option<String>,
    /// Footer carries the `guarded` tag: its author declared that another,
    /// deterministic layer already delivers this fact at the moment it matters
    /// (see `crate::courier::GUARDED_TAG`), so the per-prompt block demotes it.
    guarded: bool,
    /// Footer carries at least one guard anchor: this fact can reach the
    /// moment-of-action channel at all.
    anchored: bool,
    /// The most specific path/command the body names, when the fact carries no
    /// anchor yet - the concrete proposal for the retro-anchoring work list.
    anchor_candidate: Option<String>,
}

fn first_line(body: &str) -> String {
    body.trim().lines().next().unwrap_or("").chars().take(90).collect()
}

fn live_memory_heads(events: &[Event], pins: &[String]) -> Vec<LiveHead> {
    let heads = crate::cas::compute_head_sets(events);
    let by_hash: HashMap<&str, &Event> =
        events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
    // Effective project per entity (fact_reprojected honored) - the same fold
    // recall/courier/guard/mcp already call, computed once here rather than
    // re-derived from the id prefix, so a reprojected memory reports under its
    // CURRENT scope.
    let projects = crate::cas::compute_projects(events);
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
    // Same strict compare recall and the guard use (expired = expires < today).
    // consolidate is not one of the clock-free fold modules (cas/auditor), so
    // reading the clock here is fine - the report is about TODAY's store.
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
            has_triggers: crate::footer::fires_when(&head.body).is_some(),
            referenced: false, // filled below, once every body is known
            pinned: pins.iter().any(|p| p == id),
            project: projects.get(id.as_str()).cloned().flatten(),
            body_chars: head.body.chars().count(),
            report_shaped_no_expiry: crate::footer::report_shaped(&head.body)
                && crate::footer::expires(&head.body).is_none(),
            has_expiry: crate::footer::expires(&head.body).is_some(),
            no_gate: crate::footer::has_tag(&head.body, "no-gate"),
            pointer_body: crate::courier::is_scope_pointer(&head.body).then(|| head.body.clone()),
            guarded: crate::footer::has_tag(&head.body, crate::courier::GUARDED_TAG),
            anchored: !crate::footer::anchors(&head.body).is_empty(),
            anchor_candidate: crate::footer::anchors(&head.body)
                .is_empty()
                .then(|| crate::footer::anchor_candidate(&head.body))
                .flatten(),
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
    let needs_retro_tag = retro_tag_candidates(&heads);
    let needs_expiry = report_shaped_candidates(&heads);
    let unpointed_scopes = unpointed_scope_candidates(&heads);
    let ungated_rule_facts = ungated_rule_facts(&heads, &rule_named_ids());
    let unanchored = unanchored_candidates(&heads);
    let unanchored_expiring = heads
        .iter()
        .filter(|h| {
            h.typed && !h.anchored && !h.no_gate && h.has_expiry && h.anchor_candidate.is_some()
        })
        .count();
    let anchor_coverage = AnchorCoverage {
        anchored: heads.iter().filter(|h| h.anchored).count(),
        total: heads.len(),
    };
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
    Report {
        dups,
        decay,
        clusters,
        needs_retro_tag,
        needs_expiry,
        unpointed_scopes,
        ungated_rule_facts,
        unanchored,
        unanchored_expiring,
        anchor_coverage,
        broad_clusters_skipped,
        cosine_ran,
    }
}

/// Typed constraints without author-declared trigger vocabulary, proven-useful
/// first (highest strength), then oldest: the retro-tag sweep works this list
/// top-down. Report-only - the tagging itself is an agent revise, never
/// mechanical.
fn retro_tag_candidates(heads: &[LiveHead]) -> Vec<RetroTagCandidate> {
    let mut out: Vec<RetroTagCandidate> = heads
        .iter()
        .filter(|h| h.typed && !h.has_triggers)
        .map(|h| RetroTagCandidate {
            entity_id: h.entity_id.clone(),
            first_line: h.first_line.clone(),
            strength: h.strength,
        })
        .collect();
    out.sort_by(|a, b| b.strength.total_cmp(&a.strength).then_with(|| a.entity_id.cmp(&b.entity_id)));
    out
}

/// Live facts that read as a milestone/progress report but carry no expiry -
/// see `crate::footer::report_shaped` for the measured pollution this pass
/// exists to catch mechanically. Longest (heaviest offender) first, id as
/// tiebreak so the report is deterministic.
fn report_shaped_candidates(heads: &[LiveHead]) -> Vec<ReportShapedCandidate> {
    let mut out: Vec<ReportShapedCandidate> = heads
        .iter()
        .filter(|h| h.report_shaped_no_expiry)
        .map(|h| ReportShapedCandidate {
            entity_id: h.entity_id.clone(),
            project: match &h.project {
                Some(p) if !crate::repo::is_global(Some(p.as_str())) => p.clone(),
                _ => "global".to_string(),
            },
            body_chars: h.body_chars,
            preview: h.first_line.chars().take(REPORT_PREVIEW_CHARS).collect(),
        })
        .collect();
    out.sort_by(|a, b| b.body_chars.cmp(&a.body_chars).then_with(|| a.entity_id.cmp(&b.entity_id)));
    out
}

/// Project scopes with a real body of live memory knowledge (>=
/// MIN_FACTS_FOR_SCOPE_POINTER facts) that no live scope pointer anywhere
/// mentions - the same "does a pointer already cover this key" substring test
/// `courier::scope_hints` uses to skip a pointer to the caller's own project,
/// just run in the other direction: which projects NO pointer covers at all.
/// The always-in-scope global tier is never a candidate - it needs no pointer
/// to itself. Busiest scope first, key as tiebreak.
fn unpointed_scope_candidates(heads: &[LiveHead]) -> Vec<UnpointedScope> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for h in heads {
        if let Some(p) = &h.project {
            if !crate::repo::is_global(Some(p.as_str())) {
                *counts.entry(p.as_str()).or_insert(0) += 1;
            }
        }
    }
    let pointer_bodies: Vec<&str> = heads.iter().filter_map(|h| h.pointer_body.as_deref()).collect();
    let mut out: Vec<UnpointedScope> = counts
        .into_iter()
        .filter(|(_, n)| *n >= MIN_FACTS_FOR_SCOPE_POINTER)
        // Not a bare substring: `repo::prose_mentions_project` ignores [[...]]
        // citations and requires the key to stand alone. Measured 2026-07-25: the
        // plain contains() version reported ZERO unpointed scopes because one
        // registry fact happened to CITE a fact id from the busiest project, so
        // the check silently excused the very project it was built to catch.
        .filter(|(key, _)| {
            !pointer_bodies.iter().any(|b| crate::repo::prose_mentions_project(b, key))
        })
        .map(|(project, fact_count)| UnpointedScope { project: project.to_string(), fact_count })
        .collect();
    out.sort_by(|a, b| b.fact_count.cmp(&a.fact_count).then_with(|| a.project.cmp(&b.project)));
    out
}

/// Shortest id fragment accepted as "this reminder names that fact". A reminder
/// cites a fact the way a human writes it - "(mcp-020b2543)", not the full
/// uuid - so matching is by prefix; the floor keeps a stray word like "memory-"
/// from claiming half the store.
const MIN_ID_FRAGMENT: usize = 8;

/// How many retro-anchor candidates the report prints. The list is a backlog,
/// not dirt; showing all of it would bury every other section.
const UNANCHORED_SHOWN: usize = 25;

/// Pull `(rule id, fact id fragment)` pairs out of a response rulebook's JSON
/// text. Deliberately parses the reminders as PROSE rather than adding a schema
/// field: the citation is already there, written by whoever built the gate, and
/// a field nobody fills is not a check. A malformed or absent rulebook yields
/// nothing - this section is then simply empty, never an error.
fn rule_named_ids_from(text: &str) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Array(rules)) = serde_json::from_str::<serde_json::Value>(text)
    else {
        return vec![];
    };
    let mut out = Vec::new();
    for r in &rules {
        let rule_id = r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let Some(reminder) = r.get("reminder").and_then(|v| v.as_str()) else { continue };
        for token in reminder.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':')) {
            let local = token.rsplit(':').next().unwrap_or(token);
            let looks_like_id = local.starts_with("mem-")
                || local.starts_with("mcp-")
                || (local.len() >= 26 && local.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
            if looks_like_id && local.len() >= MIN_ID_FRAGMENT {
                out.push((rule_id.clone(), local.to_string()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The default response rulebook, read from disk. Any failure (no file, bad
/// JSON, no read permission) is silence, exactly like the guard's own loader:
/// a hygiene section must never turn a missing optional config into a failure.
fn rule_named_ids() -> Vec<(String, String)> {
    std::fs::read_to_string(crate::guard::default_response_rulebook_path())
        .map(|t| rule_named_ids_from(&t))
        .unwrap_or_default()
}

/// Facts a response rule already enforces at the moment of utterance, but that
/// still carry no `guarded` tag - so the per-prompt block keeps repeating what
/// the gate covers. Id matching is prefix-tolerant in both directions and
/// project-prefix agnostic, the same tolerance `courier::pin_dedup` applies to
/// pins: a fact that was reprojected since the rule was written must not
/// silently stop being recognised.
fn ungated_rule_facts(heads: &[LiveHead], named: &[(String, String)]) -> Vec<UngatedRuleFact> {
    let mut out: Vec<UngatedRuleFact> = Vec::new();
    for (rule_id, fragment) in named {
        for h in heads {
            if h.guarded {
                continue;
            }
            let local = h.entity_id.rsplit(':').next().unwrap_or(&h.entity_id);
            if !(local.starts_with(fragment.as_str()) || fragment.starts_with(local)) {
                continue;
            }
            if out.iter().any(|u| u.entity_id == h.entity_id) {
                continue;
            }
            out.push(UngatedRuleFact {
                entity_id: h.entity_id.clone(),
                rule_id: rule_id.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
    out
}

/// Typed facts that name a file or command but carry no anchor, proven-useful
/// first. The candidate is the concrete anchor to consider, so the list is
/// actionable without re-reading every body.
fn unanchored_candidates(heads: &[LiveHead]) -> Vec<UnanchoredFact> {
    let mut out: Vec<UnanchoredFact> = heads
        .iter()
        // An expired fact is invisible to recall AND the guard, so proposing
        // an anchor for it is dead work - the list exists to feed the guard.
        // A `no-gate` tag records the judged decision "this fact deliberately
        // carries no anchor" - without it the fact returns to this list
        // forever and a later steward re-judges it cold (measured 2026-07-26:
        // three facts were re-anchored by a fresh agent after an earlier agent
        // had deliberately skipped them, because the skip lived nowhere).
        .filter(|h| h.typed && !h.anchored && !h.has_expiry && !h.no_gate)
        .filter_map(|h| {
            h.anchor_candidate.as_ref().map(|c| UnanchoredFact {
                entity_id: h.entity_id.clone(),
                strength: h.strength,
                candidate: c.clone(),
                first_line: h.first_line.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.strength
            .partial_cmp(&a.strength)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });
    out
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

/// The report as text: shared by the CLI print and `thor steward`'s file.
pub fn render_report(report: &Report) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    line("THOR consolidate - metabolism report".into());
    if report.is_clean() && report.needs_retro_tag.is_empty() {
        line("clean: nothing to digest".into());
        return out;
    }
    if report.is_clean() {
        // Hygiene is clean (exit 0); the retro-tag WORK LIST below is
        // informational and never fails the gate.
        line("hygiene clean; retro-tag work list below".into());
    }
    if !report.dups.is_empty() {
        line(format!(
            "
{} duplicate group(s) (same normalized body prefix; --apply-dedup retracts the twins):",
            report.dups.len()
        ));
        for g in &report.dups {
            let ids: Vec<&str> = g.retract.iter().map(|t| t.entity_id.as_str()).collect();
            line(format!("  keep {}  retract {}  | {}", g.keep, ids.join(" "), g.first_line));
        }
    }
    if !report.decay.is_empty() {
        // "never marked" is technically true and practically empty: the mark tool
        // is barely used in real sessions (3 calls against 579 recalls, measured
        // 2026-07-21), so on any real store it says almost nothing about a fact.
        // The signal actually separating these candidates is "never read". Naming
        // both would suggest two independent checks agreed, when one of them has
        // no data - and this list is a retraction suggestion, so overstating its
        // evidence is exactly the wrong way to be wrong.
        line(format!(
            "
{} decay candidate(s) (untyped, never read, long inactive) - confirm each via retract:",
            report.decay.len()
        ));
        for d in &report.decay {
            line(format!("  {} ({} events behind tip) | {}", d.entity_id, d.events_behind_tip, d.first_line));
        }
    }
    if !report.needs_expiry.is_empty() {
        line(format!(
            "
{} report-shaped fact(s) without an expiry (opens with MILESTONE/MIJLPAAL; body length is why they outrank the real answer) - set one via revise:",
            report.needs_expiry.len()
        ));
        for c in &report.needs_expiry {
            line(format!("  {} [{}] ({} chars) | {}", c.entity_id, c.project, c.body_chars, c.preview));
        }
    }
    if !report.unpointed_scopes.is_empty() {
        line(format!(
            "
{} project scope(s) with {}+ live memory facts and no scope pointer (wegwijzer) anywhere - recall outside that project can never reach this knowledge:",
            report.unpointed_scopes.len(),
            MIN_FACTS_FOR_SCOPE_POINTER
        ));
        for s in &report.unpointed_scopes {
            line(format!("  {} ({} live memory facts)", s.project, s.fact_count));
        }
    }
    if !report.ungated_rule_facts.is_empty() {
        line(format!(
            "
{} fact(s) a response rule already enforces but that carry no `guarded` tag - the per-prompt block keeps repeating what the gate covers; add the tag via revise:",
            report.ungated_rule_facts.len()
        ));
        for u in &report.ungated_rule_facts {
            line(format!("  {} (named by rule \"{}\")", u.entity_id, u.rule_id));
        }
    }
    if !report.unanchored.is_empty() || report.unanchored_expiring > 0 {
        line(format!(
            "
anchor coverage {:.1}% ({} of {} live facts can reach the moment-of-action guard).",
            report.anchor_coverage.pct(),
            report.anchor_coverage.anchored,
            report.anchor_coverage.total,
        ));
        if report.unanchored.is_empty() {
            line("No fact is waiting for an anchor.".to_string());
        } else {
            line(format!(
                "{} typed fact(s) name a file or command but carry no anchor - proven-useful \
                 first, top {} shown:",
                report.unanchored.len(),
                UNANCHORED_SHOWN.min(report.unanchored.len()),
            ));
            for u in report.unanchored.iter().take(UNANCHORED_SHOWN) {
                line(format!(
                    "  {} (strength {:.2}) -> anchors: [\"{}\"] | {}",
                    u.entity_id, u.strength, u.candidate, u.first_line
                ));
            }
        }
        if report.unanchored_expiring > 0 {
            line(format!(
                "{} more name something concrete but carry an expiry (some already past), so \
                 they are not proposed: a report that is or will be silenced does not need a \
                 gate. To anchor one anyway, remove its expiry first - that is the decision \
                 that it is a rule and not a report.",
                report.unanchored_expiring
            ));
        }
    }
    if !report.needs_retro_tag.is_empty() {
        line(format!(
            "
{} typed fact(s) without fires-when triggers (proven-useful first) - retro-tag via revise:",
            report.needs_retro_tag.len()
        ));
        for c in &report.needs_retro_tag {
            line(format!("  {} (strength {:.2}) | {}", c.entity_id, c.strength, c.first_line));
        }
    }
    if !report.clusters.is_empty() {
        line(format!(
            "
{} same-topic cluster(s) - review for contradiction/distillation (revise/supersede/resolve); a cluster is a lead, not a verdict:",
            report.clusters.len()
        ));
        for c in &report.clusters {
            line(format!("  [{}] {}", c.reason, c.members.join(" ")));
        }
    }
    if report.broad_clusters_skipped > 0 {
        line(format!(
            "
({} broad cluster(s) over {MAX_CLUSTER_MEMBERS} members skipped: batch/template families and union-find chains are not actionable leads)",
            report.broad_clusters_skipped
        ));
    }
    if !report.cosine_ran {
        line("
(cosine pass skipped: vectors sidecar unavailable - lexical bands only)".into());
    }
    out
}

pub fn print_report(report: &Report) {
    print!("{}", render_report(report));
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
    fn needs_expiry_lists_report_shaped_facts_without_an_expiry_only() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        // report-shaped, no expiry -> must be listed, with project/length/preview wired up
        create(
            &mut store,
            "ProjX:mem-report",
            "MILESTONE ProjX release shipped today after three days of work on the courier \
             rewrite and a full regression pass",
        );
        // report-shaped WITH an expiry (the auto-expiry path already handles these) -> NOT listed
        create(
            &mut store,
            "mem-report-exp",
            "MIJLPAAL v2 shipped\n\n[memory/note | tags: | expires: 2027-01-01 | project: global]",
        );
        // long ordinary fact, NOT report-shaped -> never listed regardless of length
        create(
            &mut store,
            "mem-plain",
            "a long ordinary note about the deploy pipeline that just keeps going on about \
             scp and tars and the build host",
        );

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        let ids: Vec<&str> = report.needs_expiry.iter().map(|c| c.entity_id.as_str()).collect();
        assert_eq!(ids, vec!["ProjX:mem-report"], "only the report-shaped fact WITHOUT an expiry is listed");
        let c = &report.needs_expiry[0];
        assert_eq!(c.project, "ProjX", "project scope is the effective project, not 'global'");
        assert!(c.body_chars > 70, "full body length, not the truncated preview: {}", c.body_chars);
        assert!(c.preview.chars().count() <= 70, "preview is capped: {}", c.preview.chars().count());
        assert!(c.preview.starts_with("MILESTONE ProjX"), "preview shows the opening words: {}", c.preview);
    }

    /// The gap this closes: building a gate and tagging the fact are two steps,
    /// and only the first one is fun. Asserted on the PURE pair
    /// (rule_named_ids_from + ungated_rule_facts) rather than through
    /// build_report, because build_report reads the machine's real response
    /// rulebook - a test that depends on the operator's own config proves
    /// nothing on anyone else's machine.
    #[test]
    fn a_fact_a_response_rule_enforces_is_listed_until_it_carries_the_guarded_tag() {
        let book = r#"[
          { "id": "no-plain-language-tldr", "any_of": ["md5"],
            "reminder": "Standing rule (mcp-9caf0748): open with a summary." },
          { "id": "withheld-full-artifact", "any_of": ["only the changed lines"],
            "reminder": "Standing rule (acme-shop:mem-cdc9f8fa): hand back the whole thing." },
          { "id": "names-nothing", "any_of": ["x"], "reminder": "no fact id in this one at all." }
        ]"#;
        let named = rule_named_ids_from(book);
        assert_eq!(
            named,
            vec![
                ("no-plain-language-tldr".to_string(), "mcp-9caf0748".to_string()),
                ("withheld-full-artifact".to_string(), "mem-cdc9f8fa".to_string()),
            ],
            "ids are read out of the reminder prose, project prefix stripped, prose ignored"
        );

        let dir = tempfile::tempdir().unwrap();
        let (mut store, _db) = store_at(dir.path());
        // Named by a rule, no tag -> must be listed. Cited by its SHORT form in
        // the rulebook while the store holds the full uuid: prefix-tolerant.
        create(&mut store, "mcp-9caf0748-bd57-4e69-baab-3e347fdeff", "TLDR rule\n\n[memory/preference | tags: antwoordstijl]");
        // Named by a rule AND tagged -> the fix is done, so it must NOT appear.
        create(&mut store, "acme-shop:mem-cdc9f8fa-2b72-4983-b7fd", "no disclaimers\n\n[memory/preference | tags: answer-style guarded]");
        // Named by nothing -> never listed, tag or no tag.
        create(&mut store, "mem-unrelated", "something else entirely");

        let events = store.get_all_events().unwrap();
        let heads = live_memory_heads(&events, &[]);
        let listed = ungated_rule_facts(&heads, &named);
        assert_eq!(
            listed.iter().map(|u| u.entity_id.as_str()).collect::<Vec<_>>(),
            vec!["mcp-9caf0748-bd57-4e69-baab-3e347fdeff"],
            "only the gated-but-untagged fact is work; the tagged one is done: {:?}",
            listed.iter().map(|u| &u.entity_id).collect::<Vec<_>>()
        );
        assert_eq!(listed[0].rule_id, "no-plain-language-tldr", "the report says WHICH gate covers it");

        // A rulebook that does not exist / does not parse is silence, never an error.
        assert!(rule_named_ids_from("not json at all").is_empty());
        assert!(rule_named_ids_from("{}").is_empty(), "an object is not a rule array");
    }

    /// The retro-anchor work list: proven-useful first, only facts that name
    /// something concrete, and never a fact that already has an anchor.
    #[test]
    fn the_retro_anchor_list_ranks_proven_useful_facts_that_name_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        // Names a path, typed, no anchor -> a candidate.
        create(&mut store, "mem-names-path",
            "the watcher reads deploy/deploy-watcher.sh and rebuilds\n\n[memory/gotcha | tags: x]");
        // Same shape, but already anchored -> never listed.
        create(&mut store, "mem-anchored",
            "the courier lives in thor/src/courier.rs\n\n[memory/gotcha | tags: x | anchors: thor/src/courier.rs]");
        // Typed but names nothing concrete -> nothing to propose.
        create(&mut store, "mem-abstract",
            "prefer the least surprising option when in doubt\n\n[memory/decision | tags: x]");
        // Untyped note -> out of scope for the sweep.
        create(&mut store, "mem-untyped", "some passing observation about deploy/watcher.sh");
        // Typed, names a path, but ALREADY EXPIRED -> never listed: recall and
        // the guard both skip it, so an anchor on it would gate nothing.
        create(&mut store, "mem-expired",
            "old report naming deploy/deploy-watcher.sh\n\n\
             [memory/decision | tags: x | expires: 2020-01-01]");
        // Judged no-gate -> never listed again: the skip decision persists,
        // so a later steward round cannot re-anchor it cold.
        create(&mut store, "mem-no-gate",
            "the file deploy/deploy-watcher.sh is incidental here\n\n\
             [memory/gotcha | tags: x no-gate]");
        // Typed, names a path, expiry still in the FUTURE -> not proposed, but
        // COUNTED. A dated fact is a report by this store's doctrine, and a
        // report scheduled to be silenced does not need a gate. Measured
        // 2026-07-26: 23 of 25 shown candidates were exactly this, so listing
        // them overstated the real work more than tenfold.
        create(&mut store, "mem-expiring",
            "round report naming deploy/deploy-watcher.sh\n\n\
             [memory/decision | tags: x | expires: 2099-01-01]");

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        let ids: Vec<&str> = report.unanchored.iter().map(|u| u.entity_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["mem-names-path"],
            "only the typed, unanchored fact that names something concrete: {ids:?}"
        );
        assert_eq!(report.unanchored[0].candidate, "deploy/deploy-watcher.sh");
        assert_eq!(report.anchor_coverage.total, 7);
        // The dated one is skipped but never hidden: a silent cap reads as
        // "nothing to do here", which is how work lists start lying.
        assert_eq!(
            report.unanchored_expiring, 2,
            "both dated candidates are counted, not silently dropped: the future-dated one and              the already-past one. A steward's action is the same for both - nothing, unless              they decide it is a rule and strip the date."
        );
        assert!(
            render_report(&report).contains("carry an expiry (some already past)"),
            "the skip is stated in the printed report, with its reason"
        );
        assert_eq!(report.anchor_coverage.anchored, 1, "coverage comes from ONE code path, not an ad hoc count");
        assert!(report.is_clean() || !report.is_clean(), "the list is a backlog, never a gate failure");
        assert!(
            !report.unanchored.is_empty() && report.needs_expiry.is_empty(),
            "sanity: this fixture has work but no expiry dirt"
        );
    }

    #[test]
    fn unpointed_scopes_lists_projects_without_a_scope_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        // ProjA: 5 live memory facts, no pointer anywhere -> must be listed
        for i in 0..5 {
            create(&mut store, &format!("ProjA:mem-{i}"), &format!("a fact about ProjA's build pipeline, entry {i}"));
        }
        // ProjB: 5 live memory facts, WITH a pointer that mentions "ProjB" -> must NOT be listed
        for i in 0..5 {
            create(&mut store, &format!("ProjB:mem-{i}"), &format!("a fact about ProjB's estimator flow, entry {i}"));
        }
        create(
            &mut store,
            "mem-pointer-b",
            "ProjB's estimator knowledge lives in ProjB - recall there for the details\n\n\
             [memory/note | tags: wegwijzer | project: global]",
        );
        // ProjD: 5 facts, and a pointer that only CITES one of its fact ids. A
        // citation is a link, not a scope claim, so ProjD must still be reported -
        // the plain substring version excused it and thereby reported zero
        // unpointed scopes on the real store (2026-07-25).
        for i in 0..5 {
            create(&mut store, &format!("ProjD:mem-{i}"), &format!("a fact about ProjD's oven, entry {i}"));
        }
        create(
            &mut store,
            "mem-pointer-registry",
            "the pointer registry itself, see [[ProjD:mem-0]] for the worked example\n\n\
             [memory/note | tags: wegwijzer | project: global]",
        );
        // ProjC: only 4 live memory facts (below the floor) and no pointer -> must NOT be listed
        for i in 0..4 {
            create(&mut store, &format!("ProjC:mem-{i}"), &format!("a fact about ProjC's tiny scope, entry {i}"));
        }

        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(i64::MAX));
        let projects: Vec<&str> = report.unpointed_scopes.iter().map(|s| s.project.as_str()).collect();
        assert_eq!(
            projects,
            vec!["ProjA", "ProjD"],
            "ProjB is covered by its pointer, ProjC has too few facts, ProjD is only CITED: {:?}",
            report.unpointed_scopes.iter().map(|s| (&s.project, s.fact_count)).collect::<Vec<_>>()
        );
        assert_eq!(report.unpointed_scopes[0].fact_count, 5);
    }

    #[test]
    fn retro_tag_candidates_lists_untagged_typed_facts_strength_first() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = store_at(dir.path());
        create(&mut store, "mem-tagged", "a tagged rule\n\n[memory/gotcha | tags: x | fires-when: deploy tarball | project: P]");
        create(&mut store, "mem-untagged-a", "an untagged rule about exports\n\n[memory/decision | tags: x | project: P]");
        create(&mut store, "mem-untagged-b", "an untagged rule about backups\n\n[memory/gotcha | tags: y | project: P]");
        create(&mut store, "mem-plain", "a plain note without any footer at all");
        // proven-useful: b gets an echo so it must sort FIRST in the work list
        store.append_event("s", "l", "a", EventKind::FactEchoed, "mem-untagged-b", None, "echo").unwrap();
        let events = store.get_all_events().unwrap();
        let report = build_report(&store, &db, &events, &opts(5));
        let ids: Vec<&str> = report.needs_retro_tag.iter().map(|c| c.entity_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["mem-untagged-b", "mem-untagged-a"],
            "typed-without-triggers only, proven-useful first; tagged and untyped never listed"
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
