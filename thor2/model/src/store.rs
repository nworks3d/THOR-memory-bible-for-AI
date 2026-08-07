//! The write port into `core`: an item is stored as canonical JSON in the
//! body of an event, and nothing here ever composes or parses a prose
//! footer (see `FINDINGS-1.0.md` F1; `model/tests/no_footer_calls.rs`
//! enforces this by grepping this crate's own source).
//!
//! Two failure policies, as the workspace design note requires: this module
//! is the "write and declare" side, so every error type here is surfaced to
//! the caller, never swallowed.

use crate::gate::{self, Refusal};
use crate::item::{Binding, Item};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use thor_core::cas::compute_head_sets;
use thor_core::event_store::{Event, EventKind, EventStore};

/// Everything that can go wrong declaring or revising an item. Never
/// swallowed: every variant is meant to be shown to the writer.
#[derive(Debug)]
pub enum WriteError {
    /// The write gate refused the item before anything was appended.
    Refused(Refusal),
    /// Canonical serialisation failed (should not happen for a well-formed
    /// `Item`; surfaced rather than unwrapped so a caller never panics).
    Serialize(serde_json::Error),
    /// The underlying log append failed (I/O, a stale parent_rev, a
    /// retracted head - see `thor_core::event_store::MutateConflict`).
    Store(anyhow::Error),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Refused(r) => write!(f, "{r}"),
            WriteError::Serialize(e) => write!(f, "could not serialize item: {e}"),
            WriteError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl std::error::Error for WriteError {}

/// Everything that can go wrong reading an item back.
#[derive(Debug)]
pub enum ReadError {
    /// No live event exists for this id.
    NotFound(String),
    /// The entity has more than one current head (a resolve is needed first);
    /// carries how many.
    Diverged(usize),
    /// The stored body was not valid canonical JSON for an `Item`.
    Parse(serde_json::Error),
    /// The head is a tombstone: this item was retracted, on purpose, and is no
    /// longer live. Reported by kind rather than left to fail as a parse error,
    /// so "someone decided this is wrong" never reaches a caller disguised as
    /// "the body is corrupt". Carries the reason given at retraction.
    Retracted(String),
    /// The underlying log read failed.
    Store(anyhow::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::NotFound(id) => write!(f, "no item found for id '{id}'"),
            ReadError::Diverged(n) => write!(f, "id has {n} diverged heads; resolve before reading"),
            ReadError::Parse(e) => write!(f, "stored body is not a valid item: {e}"),
            ReadError::Retracted(why) => write!(
                f,
                "this item was retracted and is no longer live ({why}); a retraction is a \
                 decision, so bring it back with a fresh declare, never by revising the tombstone"
            ),
            ReadError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// The canonical JSON body for an item: `serde_json::to_string` on the
/// struct directly, never through a `serde_json::Value` map, so the field
/// order is exactly the struct's declared order - deterministic by
/// construction (see `item::Item`'s doc comment and its round-trip tests).
pub fn canonical_body(item: &Item) -> Result<String, serde_json::Error> {
    serde_json::to_string(item)
}

/// Parse a stored body back into an item.
pub fn parse_body(body: &str) -> Result<Item, serde_json::Error> {
    serde_json::from_str(body)
}

/// Validate a new item against the write gate and, only if it passes,
/// append it to the log as a `fact_created` event whose body is its
/// canonical JSON. Nothing is written when the gate refuses.
/// An item with its project put through the one normalisation that decides
/// what a project identity IS (`normalize::normalize_project`). Applied on
/// every write, so a value meaning "the global tier" can never be stored as a
/// project named "global" - see that function for the near-miss that made
/// this necessary.
fn normalized(item: &Item) -> Item {
    let mut out = item.clone();
    out.project = crate::normalize::normalize_project(out.project.as_deref());
    out
}

/// How much normalised word-set overlap (Jaccard: |intersection| / |union|
/// over each text's normalised words) between a new declaration and a live
/// item of the same kind counts as the same fact told twice. 0.8 is a
/// judgement call, not a measured value - no labelled corpus of "same fact,
/// reworded" pairs exists yet to calibrate it against. It is set high enough
/// that two sentences sharing a topic but making a different point should
/// still pass, while two close paraphrases of the same fact (the defect this
/// check exists for) should not.
const NEAR_DUPLICATE_JACCARD_THRESHOLD: f64 = 0.8;

/// Normalise item text for the near-duplicate comparison in `declare`:
/// lowercase, then keep only maximal runs of letters/digits as words, joined
/// by a single space. This collapses every whitespace run to one space AND
/// strips punctuation in the same pass - a punctuation character (a hyphen
/// included) acts as a word boundary rather than being deleted in place, so
/// "force-push" and "force push" normalise to the identical two words
/// instead of "force-push" gluing into one "forcepush" that would then match
/// nothing. This is a content-comparison normalisation, a different job from
/// `normalize::normalize_target` (which normalises a TARGET VALUE for
/// path/host/route identity) - it does not reuse or replace that function.
fn normalize_for_comparison(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The set of words in an already-normalised text, for a Jaccard comparison.
/// Splits on the single spaces `normalize_for_comparison` already collapsed
/// every gap down to.
fn word_set(normalized_text: &str) -> HashSet<&str> {
    normalized_text.split(' ').filter(|word| !word.is_empty()).collect()
}

/// Jaccard similarity of two word sets: |intersection| / |union|. Two empty
/// sets (two texts with no comparable word at all) count as identical rather
/// than as "no overlap" - an empty text is not a wildcard that matches
/// nothing.
fn jaccard_similarity(a: &HashSet<&str>, b: &HashSet<&str>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    intersection as f64 / union as f64
}

/// Every live item in the store - single current head, not a retraction
/// tombstone, body parses as a valid `Item` - as `(id, item)` pairs in
/// ascending id order (deterministic; never hashmap order). Only
/// `find_near_duplicate` (below) calls this.
///
/// Mirrors the same live/single-head/non-retracted rule `show` (above) and
/// `serve::live::live_items` already use, reimplemented here rather than
/// imported because `model` cannot depend on `serve` (the dependency runs
/// the other way) and this change may only touch this one file.
///
/// Reads the `head_state` projection (`EventStore::projected_head_events`)
/// instead of folding the whole event log whenever that projection is
/// current (`EventStore::heads_projection_current`), and falls back to the
/// untouched old fold (`live_items_from_fold`) otherwise - the identical
/// fallback rule `serve::live::live_items` already established for this same
/// question (see `serve/src/live.rs`'s own doc comment for the read-side
/// defect this closed; this closes the matching defect on the write side).
/// Correctness therefore never depends on the projection, only speed does: a
/// bypass writer (a direct log restore that skips `append_event`, a pre-M2
/// binary, or a store this binary has never opened with a write handle)
/// simply leaves the fold as the answer until the next append catches the
/// projection up.
fn live_items_from_projection(store: &EventStore) -> anyhow::Result<Vec<(String, Item)>> {
    let rows = store.projected_head_events()?;
    Ok(rows
        .into_iter()
        .filter(|(_, head_count, _)| *head_count == 1) // diverged: not a live item, never guessed at
        .filter(|(event, _, _)| event.kind != EventKind::FactRetracted) // a tombstone is not a live item
        .filter_map(|(event, _, _)| {
            parse_body(&event.body).ok().map(|existing| (event.entity_id.clone(), existing))
        })
        .collect())
}

/// The always-correct path (see `live_items_from_projection`): the original
/// whole-log fold, byte-for-byte unchanged in behaviour. Used whenever the
/// `head_state` projection is not current, so a stale or bypassed projection
/// can never change which items a new declaration is compared against.
fn live_items_from_fold(store: &EventStore) -> anyhow::Result<Vec<(String, Item)>> {
    let events = store.get_all_events()?;
    let heads = compute_head_sets(&events);
    let by_hash: HashMap<&str, &Event> = events.iter().map(|e| (e.this_hash.as_str(), e)).collect();

    let mut ids: Vec<&String> = heads.keys().collect();
    ids.sort(); // deterministic: never depend on hashmap iteration order
    let mut out = Vec::new();
    for id in ids {
        let head_set = &heads[id];
        if head_set.heads.len() != 1 {
            continue; // diverged: not a live item, never guessed at
        }
        let head_hash = head_set.heads.iter().next().expect("len checked above");
        let Some(head_event) = by_hash.get(head_hash.as_str()) else { continue };
        if head_event.kind == EventKind::FactRetracted {
            continue; // a tombstone is not a live item
        }
        let Ok(existing) = parse_body(&head_event.body) else { continue };
        out.push((id.clone(), existing));
    }
    Ok(out)
}

/// The id of a live item of the SAME kind this new declaration would be a
/// near-duplicate of, if any - the first one found in id order
/// (deterministic; never hashmap order - see `live_items_from_projection`/
/// `live_items_from_fold`). A near-duplicate is an identical normalised
/// text, or a Jaccard word-set overlap at or above
/// `NEAR_DUPLICATE_JACCARD_THRESHOLD`. Only `declare` calls this: `revise`
/// corrects an item that is already allowed to exist, so it never runs
/// through this check.
///
/// COST: bounded by the number of LIVE entities the store holds, not by the
/// number of events ever appended, whenever the `head_state` projection is
/// current - see `live_items_from_projection`'s own doc comment for how and
/// why that is safe to trust. Either way, every live item found is still
/// parsed and normalised here to compare against the new one: no projection
/// narrows by an item's business `kind` (Rule/Orientation/...) or its
/// `text`, only by liveness (`core`'s `item_binding` table projects
/// bindings, not kind or text), so that part of the cost is unavoidable
/// without a schema change.
fn find_near_duplicate(store: &EventStore, item: &Item) -> anyhow::Result<Option<String>> {
    let candidates = if store.heads_projection_current() {
        live_items_from_projection(store)?
    } else {
        live_items_from_fold(store)?
    };

    let new_normalized = normalize_for_comparison(&item.text);
    let new_words = word_set(&new_normalized);

    for (id, existing) in candidates {
        if existing.kind != item.kind {
            continue; // only a live item of the SAME kind can be a near-duplicate
        }
        let existing_normalized = normalize_for_comparison(&existing.text);
        if existing_normalized == new_normalized {
            return Ok(Some(id));
        }
        if jaccard_similarity(&new_words, &word_set(&existing_normalized)) >= NEAR_DUPLICATE_JACCARD_THRESHOLD {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Validate a new item against the write gate, then refuse it if it is a
/// near-duplicate of a live item of the same kind; only if both checks pass
/// is it appended to the log as a `fact_created` event whose body is its
/// canonical JSON. Nothing is written when either check refuses.
///
/// THE DEFECT THIS CLOSES. Storing a second near-identical copy of a fact
/// used to be prevented only by an instruction in a tool description telling
/// the caller to search first - a convention, not a mechanism, and it has
/// been observed failing in practice: a caller told a rule already existed
/// stored a second copy of it anyway. `find_near_duplicate` (above) is the
/// mechanism: same `kind`, and either an identical normalised text or a
/// Jaccard word-set overlap at or above `NEAR_DUPLICATE_JACCARD_THRESHOLD`,
/// is refused before anything is appended. `revise` does not call this - an
/// existing item is already allowed to exist, so correcting it is never
/// blocked by this check.
///
/// COST: `find_near_duplicate` reads the `head_state` projection instead of
/// folding the whole event log whenever that projection is current, falling
/// back to the old whole-log fold only when it is not (see
/// `find_near_duplicate`'s own doc comment) - then normalises and compares
/// the new text against every LIVE item of the same kind found that way. No
/// projection narrows by an item's business kind or text, only by liveness,
/// so every live item is still parsed.
/// A live item that shares a target with something being written now, and
/// whose own proof currently comes out FALSE.
#[derive(Debug, Clone)]
pub struct UnsettledNeighbour {
    pub id: String,
    /// The target both items are bound to, as `kind:value`.
    pub target: String,
}

/// The targets an item is bound to, as comparable `kind:value` strings.
fn target_keys(item: &Item) -> Vec<String> {
    item.bindings
        .iter()
        .filter_map(|b| match b {
            Binding::Target { kind, value } => Some(format!("{kind:?}:{value}")),
            _ => None,
        })
        .collect()
}

/// Live items sharing a target with `item` whose check currently FAILS.
///
/// WHY ONLY `Fails`, AND NEVER `CannotRun`. A `Fails` is a positive
/// statement about this root: the checker looked and the condition was not
/// true, so that neighbour is very probably stale. A `CannotRun` is the
/// absence of information - the file is not here, which usually means this
/// is simply a different checkout - and the doctrine is explicit that an
/// unresolvable check must never block anything. Blocking on it would fire
/// on every write made from the wrong directory, which is how a gate teaches
/// people to route around it.
pub fn unsettled_neighbours(
    store: &EventStore,
    item: &Item,
    root: &Path,
) -> anyhow::Result<Vec<UnsettledNeighbour>> {
    let keys = target_keys(item);
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let candidates = if store.heads_projection_current() {
        live_items_from_projection(store)?
    } else {
        live_items_from_fold(store)?
    };

    let mut out = Vec::new();
    for (id, existing) in candidates {
        if id == item.id {
            continue; // revising your own neighbourhood is not a debt
        }
        // A path is only unique WITHIN a project. `README.md` names a
        // different file in every checkout, so a fact about one repo's
        // README must never hold a write to another's hostage - and its
        // check, run against the wrong root, would report a failure that
        // says nothing about the file the writer is actually touching.
        // Caught 2026-08-06, one revise after attaching the first checks:
        // a fact about the 1.0 README would have blocked writes to this
        // repo's README, which shares only its name.
        if existing.project != item.project {
            continue;
        }
        let Some(check) = existing.check.as_ref() else { continue };
        let shared: Vec<String> = target_keys(&existing).into_iter().filter(|k| keys.contains(k)).collect();
        let Some(target) = shared.into_iter().next() else { continue };
        if crate::check::run(check, root) == crate::check::Outcome::Fails {
            out.push(UnsettledNeighbour { id, target });
        }
    }
    Ok(out)
}

/// `declare`, with a root to run the neighbourhood toll against.
///
/// THE TOLL, AND WHY IT IS HERE RATHER THAN IN A CHORE LIST SOMEWHERE.
/// Maintenance that lives beside the work is a suggestion, and suggestions
/// are exactly what an agent skips - measured on this project repeatedly, by
/// its own author and by this assistant on the day this was written. The
/// only thing that is not a suggestion is a refusal on something the writer
/// actually wants. So: adding a fact to a target where an EXISTING fact's
/// own proof has gone false is refused, and the refusal names them.
///
/// Deliberately narrow, in three ways:
/// - It is local. You pay for the one target you are touching, never for a
///   backlog somewhere else. A toll that says "clear everything first" gets
///   routed around, and then the store stops growing, which is worse than a
///   store that is slightly stale.
/// - It fires on `declare` only, never on `revise` or `retract`. Those ARE
///   the maintenance; tolling them would charge people for doing the thing.
/// - It needs a root. Without one there is nothing to run a check against
///   and the toll simply does not exist, exactly like every other check in
///   this system when its anchor does not resolve.
pub fn declare_in(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    item: &Item,
    root: Option<&Path>,
) -> Result<Event, WriteError> {
    if let Some(root) = root {
        let stale = unsettled_neighbours(store, &normalized(item), root).map_err(WriteError::Store)?;
        if !stale.is_empty() {
            let named = stale
                .iter()
                .map(|n| format!("'{}' (on {})", n.id, n.target))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(WriteError::Refused(Refusal {
                problem: format!(
                    "the target you are anchoring to already holds {} fact(s) whose own proof now comes out FALSE: {named}. Adding a fact next to one that has gone stale is how a memory stops being worth trusting",
                    stale.len()
                ),
                fix: "settle those first, then write this. Fixing one is cheap: revise it with a corrected check (its text is kept when you do not pass any), or retract it with a reason if it is simply gone. Then this write goes through untouched".to_string(),
            }));
        }
    }
    declare(store, session_id, lineage_id, actor, item)
}

pub fn declare(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    item: &Item,
) -> Result<Event, WriteError> {
    let item = normalized(item);
    gate::declare(&item).map_err(WriteError::Refused)?;
    if let Some(existing_id) = find_near_duplicate(store, &item).map_err(WriteError::Store)? {
        return Err(WriteError::Refused(Refusal {
            problem: format!("this is a near-duplicate of an existing live item (id '{existing_id}')"),
            fix: format!(
                "revise the existing item '{existing_id}' instead of storing a second copy of the same fact"
            ),
        }));
    }
    let body = canonical_body(&item).map_err(WriteError::Serialize)?;
    store
        .append_event(session_id, lineage_id, actor, EventKind::FactCreated, &item.id, None, &body)
        .map_err(WriteError::Store)
}

/// Validate a revise (everything `declare` requires, plus no field the
/// existing item had may be dropped) and, only if it passes, append it as a
/// `fact_revised` event. `existing` should be the item just read with
/// `show`, so the field-preservation check has something real to compare
/// against.
pub fn revise(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    existing: &Item,
    updated: &Item,
) -> Result<Event, WriteError> {
    // BOTH sides are normalised before the dropped-field check. An item
    // already stored with project "global" is a global item; rewriting it as
    // `None` says the same thing, so it must not read as dropping the field.
    // Comparing a normalised update against an unnormalised original would
    // refuse exactly the repair that fixes the store.
    let existing = normalized(existing);
    let updated = normalized(updated);
    gate::revise(&existing, &updated).map_err(WriteError::Refused)?;
    let body = canonical_body(&updated).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, None, &body)
        .map_err(WriteError::Store)
}

/// Read the current item stored under `entity_id`. Refuses (well, errors -
/// this is the read side, so per R5 a caller may choose to treat this as
/// "nothing to show" rather than a hard failure) when the entity does not
/// exist, is diverged, or its head body is not valid canonical JSON.
pub fn show(store: &EventStore, entity_id: &str) -> Result<Item, ReadError> {
    let events = store.get_events_by_entity(entity_id).map_err(|e| ReadError::Store(e.into()))?;
    if events.is_empty() {
        return Err(ReadError::NotFound(entity_id.to_string()));
    }
    let heads = thor_core::cas::compute_head_sets(&events);
    let head_set = heads.get(entity_id).ok_or_else(|| ReadError::NotFound(entity_id.to_string()))?;
    if head_set.heads.len() != 1 {
        return Err(ReadError::Diverged(head_set.heads.len()));
    }
    let head_hash = head_set.heads.iter().next().expect("checked len == 1 above");
    let head_event = events
        .iter()
        .rev()
        .find(|e| &e.this_hash == head_hash)
        .ok_or_else(|| ReadError::NotFound(entity_id.to_string()))?;
    if head_event.kind == EventKind::FactRetracted {
        return Err(ReadError::Retracted(tombstone_reason(&head_event.body)));
    }
    parse_body(&head_event.body).map_err(ReadError::Parse)
}

/// The reason out of a tombstone body, or a plain stand-in when the body is not
/// one this version wrote. Never fails: a caller asking "why is this gone" must
/// get an answer, not a second error on top of the first.
fn tombstone_reason(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_else(|| "no reason recorded".to_string())
}

/// Retract an item: append a tombstone so it stops being live everywhere that
/// reads through `live_items` or `show`. Nothing is deleted - the log keeps the
/// whole chain and `history` still walks it, which is the point: a memory that
/// can quietly lose a fact is not a memory.
///
/// `reason` is required and not allowed to be blank. A retraction with no
/// reason is the same silent-decision problem the whole contract is against:
/// six weeks later nobody can tell a deliberate removal from a mistake.
pub fn retract(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    entity_id: &str,
    reason: &str,
) -> Result<Event, WriteError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(WriteError::Refused(Refusal {
            problem: "a retraction with no reason".to_string(),
            fix: "say why this item is wrong or no longer applies - a future reader cannot tell \
                  a deliberate removal from an accident without it"
                .to_string(),
        }));
    }
    // Reading first turns "no such id" and "already retracted" into their own
    // honest errors instead of a CAS conflict that says neither.
    match show(store, entity_id) {
        Ok(_) => {}
        Err(ReadError::Parse(_)) => {} // unreadable body, still retractable
        Err(e) => return Err(WriteError::Store(anyhow::anyhow!("{e}"))),
    }
    let body = serde_json::json!({ "retracted": entity_id, "reason": reason }).to_string();
    store
        .append_mutate_checked(
            session_id,
            lineage_id,
            actor,
            EventKind::FactRetracted,
            entity_id,
            None,
            &body,
        )
        .map_err(WriteError::Store)
}

/// Settle a diverged entity by naming which head survives. Every current head
/// must be accounted for - `keep` plus `discard` has to be the whole set - and
/// `core` recomputes that set under the write lock, so a head that appeared
/// while the caller was deciding makes this fail instead of silently dropping
/// someone else's revision.
pub fn resolve(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    entity_id: &str,
    keep: &str,
    discard: &[String],
) -> Result<Event, WriteError> {
    store
        .append_resolve(session_id, lineage_id, actor, entity_id, keep, discard)
        .map_err(WriteError::Store)
}

/// One step in an item's life, oldest first.
/// There is no wall-clock field here because the event log has none: order is
/// `seq`, and `seq` is what the chain actually guarantees. Inventing a
/// timestamp from the row's insertion time would read as recorded fact.
pub struct Revision {
    pub seq: i64,
    pub kind: String,
    pub rev_hash: String,
    pub actor: String,
    /// The item as it stood after this step, when the body still parses as one.
    /// A tombstone or an unreadable body leaves this `None` rather than
    /// dropping the row: the step happened either way.
    pub item: Option<Item>,
}

/// The whole revision log for one id, oldest first, including retractions.
/// A read path: an entity with nothing in it comes back as an empty list, not
/// an error.
pub fn history(store: &EventStore, entity_id: &str) -> Result<Vec<Revision>, ReadError> {
    let events = store.get_events_by_entity(entity_id).map_err(|e| ReadError::Store(e.into()))?;
    Ok(events
        .into_iter()
        .map(|e| Revision {
            seq: e.seq,
            kind: e.kind.as_str().to_string(),
            rev_hash: e.this_hash.clone(),
            actor: e.actor.clone(),
            item: parse_body(&e.body).ok(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Binding, Kind, Severity, TargetKind};
    use intent::Action;

    fn sample() -> Item {
        Item {
            id: "test-item".to_string(),
            kind: Kind::Orientation,
            text: "the config lives in config/app.toml".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "config/app.toml".to_string() }],
            severity: Some(Severity::Costly),
            project: Some("thor2".to_string()),
            tags: vec!["config".to_string()],
            expires: None,
            key: None,
            falsifier: Some("config/app.toml is removed or the app stops reading it".to_string()),
            check: None,
        }
    }

    /// `sample()` with just the id and text swapped in - every other field
    /// (bindings, severity, project, tags, falsifier) stays constant across
    /// the near-duplicate tests below, since only `kind` and `text` are
    /// meant to matter to that check.
    fn sample_with(id: &str, text: &str) -> Item {
        let mut item = sample();
        item.id = id.to_string();
        item.text = text.to_string();
        item
    }

    #[test]
    fn declare_then_show_round_trips_through_a_real_store() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        let back = show(&store, &item.id).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn declare_refuses_and_writes_nothing_on_a_bad_item() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample();
        item.kind = Kind::Rule;
        item.bindings = vec![]; // ground 1: a Rule with no binding
        let err = declare(&mut store, "s1", "l1", "test", &item).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));
        assert!(store.get_all_events().unwrap().is_empty(), "a refused declare must write nothing");
    }

    #[test]
    fn show_reports_not_found_for_an_unknown_id() {
        let store = EventStore::in_memory().unwrap();
        let err = show(&store, "does-not-exist").unwrap_err();
        assert!(matches!(err, ReadError::NotFound(_)));
    }

    #[test]
    fn revise_round_trips_and_keeps_the_hash_chain_valid() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let mut updated = item.clone();
        updated.bindings.push(Binding::Moment(Action::Configure));
        revise(&mut store, "s1", "l1", "test", &item, &updated).unwrap();

        let back = show(&store, &item.id).unwrap();
        assert_eq!(back, updated);

        let events = store.get_all_events().unwrap();
        assert_eq!(events.len(), 2);
        thor_core::auditor::verify_chain_integrity(&events).expect("hash chain stays intact across a model-level revise");
    }

