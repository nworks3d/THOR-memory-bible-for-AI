//! The write port into `core`: an item is stored as canonical JSON in the
//! body of an event, and nothing here ever composes or parses a prose
//! footer (see `FINDINGS-1.0.md` F1; `model/tests/no_footer_calls.rs`
//! enforces this by grepping this crate's own source).
//!
//! Two failure policies, as the workspace design note requires: this module
//! is the "write and declare" side, so every error type here is surfaced to
//! the caller, never swallowed.

use crate::gate::{self, Refusal};
use crate::item::{Binding, Item, Kind};
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
/// Made public so a census can ask "which pairs ALREADY in the store would
/// this rule have refused" using the one definition, rather than a second
/// one that drifts from it. The write gate refuses a near-duplicate on the
/// way in; nothing ever looked at the pairs that were already there.
pub const NEAR_DUPLICATE_JACCARD_THRESHOLD: f64 = 0.8;

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
pub fn normalize_for_comparison(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The set of words in an already-normalised text, for a Jaccard comparison.
/// Splits on the single spaces `normalize_for_comparison` already collapsed
/// every gap down to.
pub fn word_set(normalized_text: &str) -> HashSet<&str> {
    normalized_text.split(' ').filter(|word| !word.is_empty()).collect()
}

/// Jaccard similarity of two word sets: |intersection| / |union|. Two empty
/// sets (two texts with no comparable word at all) count as identical rather
/// than as "no overlap" - an empty text is not a wildcard that matches
/// nothing.
pub fn jaccard_similarity(a: &HashSet<&str>, b: &HashSet<&str>) -> f64 {
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
        // Two items scoped to DIFFERENT projects are not copies of each other,
        // however alike they read. Two repositories can genuinely carry the
        // same constraint - the same licence wording, the same release step -
        // and each needs its own, because a fact serves the project it is
        // scoped to and no other.
        //
        // THE DEFECT THIS CLOSES, and it is the exact opposite of what this
        // function is for. Comparing across every project meant the second
        // project could not have the rule at all, and the only shape that
        // satisfied both was ONE GLOBAL fact - which then fires in every
        // project on the machine, asserting something true of two of them and
        // false everywhere else. Observed 2026-08-07 with a licence rule: it
        // was global, so it fired on a GPLv3 repository claiming that
        // repository was non-commercial.
        //
        // Global against scoped is still compared, and must be: a global item
        // already fires inside every project, so a scoped copy of it really is
        // a second copy of something already being served there.
        if let (Some(existing_project), Some(new_project)) = (&existing.project, &item.project) {
            if existing_project != new_project {
                continue;
            }
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
    // CONTRACT R1's own refusal class, unpaid until 2026-08-08: an item that
    // cannot reach a block is cover that looks real and never fires. Only the
    // provable half refuses here; the rest comes back as a note on the write
    // (see `capacity`).
    if let Capacity::DeadOnArrival(refusal) = capacity(store, &item).map_err(WriteError::Store)? {
        return Err(WriteError::Refused(refusal));
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
    // The same capacity refusal `declare` makes. Without this the gate was
    // one door wide: declare refused an item that could never be shown, and
    // revise walked the identical binding in through the back.
    if let Capacity::DeadOnArrival(refusal) = capacity(store, &updated).map_err(WriteError::Store)? {
        return Err(WriteError::Refused(refusal));
    }
    let body = canonical_body(&updated).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, None, &body)
        .map_err(WriteError::Store)
}

/// What the capacity check concluded about a new item's chances of ever
/// being shown.
#[derive(Debug)]
pub enum Capacity {
    /// At least one of its bindings has room, or it outranks enough rivals.
    Fine,
    /// Every binding is already full of rivals it can never outrank. The item
    /// would be stored, fire nowhere, and nothing would say so.
    DeadOnArrival(Refusal),
    /// A binding is full of rivals at the same weight or heavier. Whether
    /// this item is seen then depends on closeness at some future moment,
    /// which nobody can decide now - so it is said, not enforced.
    Crowded(String),
}

/// Which live items would compete with `item` at the moment `binding` stands
/// for.
///
/// `Always` is deliberately not a pool: session start serves every pinned
/// item in full and caps nothing, so a pin never takes another pin's place
/// (CONTRACT: the five surfaces never share a candidate pool).
///
/// Scope follows what actually gets served: a project item competes with its
/// own project and with the global tier that reaches every project. A GLOBAL
/// item is compared only against other globals, because the projects it will
/// land in are not knowable at write time and guessing them would refuse
/// writes for crowding that may never happen.
/// A DIRECTORY rival is deliberately NOT counted against a file inside it,
/// and this note exists because the opposite was tried for a few hours on
/// 2026-08-08 and was wrong.
///
/// The reasoning that failed held that a file touch offers its parent
/// directory as a second target beside the path, putting a directory anchor in
/// the pool every time a file under it is touched. It does not.
/// `ServeInput::add_file` adds
/// a Path target only, and `normalize::target_matches` refuses a kind
/// mismatch, so that pool is never assembled anywhere.
///
/// Why that mattered rather than being a harmless extra: this count decides a
/// REFUSAL. Over-counting refuses an honest write for rivals it will never
/// meet, and the comment justifying it claimed the safe direction while doing
/// the unsafe one. Two independent reviews found it the same evening, both by
/// reading `input.rs` instead of the comment.
///
/// What is true about a Dir binding is said where it belongs, in `capacity`:
/// it reaches no automatic serving surface at all.
fn pool_rivals<'a>(
    candidates: &'a [(String, Item)],
    item: &Item,
    binding: &Binding,
) -> Vec<&'a (String, Item)> {
    let Some(kind_match): Option<&dyn Fn(&Binding) -> bool> = (match binding {
        Binding::Always => None,
        Binding::Moment(_) => Some(&|b: &Binding| matches!((b, binding), (Binding::Moment(x), Binding::Moment(y)) if x == y)),
        Binding::Target { .. } => Some(&|b: &Binding| match (b, binding) {
            (Binding::Target { kind: bk, value: bv }, Binding::Target { kind, value }) => {
                crate::normalize::target_matches(*bk, bv, *kind, value)
            }
            _ => false,
        }),
    }) else {
        return Vec::new();
    };

    candidates
        .iter()
        .filter(|(id, other)| {
            if *id == item.id || !other.kind.can_fire() {
                return false;
            }
            match (item.project.as_deref(), other.project.as_deref()) {
                (None, None) => true,
                (None, Some(_)) => false,
                (Some(_), None) => true,
                (Some(mine), Some(theirs)) => mine == theirs,
            }
        })
        .filter(|(_, other)| other.bindings.iter().any(|b| kind_match(b)))
        .collect()
}

/// Would this item ever reach a block, and if not, is that provable?
///
/// WHY THIS EXISTS AND WHY IT IS NOT A KNOB. CONTRACT R1 names "anchors on a
/// full target" as a refusal class and nothing ever implemented it, so by
/// R1's own last line that refusal did not exist. Version 1 had one, counted
/// per anchor; that unit is wrong here. Measured 2026-08-08: of the 38 items
/// competing at the worst target, only 5 were anchored there and 33 were
/// bound to a MOMENT, so a per-anchor gate would have let every one of them
/// through. The unit that competes is the pool `rank::select` assembles.
///
/// TWO ANSWERS, ALONG THE LINE THE DOCTRINE ALREADY DRAWS. A refusal has to
/// be provable, so it fires only when EVERY binding the item carries is
/// already full of rivals of STRICTLY heavier severity. Severity is compared
/// before closeness, so no context at any future moment can lift this item
/// into the block: it is dead on arrival, and storing it would be cover that
/// looks real and never fires. Anything less is a warning, because whether a
/// same-weight rival wins depends on closeness to something that has not
/// happened yet, and refusing on a guess is how a gate teaches people to
/// route around it.
///
/// No new constant: the line is `item::MAX_ITEMS`, the number of places a
/// block actually has. A configurable capacity would be exactly the
/// compensating knob CONTRACT R9 calls a reported design failure.
pub fn capacity(store: &EventStore, item: &Item) -> anyhow::Result<Capacity> {
    let bindings: Vec<&Binding> =
        item.bindings.iter().filter(|b| !matches!(b, Binding::Always)).collect();
    if bindings.is_empty() || !item.kind.can_fire() {
        return Ok(Capacity::Fine);
    }
    let candidates = if store.heads_projection_current() {
        live_items_from_projection(store)?
    } else {
        live_items_from_fold(store)?
    };

    // A Dir target reaches NO automatic serving surface. `ServeInput::add_file`
    // adds a Path target only, and `normalize::target_matches` refuses a kind
    // mismatch, so `rank::select` drops a Dir-bound item before it compares a
    // single path. Such an item can still REFUSE - the guard's location and
    // dir-content arms narrow their own pool by kind and decide containment
    // themselves - but it will never appear as advice at a file touch.
    //
    // Said out loud rather than refused, because "only ever refuses" is a
    // legitimate thing for a rule to be. But it has to be SAID, because this
    // is exactly the case the crowding count below cannot see: a directory
    // pool holding three items reads as roomy while being unreachable.
    // `bindings` above already dropped every Always, so this has to ask the
    // ITEM, not the filtered list. A pinned item is served in full at every
    // session start, so "never shown as advice" would be a flat lie about an
    // [Always, Dir] pair - and both reviews caught exactly that wording.
    let pinned = item.bindings.iter().any(|b| matches!(b, Binding::Always));
    let all_dir = !pinned
        && bindings.iter().all(|b| matches!(b, Binding::Target { kind: crate::item::TargetKind::Dir, .. }));
    if all_dir {
        return Ok(Capacity::Crowded(
            "every binding on this item is a DIRECTORY, which no automatic serving surface can \
             reach: a file touch offers the path, never its parent directory, so this item will \
             never be shown as advice. It can still refuse a write inside that directory. If you \
             meant it to be read rather than only enforced, bind it to the exact file it is about."
                .to_string(),
        ));
    }

    let mine = crate::item::severity_rank(item.severity);
    let mut every_binding_is_hopeless = true;
    let mut crowded: Option<String> = None;

    for binding in &bindings {
        let rivals = pool_rivals(&candidates, item, binding);
        let heavier = rivals.iter().filter(|(_, o)| crate::item::severity_rank(o.severity) < mine).count();
        let at_least_equal =
            rivals.iter().filter(|(_, o)| crate::item::severity_rank(o.severity) <= mine).count();

        if heavier < crate::item::MAX_ITEMS {
            every_binding_is_hopeless = false;
        }
        if crowded.is_none() && at_least_equal >= crate::item::MAX_ITEMS {
            crowded = Some(format!(
                "{} already holds AT LEAST {at_least_equal} item(s) of the same weight or heavier, \
                 for {} place(s) in a block - this one may well never be shown there. At least, \
                 because this count sees only the rivals sharing this one binding; the real crowd \
                 also includes everything reaching that place through a moment, which only doctor's \
                 crowding line can count. Bind it to the exact file or command it is about instead \
                 of the broad moment, or fold it into whichever item already carries that ground.",
                describe(binding),
                crate::item::MAX_ITEMS
            ));
        }
    }

    if every_binding_is_hopeless {
        let worst = bindings
            .iter()
            .map(|b| (describe(b), pool_rivals(&candidates, item, b)))
            .max_by_key(|(_, r)| r.len())
            .map(|(d, r)| {
                let ids: Vec<&str> = r.iter().take(3).map(|(id, _)| id.as_str()).collect();
                format!("{d} is held by {} heavier item(s), among them {}", r.len(), ids.join(", "))
            })
            .unwrap_or_default();
        return Ok(Capacity::DeadOnArrival(Refusal {
            problem: format!(
                "every binding on this item is already full of heavier rivals, so it can never \
                 reach a block: {worst}"
            ),
            fix: "give it a binding that is actually free - the exact file or command it is about \
                  rather than a broad moment - or raise its severity if that is honestly what it \
                  is, or fold the constraint into the item that already holds that ground. Storing \
                  it as it stands would be cover that looks real and never fires."
                .to_string(),
        }));
    }
    Ok(match crowded {
        Some(note) => Capacity::Crowded(note),
        None => Capacity::Fine,
    })
}