    #[test]
    fn revise_refuses_and_writes_nothing_when_a_field_is_dropped() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let mut updated = item.clone();
        updated.tags = vec![]; // ground 9: existing had tags, this drops them

        let err = revise(&mut store, "s1", "l1", "test", &item, &updated).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));
        assert_eq!(store.get_all_events().unwrap().len(), 1, "a refused revise must append nothing");
    }

    /// The defect this guards against: a retracted item that still answers
    /// `show` as if it were live. A wrong fact that cannot be taken out is
    /// worse than no memory, and one that reads as live after removal is worse
    /// still - the caller cannot tell.
    #[test]
    fn a_retracted_item_stops_answering_show_and_says_why() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        show(&store, &item.id).expect("live before the retraction");

        retract(&mut store, "s1", "l1", "test", &item.id, "the config moved").unwrap();

        match show(&store, &item.id) {
            Err(ReadError::Retracted(why)) => assert!(
                why.contains("the config moved"),
                "the reason given at retraction must come back out, got {why:?}"
            ),
            other => panic!("a retracted item must report itself retracted, got {other:?}"),
        }
    }

    /// The defect this guards against: a silent removal. Six weeks on, nobody
    /// can tell a deliberate retraction from an accident without a reason.
    #[test]
    fn a_retraction_with_no_reason_is_refused_and_writes_nothing() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let err = retract(&mut store, "s1", "l1", "test", &item.id, "   ").unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));
        assert_eq!(
            store.get_all_events().unwrap().len(),
            1,
            "a refused retraction must append nothing"
        );
        show(&store, &item.id).expect("and the item must still be live");
    }

    /// Nothing is deleted: the log keeps every step, retraction included, so
    /// the history is still walkable after the item stops being live.
    #[test]
    fn history_still_walks_an_item_that_was_retracted() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        let mut updated = item.clone();
        updated.text = "the config lives in config/app.toml, and is read once at start".to_string();
        revise(&mut store, "s1", "l1", "test", &item, &updated).unwrap();
        retract(&mut store, "s1", "l1", "test", &item.id, "superseded by the new layout").unwrap();

        let log = history(&store, &item.id).unwrap();
        assert_eq!(log.len(), 3, "declare, revise and retract must all still be there");
        assert_eq!(log[0].kind, "fact_created");
        assert_eq!(log[2].kind, "fact_retracted");
        assert!(log[0].item.is_some(), "a readable body parses back into an item");
        assert!(log[2].item.is_none(), "a tombstone is not an item, and is not pretended to be one");
        assert!(log[0].seq < log[1].seq && log[1].seq < log[2].seq, "oldest first");
    }

    #[test]
    fn history_of_an_unknown_id_is_empty_not_an_error() {
        let store = EventStore::in_memory().unwrap();
        assert!(history(&store, "never-existed").unwrap().is_empty());
    }

    // ---------------------------------------------------- near-duplicate check
    //
    // THE DEFECT THESE PREVENT: storing a second near-identical copy of a
    // fact used to be stopped only by a tool description telling the caller
    // to search first - a convention, not a mechanism, and one that has been
    // observed failing: a caller told a rule already existed stored a second
    // copy of it anyway. Each test below is named after the exact failure
    // mode it pins shut.

    #[test]
    fn declare_refuses_an_exact_duplicate_and_names_the_existing_id() {
        let mut store = EventStore::in_memory().unwrap();
        let first = sample_with("first-item", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second = sample_with("second-item", "the config lives in config/app.toml");
        let err = declare(&mut store, "s1", "l1", "test", &second).unwrap_err();
        match err {
            WriteError::Refused(r) => {
                assert!(
                    r.problem.contains("first-item") || r.fix.contains("first-item"),
                    "the refusal must name the existing item's id, got: {r}"
                );
                assert!(
                    r.fix.to_lowercase().contains("revise"),
                    "the refusal must say to revise the existing item instead: {r}"
                );
            }
            other => panic!("expected a Refused error for an exact duplicate, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_duplicate_appends_nothing_to_the_log() {
        let mut store = EventStore::in_memory().unwrap();
        let first = sample_with("first-item", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();
        let before = store.get_all_events().unwrap().len();

        let second = sample_with("second-item", "the config lives in config/app.toml");
        let err = declare(&mut store, "s1", "l1", "test", &second).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));

        let after = store.get_all_events().unwrap().len();
        assert_eq!(before, after, "a refused near-duplicate must append nothing to the log");
        assert!(
            matches!(show(&store, &second.id), Err(ReadError::NotFound(_))),
            "the refused item must not be readable either - nothing was ever written for it"
        );
    }

    #[test]
    fn a_paraphrase_with_high_word_overlap_is_refused_as_a_near_duplicate() {
        // Not an identical normalised string (the last word differs), but the
        // word-set overlap is well past the threshold - proves the Jaccard
        // path refuses on its own, not just the exact-text-match path.
        let mut store = EventStore::in_memory().unwrap();
        let first =
            sample_with("first-item", "never force-push to the main branch under any circumstance");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second =
            sample_with("second-item", "never force-push to the main branch under any circumstances");
        let err = declare(&mut store, "s1", "l1", "test", &second).unwrap_err();
        assert!(
            matches!(err, WriteError::Refused(_)),
            "a high-overlap paraphrase must be refused too, got {err:?}"
        );
    }

    #[test]
    fn a_genuinely_different_fact_of_the_same_kind_is_accepted() {
        let mut store = EventStore::in_memory().unwrap();
        let first = sample_with("first-item", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second = sample_with("second-item", "never force-push to the main branch");
        declare(&mut store, "s1", "l1", "test", &second)
            .expect("a genuinely different fact of the same kind must not be refused");
    }

    #[test]
    fn the_same_text_under_a_different_kind_is_accepted() {
        let mut store = EventStore::in_memory().unwrap();
        let first = sample_with("first-item", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let mut second = sample_with("second-item", "the config lives in config/app.toml");
        second.kind = Kind::Chunk;
        second.bindings = vec![]; // archive kinds may carry no binding (gate ground 3)
        second.falsifier = None; // only Rule/Orientation require one (gate ground 10)
        declare(&mut store, "s1", "l1", "test", &second)
            .expect("identical text under a DIFFERENT kind must not be refused as a near-duplicate");
    }

    #[test]
    fn revise_is_never_refused_by_the_near_duplicate_check() {
        let mut store = EventStore::in_memory().unwrap();
        let a = sample_with("item-a", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &a).unwrap();

        let b = sample_with("item-b", "never force-push to the main branch");
        declare(&mut store, "s1", "l1", "test", &b).unwrap();

        // Revise b so its text becomes IDENTICAL to a's live text. If revise
        // ran through the same check declare does, this would be refused as
        // a near-duplicate of a - it must not be: a revise corrects an item
        // that is already allowed to exist, and is never blocked by this
        // check.
        let mut updated_b = b.clone();
        updated_b.text = a.text.clone();
        let result = revise(&mut store, "s1", "l1", "test", &b, &updated_b);
        assert!(result.is_ok(), "a revise must never be refused by the near-duplicate check, got {result:?}");

        let back = show(&store, &b.id).unwrap();
        assert_eq!(back.text, a.text, "the revise must have gone through with the now-duplicate text");
    }

    /// THE DEFECT THIS CLOSES: `find_near_duplicate` now reads the
    /// `head_state` projection (`live_items_from_projection`) instead of
    /// folding the whole log (`live_items_from_fold`) on every `declare`
    /// call. This pins that the fast path names exactly the same live
    /// candidates the fold would, across the cases that decide liveness: a
    /// plain live item, one whose head moved under a revise, a tombstoned
    /// one (must be absent from both), and a diverged one (must be absent
    /// from both) - so swapping the read strategy could never quietly
    /// change which items a new declaration is compared against.
    #[test]
    fn near_duplicate_projection_path_matches_the_fold_over_a_mixed_store() {
        let mut store = EventStore::in_memory().unwrap();

        // p1: a plain live item.
        let p1 = sample_with("p1", "back up the database nightly");
        declare(&mut store, "s1", "l1", "test", &p1).unwrap();

        // p2: revised - its head must have moved off the original create.
        // revise is never subject to the near-duplicate gate, so the
        // revised text is free to be anything.
        let original = sample_with("p2", "rotate the api keys quarterly");
        declare(&mut store, "s1", "l1", "test", &original).unwrap();
        let mut updated = original.clone();
        updated.text = "do the other thing entirely".to_string();
        revise(&mut store, "s1", "l1", "test", &original, &updated).unwrap();

        // p3: retracted - a tombstone, must be absent from both paths.
        let p3 = sample_with("p3", "run linting before every commit");
        declare(&mut store, "s1", "l1", "test", &p3).unwrap();
        retract(&mut store, "s1", "l1", "test", &p3.id, "superseded by the new layout").unwrap();

        // p4: diverged (hand-crafted, same shape as
        // `serve::live`'s own equivalence test) - must be absent too.
        store.append_event("s1", "l1", "test", EventKind::FactCreated, "p4", None, "not json").unwrap();
        store
            .append_event("s1", "l1", "test", EventKind::FactRevised, "p4", Some("stale"), "also not json")
            .unwrap();

        assert!(store.heads_projection_current(), "no bypass writes here: projection must be current");

        let fast = live_items_from_projection(&store).unwrap();
        let slow = live_items_from_fold(&store).unwrap();

        let fast_ids: Vec<&str> = fast.iter().map(|(id, _)| id.as_str()).collect();
        let slow_ids: Vec<&str> = slow.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            fast_ids, slow_ids,
            "the projection path must name exactly the same live items as the fold, in the same order"
        );
        assert_eq!(fast_ids, vec!["p1", "p2"], "p3 (retracted) and p4 (diverged) must both be absent");

        let fast_texts: Vec<&str> = fast.iter().map(|(_, item)| item.text.as_str()).collect();
        let slow_texts: Vec<&str> = slow.iter().map(|(_, item)| item.text.as_str()).collect();
        assert_eq!(fast_texts, slow_texts, "both paths must carry the identical body for each id");
        assert_eq!(fast[1].1.text, "do the other thing entirely", "p2 must carry the REVISED body, not the original");
    }
}