/// One binding, in the words a refusal can use.
fn describe(binding: &Binding) -> String {
    match binding {
        Binding::Always => "the pinned layer".to_string(),
        Binding::Moment(a) => format!("the moment '{}'", a.as_str()),
        Binding::Target { kind, value } => format!("the target {kind:?}:{value}"),
    }
}

/// The longest reason `archive` will carry. It becomes a tag, and a tag is
/// something search matches on: a paragraph in there would match everything.
pub const ARCHIVE_REASON_LIMIT: usize = 120;

/// Marks an anchor that points at something DELIBERATELY absent, so it must
/// never be read as decay.
///
/// THE DEFECT THIS CLOSES, and it was found the hard way on 2026-08-07. A
/// sweep archived six rules anchored at `firmware/src/secrets.h` - a file
/// that is gitignored and is SUPPOSED not to exist in a checkout. The anchor
/// was never stale: it was there precisely so the rules fire the moment
/// anybody creates or touches that file. One of the six said so in its own
/// text, in capitals. Prose could not stop the sweep, because a sweep reads
/// paths and not sentences, and that rule was scoped to a project the sweep
/// was not even running in.
///
/// So the distinction has to be something a machine can see. An item carrying
/// this tag is skipped by the decay count and refused by `archive`.
pub const DELIBERATE_ANCHOR_TAG: &str = "deliberate-anchor";

/// A heavy rule that carries no check informs and never refuses. Sometimes
/// that is correct - "ask before pushing to main" has no text whose presence
/// means the mistake, because an authorised push looks exactly like an
/// unauthorised one. Sometimes it is an oversight nobody ever revisits.
///
/// Prose cannot tell those apart, and nothing else can either: a rule that
/// merely informs is indistinguishable, from the outside, from a rule someone
/// forgot to give teeth. Measured 2026-08-09 on the real store: 2757 rules,
/// 331 carrying any check at all, and 17 able to refuse an introduction. Not
/// one thing in the system had ever asked the question.
///
/// So the distinction has to be something a machine can see. An item carrying
/// this tag has been through the question and the answer was no. Without it, a
/// heavy rule with no check is refused by `gate::declare` - not because it must
/// have teeth, but because it must have been ASKED.
pub const NO_LITERAL_TAG: &str = "no-literal";

/// The same answer, with the reason it was given: `no-literal:<why>`.
///
/// THE DEFECT THIS CLOSES, found in a session review on 2026-08-14. The tag
/// above is an exit from the gate, and it was the only exit here that cost
/// nothing. `archive` demands a reason. `retract` demands a reason. Every Rule
/// demands a falsifier. But "there is nothing a guard could catch here" was a
/// bare word, taken on the writer's say-so, and after it nothing in the system
/// ever asked again. An exit that is cheaper to take than the work it excuses
/// is the exit that gets taken: the review found a rule about a command tagged
/// rather than checked, and the tag ended the conversation.
///
/// Nothing can verify the reason, and that is not the point. No machine can
/// tell an honest "nothing to catch" from a lazy one - if one could, this
/// would be a check and not a tag (see `NO_LITERAL_TAG` above). What a reason
/// buys is that the answer can be read back, counted, and disagreed with by
/// whoever comes next. A bare word cannot be argued with.
pub const NO_LITERAL_REASON_PREFIX: &str = "no-literal:";

/// Short enough to write, long enough that "no" is not a reason.
pub const NO_LITERAL_REASON_MIN: usize = 20;

/// What one tag says about the teeth question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeethAnswer<'a> {
    /// The bare `no-literal`, with no reason. Still honoured for an item that
    /// already carried it - see `gate::revise` - and never accepted as a new
    /// answer.
    Bare,
    /// `no-literal:<why>`, carrying its reason (already trimmed, possibly
    /// empty if the writer typed the prefix and stopped).
    Reasoned(&'a str),
}

/// Read one tag as an answer to the teeth question, or `None` if it is not
/// one. The single definition of what answering looks like, so the gate that
/// accepts an answer and the debt that stops asking after one can never
/// disagree about which tags count.
pub fn teeth_answer(tag: &str) -> Option<TeethAnswer<'_>> {
    if let Some(reason) = tag.strip_prefix(NO_LITERAL_REASON_PREFIX) {
        return Some(TeethAnswer::Reasoned(reason.trim()));
    }
    (tag == NO_LITERAL_TAG).then_some(TeethAnswer::Bare)
}

/// Sometimes a fact belongs exactly where it is and the place is honestly full
/// of heavier things. The crowding debt has always SAID so - "leave it and say
/// why" is the third of the three ways out it offers - but saying why happened
/// in prose, and prose settles nothing. Measured 2026-08-09, on this codebase:
/// the debt re-fired on the same item every turn, because folding and
/// re-anchoring were the only exits that actually closed it. The one honest
/// answer left was to delete a true fact to stop the nagging, which is the
/// wrong incentive to build into a memory.
///
/// So the third exit gets the same shape as the other two: something a machine
/// can see. An item carrying this tag has been through the question and the
/// answer was that the crowd is deserved. The debt skips it; nothing else
/// changes, and it stays as findable and as crowded as it was.
pub const CROWDED_ON_PURPOSE_TAG: &str = "crowded-on-purpose";

/// A rule about what gets SAID rather than written can never carry a check:
/// a check reaches file writes and commands, and an answer is neither. Its
/// enforcement lives in the response guard's own rulebook, a file this store
/// knows nothing about - so a tag naming the entry is the link between the
/// two, and it is a BETTER answer to the teeth question than "nothing to
/// catch here", because it says where the catching actually happens.
///
/// Added 2026-08-09, the same evening ground 11 started asking: the first
/// rule to deserve this answer was refused for not having one, which is the
/// gate being right about the question and wrong about the vocabulary.
pub const ANSWER_GUARD_TAG_PREFIX: &str = "answer-guard:";

/// Turn a fireable item into archive material: same id, same text, same
/// history, still fully findable by `lookup` - but it stops claiming to fire.
///
/// WHY THIS IS NOT A `revise`. A Report may carry no bindings (gate ground 6)
/// and `gate::revise` refuses dropping bindings unconditionally, on purpose.
/// Those two rules are both right and they meet head on here, which is the
/// signal that archiving is not a correction at all. A correction says the
/// fact was wrong. This says the fact is still true and no longer has a place
/// to fire - a different act, so it gets its own door, the same way `retract`
/// does.
///
/// THE DEFECT THIS CLOSES. A store fills up with items anchored to things
/// that stopped existing: the one-off script that produced a measurement, the
/// experiment directory, the log file. The knowledge in them is often still
/// true and worth finding. But they are counted as live rules, they answer
/// "how many rules do you have" with a yes, and every one of them fires
/// nowhere while nothing says so. Retracting would take the knowledge out of
/// `lookup` as well, which is too blunt. This keeps the words and drops only
/// the claim that they fire.
///
/// A rule carrying a runnable check is REFUSED. That is exactly the kind that
/// can still block a wrong write, so archiving one would be throwing away the
/// only enforcement this memory has. Settle or remove the check first if that
/// is really what you mean.
pub fn archive(
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
            problem: "an archive with no reason".to_string(),
            fix: "say why this item can no longer fire - six weeks later nobody can tell a \
                  deliberate archive from an accident without it"
                .to_string(),
        }));
    }
    if reason.chars().count() > ARCHIVE_REASON_LIMIT {
        return Err(WriteError::Refused(Refusal {
            problem: format!("the reason is {} characters", reason.chars().count()),
            fix: format!("keep it under {ARCHIVE_REASON_LIMIT}: it becomes a tag, and search matches on tags"),
        }));
    }
    if reason.contains(',') || reason.contains('\n') || reason.contains('\r') {
        return Err(WriteError::Refused(Refusal {
            problem: "the reason contains a comma or a line break".to_string(),
            fix: "write it as one plain phrase - it becomes a single tag".to_string(),
        }));
    }

    let existing = show(store, entity_id).map_err(|e| WriteError::Store(anyhow::anyhow!("{e}")))?;
    if !existing.kind.can_fire() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{entity_id}' is already archive material ({:?})", existing.kind),
            fix: "nothing to do - it does not claim to fire".to_string(),
        }));
    }
    if existing.check.is_some() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{entity_id}' carries a runnable check"),
            fix: "a rule that can prove itself is the only kind that can ever block a wrong \
                  write; settle or clear the check first if archiving it is really the intent"
                .to_string(),
        }));
    }
    if existing.tags.iter().any(|t| t == DELIBERATE_ANCHOR_TAG) {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{entity_id}' anchors at something deliberately absent"),
            fix: "its anchor is not decay - it is there so the rule fires the moment that file \
                  appears or is touched; drop the deliberate-anchor tag first if that has really \
                  stopped being true"
                .to_string(),
        }));
    }

    let mut updated = existing.clone();
    updated.kind = Kind::Report;
    updated.bindings = Vec::new(); // a Report may carry none, and must not
    updated.severity = None; // meaningless once it cannot fire
    updated.tags.push(format!("archived:{reason}"));

    gate::declare(&updated).map_err(WriteError::Refused)?;
    let body = canonical_body(&updated).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, None, &body)
        .map_err(WriteError::Store)
}

/// Bring an item back out of the archive: restore the kind and the anchor it
/// had, and mark that anchor deliberate so no later sweep takes it again.
///
/// The counterpart to `archive`, and it exists because that operation was
/// used wrongly once. It is deliberately narrow - it takes the kind and the
/// one anchor explicitly rather than digging them out of history - because
/// undoing a mistake should be an act someone states in full, not a magic
/// rewind that might restore something nobody looked at.
pub fn restore_deliberate_anchor(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    entity_id: &str,
    kind: Kind,
    anchor: &str,
) -> Result<Event, WriteError> {
    let existing = show(store, entity_id).map_err(|e| WriteError::Store(anyhow::anyhow!("{e}")))?;
    if existing.kind.can_fire() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{entity_id}' is not archived ({:?})", existing.kind),
            fix: "nothing to restore - it already fires".to_string(),
        }));
    }
    if !kind.can_fire() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("{kind:?} is not a kind that fires"),
            fix: "restore it as a Rule or an Orientation, or leave it archived".to_string(),
        }));
    }

    let mut updated = existing.clone();
    updated.kind = kind;
    updated.bindings =
        vec![Binding::Target { kind: crate::item::TargetKind::Path, value: anchor.to_string() }];
    updated.tags.retain(|t| !t.starts_with("archived:"));
    if !updated.tags.iter().any(|t| t == DELIBERATE_ANCHOR_TAG) {
        updated.tags.push(DELIBERATE_ANCHOR_TAG.to_string());
    }

    gate::declare(&updated).map_err(WriteError::Refused)?;
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
    use crate::item::{Binding, Check, Kind, Severity, TargetKind};
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

    /// The defect this guards against, and it is this check working against
    /// its own purpose. Two repositories can genuinely need the same rule -
    /// the same licence wording, the same release step. Comparing across every
    /// project meant the second one could not have it, and the only shape that
    /// satisfied both was ONE GLOBAL fact, which then fires in every project on
    /// the machine. Observed 2026-08-07 with a licence rule that was global and
    /// therefore fired on a GPLv3 repository claiming it was non-commercial.
    #[test]
    fn the_same_rule_is_accepted_in_a_different_project() {
        let mut store = EventStore::in_memory().unwrap();
        let mut first = sample_with("licence-a", "never call this project free and open-source");
        first.project = Some("project-a".to_string());
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let mut second = sample_with("licence-b", "never call this project free and open-source");
        second.project = Some("project-b".to_string());
        declare(&mut store, "s1", "l1", "test", &second).expect(
            "the same constraint in a DIFFERENT project is not a second copy - refusing it \
             leaves one global fact as the only option, which is the defect",
        );
    }

    #[test]
    fn the_same_rule_twice_in_one_project_is_still_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let mut first = sample_with("licence-a", "never call this project free and open-source");
        first.project = Some("project-a".to_string());
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let mut second = sample_with("licence-b", "never call this project free and open-source");
        second.project = Some("project-a".to_string());
        assert!(
            matches!(declare(&mut store, "s1", "l1", "test", &second), Err(WriteError::Refused(_))),
            "within ONE project the duplicate check must still bite"
        );
    }

    /// A global item already fires inside every project, so a scoped copy of
    /// it really is a second copy of something already being served there.
    /// Skipping this pairing would let the store fill with per-project echoes
    /// of one global rule.
    #[test]
    fn a_scoped_copy_of_a_global_rule_is_still_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let mut global = sample_with("global-rule", "never call this project free and open-source");
        global.project = None;
        declare(&mut store, "s1", "l1", "test", &global).unwrap();

        let mut scoped = sample_with("scoped-rule", "never call this project free and open-source");
        scoped.project = Some("project-a".to_string());
        assert!(
            matches!(declare(&mut store, "s1", "l1", "test", &scoped), Err(WriteError::Refused(_))),
            "a global rule already fires here, so a scoped copy is a second copy"
        );
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

    /// Wildly different sentences on purpose: the near-duplicate check runs
    /// before the capacity check, and near-identical fixtures would be
    /// refused for the wrong reason and quietly test nothing.
    const DISTINCT: [&str; 9] = [
        "the changelog belongs at the repository root and nowhere else",
        "database migrations run forward only, never in reverse",
        "every uploaded image is stripped of its location metadata",
        "the nightly job writes its report before it deletes anything",
        "a released version number is never reused for a different build",
        "connection pools close on shutdown, in the opposite order they opened",
        "the search index rebuilds from source, never from its own output",
        "temporary credentials expire within the hour they were issued",
        "a queue consumer acknowledges only after the work has landed",
    ];

    fn bound_to(id: &str, nth: usize, moment: intent::Action, severity: Option<Severity>) -> Item {
        let mut item = sample_with(id, DISTINCT[nth % DISTINCT.len()]);
        item.bindings = vec![Binding::Moment(moment)];
        item.severity = severity;
        item
    }

    /// THE DEFECT THIS PREVENTS, and CONTRACT R1 named it long before anything
    /// implemented it: an item every one of whose bindings is already full of
    /// heavier rivals is stored, fires nowhere, and nothing says so. Severity
    /// is compared before closeness, so no future moment can lift it in - it
    /// is not unlucky, it is unreachable.
    #[test]
    fn an_item_that_can_never_reach_a_block_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let heavy = bound_to(&format!("heavy-{i}"), i, intent::Action::Commit, Some(Severity::Irreversible));
            declare(&mut store, "s", "l", "t", &heavy).expect("fixture must store");
        }
        let light = bound_to("light-one", 8, intent::Action::Commit, Some(Severity::HouseStyle));
        let err = declare(&mut store, "s", "l", "t", &light).expect_err("it can never be shown");
        let msg = format!("{err}");
        assert!(msg.contains("never reach a block"), "the refusal must say why: {msg}");
        assert!(msg.contains("heavy-"), "and name what holds the places: {msg}");
    }

    /// The other half, and the reason the refusal is narrow: a rival of the
    /// SAME weight might still lose to this item on closeness at some future
    /// moment. Refusing on that would be refusing on a guess, which is how a
    /// gate teaches people to route around it.
    #[test]
    fn a_full_pool_of_equals_is_a_note_and_never_a_refusal() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS + 2 {
            let peer = bound_to(&format!("peer-{i}"), i, intent::Action::Commit, Some(Severity::Costly));
            declare(&mut store, "s", "l", "t", &peer).expect("fixture must store");
        }
        let another = bound_to("another-peer", 8, intent::Action::Commit, Some(Severity::Costly));
        declare(&mut store, "s", "l", "t", &another).expect("an equal must still be storable");

        match capacity(&store, &another).unwrap() {
            Capacity::Crowded(note) => assert!(note.contains("same weight or heavier"), "{note}"),
            other => panic!("a full pool of equals must be reported, got {other:?}"),
        }
    }

    /// A second binding that is free saves the item: it is only dead when
    /// EVERY door is shut. Refusing while one is open would delete a fact that
    /// works perfectly well somewhere else.
    #[test]
    fn one_free_binding_is_enough_to_be_storable() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let heavy = bound_to(&format!("h-{i}"), i, intent::Action::Commit, Some(Severity::Irreversible));
            declare(&mut store, "s", "l", "t", &heavy).expect("fixture must store");
        }
        let mut light = bound_to("two-doors", 8, intent::Action::Commit, Some(Severity::HouseStyle));
        light.bindings.push(Binding::Target { kind: TargetKind::Path, value: "src/lonely.rs".to_string() });
        declare(&mut store, "s", "l", "t", &light).expect("the free target binding must save it");
    }

    /// THE DEFECT THIS PREVENTS, and it is one this file caused itself. For a
    /// few hours on 2026-08-08 a rule here made a Dir-bound rival compete with
    /// a Path-bound item, on the belief that a file touch offers the parent
    /// directory as a second target. It does not: `ServeInput::add_file` adds
    /// one target, a Path, and `normalize::target_matches` refuses a kind
    /// mismatch, so that pool is never assembled anywhere. (The false sentence
    /// itself is not repeated here; `serve/tests/
    /// a_comment_never_claims_what_the_code_does_not_do.rs` fails if it comes
    /// back, because copying it from one file to another is how it spread.)
    ///
    /// This count decides a REFUSAL, so an over-count is not an error in the
    /// safe direction: it refuses an honest write for rivals it will never
    /// meet. Two independent reviews found it the same evening, both by
    /// reading `input.rs` rather than the comment that claimed otherwise.
    #[test]
    fn a_directory_bound_rival_does_not_crowd_a_file_inside_it() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS + 2 {
            let mut heavy = sample_with(&format!("dir-{i}"), DISTINCT[i % DISTINCT.len()]);
            heavy.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: "src/deep".to_string() }];
            heavy.severity = Some(Severity::Irreversible);
            declare(&mut store, "s", "l", "t", &heavy).expect("fixture must store");
        }
        let mut light = sample_with("newcomer", "a webhook retry backs off before it gives up entirely");
        light.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/deep/mod.rs".to_string() }];
        light.severity = Some(Severity::HouseStyle);
        declare(&mut store, "s", "l", "t", &light)
            .expect("a directory anchor is not in the pool a file touch assembles, so it crowds nothing");
    }

    /// A note, never a refusal: "only ever refuses" is a legitimate thing for
    /// a rule to be. But it has to be SAID, because the crowding count cannot
    /// see it - a directory pool holding three items reads as roomy while
    /// being unreachable by every automatic surface.
    #[test]
    fn a_directory_only_item_is_told_it_will_never_be_shown_as_advice() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample_with("dir-only", "a webhook retry backs off before it gives up entirely");
        item.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: "src/deep".to_string() }];

        match capacity(&store, &item).unwrap() {
            Capacity::Crowded(note) => {
                assert!(note.contains("DIRECTORY"), "{note}");
                assert!(note.contains("never be shown"), "{note}");
                assert!(note.contains("can still refuse"), "it must not read as useless: {note}");
            }
            other => panic!("a directory-only item must be warned about, got {other:?}"),
        }
        declare(&mut store, "s", "l", "t", &item).expect("a note never refuses the write");

        // A Path binding beside it takes the note away, because then the item
        // really is reachable at a file touch.
        let mut reachable = sample_with("dir-plus-path", "the estimator rounds a quote up to whole cents");
        reachable.bindings = vec![
            Binding::Target { kind: TargetKind::Dir, value: "src/deep".to_string() },
            Binding::Target { kind: TargetKind::Path, value: "src/deep/mod.rs".to_string() },
        ];
        assert!(
            matches!(capacity(&store, &reachable).unwrap(), Capacity::Fine),
            "a path binding beside the directory makes it reachable"
        );
    }

    /// A pinned item is never in a pool: session start serves every pin in
    /// full and caps nothing, so a pin cannot take another pin's place.
    /// Counting them would refuse writes for crowding that does not exist.
    #[test]
    fn a_pinned_item_is_never_refused_for_crowding() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS + 3 {
            let mut pin = sample_with(&format!("pin-{i}"), DISTINCT[i % DISTINCT.len()]);
            pin.bindings = vec![Binding::Always];
            pin.severity = Some(Severity::Irreversible);
            declare(&mut store, "s", "l", "t", &pin).expect("fixture must store");
        }
        let mut another = sample_with("pin-more", "a webhook retry backs off before it gives up entirely");
        another.bindings = vec![Binding::Always];
        another.severity = Some(Severity::HouseStyle);
        declare(&mut store, "s", "l", "t", &another).expect("a pin never competes for a place");
    }

    /// The promise archiving makes: the words survive, the claim to fire does
    /// not. If this ever stops holding, archiving has quietly become a
    /// retraction with extra steps.
    #[test]
    fn archiving_keeps_the_words_and_drops_only_the_claim_to_fire() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample_with("measured-thing", "that reranker failed both gates");
        item.project = Some("some-project".to_string());
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        archive(&mut store, "s1", "l1", "test", "measured-thing", "the script it measured is gone").unwrap();

        let after = show(&store, "measured-thing").unwrap();
        assert_eq!(after.kind, Kind::Report, "it must stop being a kind that claims to fire");
        assert!(!after.kind.can_fire());
        assert_eq!(after.text, "that reranker failed both gates", "the words must survive untouched");
        assert_eq!(after.project.as_deref(), Some("some-project"), "the scope must survive");
        assert!(after.bindings.is_empty(), "a Report may carry no bindings");
        assert!(
            after.tags.iter().any(|t| t == "archived:the script it measured is gone"),
            "the reason must survive as a tag, or nobody can tell this from an accident: {:?}",
            after.tags
        );
    }

    /// An archived item stays in the live set. That is the whole difference
    /// from retracting: `lookup` must still find it.
    #[test]
    fn an_archived_item_is_still_live_and_findable() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("measured-thing", "that reranker failed both gates");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        archive(&mut store, "s1", "l1", "test", "measured-thing", "anchor gone").unwrap();

        assert!(
            show(&store, "measured-thing").is_ok(),
            "archiving must not remove the item - that is what retract is for"
        );
    }

    #[test]
    fn archiving_refuses_without_a_reason() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("thing", "some fact");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        assert!(matches!(
            archive(&mut store, "s1", "l1", "test", "thing", "   "),
            Err(WriteError::Refused(_))
        ));
        assert_eq!(show(&store, "thing").unwrap().kind, Kind::Orientation, "a refused archive changes nothing");
    }

    /// The defect this guards against: archiving a rule that can prove itself
    /// would throw away the only enforcement this memory actually has. A rule
    /// with a runnable check is the one kind that can block a wrong write.
    #[test]
    fn archiving_refuses_a_rule_that_can_still_prove_itself() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample_with("provable", "the config lives in config/app.toml");
        item.check = Some(Check::PathExists { path: "config/app.toml".to_string() });
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let err = archive(&mut store, "s1", "l1", "test", "provable", "anchor gone")
            .expect_err("a provable rule must not be archivable in one step");
        assert!(format!("{err:?}").contains("check"), "the refusal must name why: {err:?}");
        assert_eq!(show(&store, "provable").unwrap().kind, Kind::Orientation);
    }

    #[test]
    fn archiving_something_already_archived_is_refused_rather_than_repeated() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("thing", "some fact");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        archive(&mut store, "s1", "l1", "test", "thing", "anchor gone").unwrap();

        assert!(matches!(
            archive(&mut store, "s1", "l1", "test", "thing", "anchor gone"),
            Err(WriteError::Refused(_))
        ));
        let after = show(&store, "thing").unwrap();
        assert_eq!(
            after.tags.iter().filter(|t| t.starts_with("archived:")).count(),
            1,
            "a second archive must not stack a second reason tag"
        );
    }

    /// The defect this guards against, and it is the one that actually
    /// happened. Six rules anchored at a gitignored secrets file were swept
    /// into the archive as "dead anchors". They were not dead: the anchor was
    /// there so they fire the moment anybody touches that file. One of them
    /// said so in its own text and the sweep read paths, not sentences.
    #[test]
    fn archiving_refuses_an_anchor_that_is_absent_on_purpose() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample_with("guards-secrets", "this file is gitignored and must never be committed");
        item.tags = vec![DELIBERATE_ANCHOR_TAG.to_string()];
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let err = archive(&mut store, "s1", "l1", "test", "guards-secrets", "anchor resolves to nothing")
            .expect_err("an anchor marked deliberate must survive any sweep");
        assert!(format!("{err:?}").contains("deliberately absent"), "the refusal must say why: {err:?}");
        assert!(show(&store, "guards-secrets").unwrap().kind.can_fire(), "it must still fire");
    }

    #[test]
    fn restoring_brings_back_the_kind_and_the_anchor_and_marks_it_deliberate() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("guards-secrets", "this file is gitignored and must never be committed");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        archive(&mut store, "s1", "l1", "test", "guards-secrets", "swept by mistake").unwrap();

        restore_deliberate_anchor(
            &mut store,
            "s1",
            "l1",
            "test",
            "guards-secrets",
            Kind::Rule,
            "firmware/src/secrets.h",
        )
        .unwrap();

        let after = show(&store, "guards-secrets").unwrap();
        assert_eq!(after.kind, Kind::Rule);
        assert_eq!(
            after.bindings,
            vec![Binding::Target { kind: TargetKind::Path, value: "firmware/src/secrets.h".to_string() }]
        );
        assert!(after.tags.iter().any(|t| t == DELIBERATE_ANCHOR_TAG), "it must be marked: {:?}", after.tags);
        assert!(!after.tags.iter().any(|t| t.starts_with("archived:")), "the archive reason must go: {:?}", after.tags);

        // And now it is immune to the same mistake.
        assert!(matches!(
            archive(&mut store, "s1", "l1", "test", "guards-secrets", "anchor resolves to nothing"),
            Err(WriteError::Refused(_))
        ));
    }

    #[test]
    fn restoring_something_that_was_never_archived_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("thing", "some fact");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        assert!(matches!(
            restore_deliberate_anchor(&mut store, "s1", "l1", "test", "thing", Kind::Rule, "a/b.rs"),
            Err(WriteError::Refused(_))
        ));
    }

    #[test]
    fn archiving_refuses_a_reason_that_would_pollute_search() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("thing", "some fact");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let long = "x".repeat(ARCHIVE_REASON_LIMIT + 1);
        assert!(matches!(archive(&mut store, "s1", "l1", "test", "thing", &long), Err(WriteError::Refused(_))));
        assert!(matches!(
            archive(&mut store, "s1", "l1", "test", "thing", "two, reasons"),
            Err(WriteError::Refused(_))
        ));
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
