//! Read every LIVE item out of the store: single-headed, non-retracted,
//! body parses as canonical JSON. Shared by the select path (candidates for
//! a moment/target) and by `audit` (every declared item, served or not).
//!
//! Fails open on anything wrong with one entity (a diverged head, a body
//! that does not parse) by skipping that entity - reading never crashes and
//! never blocks (R5). This is the read half of the boundary; the write half
//! (model::store::declare/revise) is the one required to be loud.

use crate::input::ServeInput;
use model::item::{Item, TargetKind};
use thor_core::cas::compute_head_sets;
use thor_core::event_store::{Event, EventKind, EventStore};

/// One live item plus the id of the event log entity that carries it.
pub struct LiveItem {
    pub id: String,
    pub item: Item,
}

/// Every entity in the store whose current head is a single, non-retracted
/// revision with a body that parses as a valid `Item`. A diverged entity (more
/// than one head) or a body that fails to parse is skipped, never reported as
/// an error - this is a read path, not a write path.
///
/// THE DEFECT THIS CLOSES (2026-08-03). This is called on EVERY tool call (the
/// PreToolUse hook in serve/src/bin/serve.rs) and on every session start, and
/// it used to call `store.get_all_events()` - the WHOLE event log, tens of
/// thousands of events - and fold it with `compute_head_sets` from scratch on
/// every single call. That full-log fold was the diagnosed cost of the
/// moment-of-action hook (measured ~104ms p50, against 1.0's ~17ms, which read
/// a projection table instead of folding the log). The store already
/// maintains that same fold incrementally as it writes, in the `head_state`
/// table (see `core/src/event_store.rs`), kept current in the SAME
/// transaction as every append. This now reads THAT projection
/// (`live_items_from_projection`) instead of refolding the log, and falls
/// back to the exact old fold (`live_items_from_fold`) whenever the
/// projection is not current - a bypass writer (restore, a pre-M2 binary)
/// leaves `head_state` stale until the next append rebuilds it wholesale - so
/// correctness never depends on the projection being up to date, only speed
/// does.
pub fn live_items(store: &EventStore) -> Vec<LiveItem> {
    if store.heads_projection_current() {
        live_items_from_projection(store)
    } else {
        live_items_from_fold(store)
    }
}

/// The fast path (see `live_items`): read the current heads straight out of
/// the `head_state` projection instead of folding the whole log. One row per
/// current head, already ordered by entity id then rev hash (a `head_state`
/// primary-key-order scan), so an entity with more than one row for its id is
/// diverged - `head_count` is that entity's total row count, so this drops it
/// with the exact same `!= 1` rule the fold uses, without ever having to look
/// at the other head's body. Ordering matches `live_items_from_fold`'s sorted
/// entity ids because both are ascending over the same id space.
fn live_items_from_projection(store: &EventStore) -> Vec<LiveItem> {
    let Ok(rows) = store.projected_head_events() else { return Vec::new() };
    rows_to_live_items(rows)
}

/// Turn `projected_head_events`/`projected_head_events_for`-shaped rows into
/// `LiveItem`s - the one filter every projection-backed reader in this file
/// shares (a diverged or retracted row is dropped, never guessed at, and an
/// unparsable body is skipped rather than treated as an error, same as every
/// other reader on this boundary).
fn rows_to_live_items(rows: Vec<(Event, i64, Option<String>)>) -> Vec<LiveItem> {
    let mut out = Vec::new();
    for (event, head_count, _project) in rows {
        if head_count != 1 {
            continue; // diverged: resolve first, never guess which head is real
        }
        if matches!(event.kind, EventKind::FactRetracted) {
            continue; // a tombstone is not a live item
        }
        let Ok(item) = model::store::parse_body(&event.body) else { continue };
        out.push(LiveItem { id: event.entity_id.clone(), item });
    }
    out
}

/// The always-correct path (see `live_items`): the original whole-log fold.
/// Used only when the `head_state` projection is not current, so a stale or
/// bypassed projection can never change which items are reported live.
fn live_items_from_fold(store: &EventStore) -> Vec<LiveItem> {
    let Ok(events) = store.get_all_events() else { return Vec::new() };
    let heads = compute_head_sets(&events);
    let by_hash: std::collections::HashMap<&str, &Event> =
        events.iter().map(|e| (e.this_hash.as_str(), e)).collect();

    let mut out = Vec::new();
    let mut ids: Vec<&String> = heads.keys().collect();
    ids.sort(); // deterministic order: never depend on HashMap iteration order
    for id in ids {
        let head_set = &heads[id];
        if head_set.heads.len() != 1 {
            continue; // diverged: resolve first, never guess which head is real
        }
        let head_hash = head_set.heads.iter().next().expect("len checked above");
        let Some(head_event) = by_hash.get(head_hash.as_str()) else { continue };
        if matches!(head_event.kind, EventKind::FactRetracted) {
            continue; // a tombstone is not a live item
        }
        let Ok(item) = model::store::parse_body(&head_event.body) else { continue };
        out.push(LiveItem { id: id.clone(), item });
    }
    out
}

/// The wire string one `TargetKind` serialises to. `model::item::TargetKind`
/// derives `Serialize` with `#[serde(rename_all = "snake_case")]`; reading
/// that back through `serde_json` (rather than re-typing the mapping by
/// hand) guarantees this can never drift from whatever `model` actually
/// wrote into a stored item's body - the exact string
/// `thor_core::event_store`'s `item_binding.target_kind` column holds.
fn target_kind_wire(kind: TargetKind) -> String {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// The narrowed candidate pool for one `ServeInput`, read through the
/// binding projection instead of parsing every live item - `None` whenever
/// the projection is not current, so the caller always has a safe,
/// unconditional fallback (`candidates_for` below), exactly like
/// `live_items`'s own relationship with `heads_projection_current`.
///
/// THE DEFECT THIS CLOSES (2026-08-03). `live_items` already reads
/// `head_state` instead of folding the whole log, but it still PARSES every
/// one of the store's ~3800 live items into a full `Item` on every call -
/// the head fold was never the remaining cost on the moment-of-action hook,
/// the JSON parse was. A moment call only ever needs the items whose binding
/// could possibly match a specific action or a specific `TargetKind`; this
/// narrows to exactly that set FIRST via `item_binding` (maintained by
/// `core` alongside `head_state`, see `thor_core::event_store`'s own doc
/// comment on that table) and only then parses the result.
///
/// The narrowing can only ever OVER-include, never drop a true match: a
/// moment narrows by an EXACT action-string match (the same equality
/// `rank::binding_matches` uses for `Binding::Moment`, so this loses
/// nothing); a target narrows by `TargetKind` ONLY, never by value - the
/// exact value/normalisation comparison (`rank::target_matches`) still runs
/// unmodified on the fully parsed candidates this returns, exactly as it
/// would over the full list. Selection semantics are therefore IDENTICAL to
/// running `rank::select` over `live_items`'s full output; only how many
/// items get parsed to reach that answer changes.
fn candidates_from_projection(
    store: &EventStore,
    moments: &[String],
    target_kinds: &[String],
) -> Option<Vec<LiveItem>> {
    if !store.item_binding_projection_current() {
        return None;
    }
    let ids = store.projected_binding_candidates(moments, target_kinds).ok()?;
    let rows = store.projected_head_events_for(&ids).ok()?;
    Some(rows_to_live_items(rows))
}

/// Surface 2/3's candidate pool for one `ServeInput`: narrowed through the
/// binding projection when it is current (see `candidates_from_projection`),
/// or every live item when it is not - correctness never depends on which
/// path ran, only speed does.
pub fn candidates_for(store: &EventStore, input: &ServeInput) -> Vec<LiveItem> {
    let moments: Vec<String> = input.moments.iter().map(|a| a.as_str().to_string()).collect();
    let mut target_kinds: Vec<String> = input.targets.iter().map(|(k, _)| target_kind_wire(*k)).collect();
    target_kinds.sort();
    target_kinds.dedup();
    candidates_from_projection(store, &moments, &target_kinds).unwrap_or_else(|| live_items(store))
}

/// Surface 1's candidate pool: every live item bound `Always`, narrowed
/// through the binding projection when it is current, or every live item
/// when it is not. `session_start::select` re-derives kind/project scoping
/// on whatever this returns exactly as it always has, so narrowing here can
/// only ever hand it a smaller-but-sufficient input, never a different
/// answer.
pub fn always_candidates(store: &EventStore) -> Vec<LiveItem> {
    if !store.item_binding_projection_current() {
        return live_items(store);
    }
    let Ok(ids) = store.projected_always_entities() else { return live_items(store) };
    let Ok(rows) = store.projected_head_events_for(&ids) else { return live_items(store) };
    rows_to_live_items(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind, TargetKind};
    use model::store;

    fn item(id: &str, kind: Kind) -> Item {
        Item {
            id: id.to_string(),
            kind,
            text: "do the thing".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: if kind.can_fire() {
                Some("this synthetic fixture item turns out to be wrong".to_string())
            } else {
                None
            },
            check: None,
        }
    }

    #[test]
    fn a_declared_item_is_live() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("x1", Kind::Rule)).unwrap();
        let live = live_items(&db);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "x1");
    }

    #[test]
    fn a_retracted_item_is_not_live() {
        let mut db = EventStore::in_memory().unwrap();
        let it = item("x2", Kind::Rule);
        store::declare(&mut db, "s", "l", "a", &it).unwrap();
        db.append_mutate_checked("s", "l", "a", EventKind::FactRetracted, "x2", None, "tombstone")
            .unwrap();
        assert!(live_items(&db).is_empty(), "a retracted entity must not be reported live");
    }

    #[test]
    fn a_diverged_item_is_skipped_not_guessed() {
        let mut db = EventStore::in_memory().unwrap();
        // Two independent creates under the same id never happens through the
        // gate, but the fold must still not crash or pick a side for a
        // hand-crafted diverged log (defense in depth over the raw log).
        db.append_event("s", "l", "a", EventKind::FactCreated, "x3", None, "not json").unwrap();
        db.append_event("s", "l", "a", EventKind::FactRevised, "x3", Some("stale"), "also not json")
            .unwrap();
        assert!(live_items(&db).is_empty(), "a diverged entity must be skipped, never guessed at");
    }

    #[test]
    fn an_unparsable_body_is_skipped_not_an_error() {
        let mut db = EventStore::in_memory().unwrap();
        db.append_event("s", "l", "a", EventKind::FactCreated, "x4", None, "not json at all")
            .unwrap();
        assert!(live_items(&db).is_empty());
    }

    #[test]
    fn every_kind_is_returned_lookup_report_and_chunk_included() {
        // live_items is a plain reader: it is the SELECT path (rank.rs) and
        // the audit path, not this one, that must refuse to serve archive
        // kinds (Report, Lookup, Chunk) - both of them decide that through
        // the ONE definition, model::item::Kind::can_fire. This pins the
        // neutral-reader design live_items itself depends on: if a future
        // change quietly filtered archive kinds out here too, this count
        // would drop and this test would catch it.
        let mut db = EventStore::in_memory().unwrap();
        let mut report = item("x5", Kind::Report);
        report.bindings = vec![];
        store::declare(&mut db, "s", "l", "a", &report).unwrap();
        let mut lookup = item("x6", Kind::Lookup);
        lookup.bindings = vec![];
        lookup.key = Some("k".to_string());
        store::declare(&mut db, "s", "l", "a", &lookup).unwrap();
        let mut chunk = item("x7", Kind::Chunk);
        chunk.bindings = vec![];
        store::declare(&mut db, "s", "l", "a", &chunk).unwrap();
        let live = live_items(&db);
        assert_eq!(live.len(), 3);
    }

    #[test]
    fn iteration_order_is_deterministic() {
        let mut db = EventStore::in_memory().unwrap();
        // Distinct text per item (the near-duplicate gate refuses a second
        // live item of the same kind with the same or near-same text) and
        // deliberately NOT in id order: declared c, a, b, and each item's
        // own text sorts c, a, b too (alphabetically), so a row order that
        // matched TEXT order would read "c, a, b" here, not "a, b, c" - only
        // a sort keyed on id produces the order this test asserts.
        let texts = [("c", "back up the database nightly"), ("a", "rotate the api keys quarterly"), ("b", "run linting before every commit")];
        for (id, text) in texts {
            let mut it = item(id, Kind::Rule);
            it.text = text.to_string();
            store::declare(&mut db, "s", "l", "a", &it).unwrap();
        }
        let items = live_items(&db);
        let ids: Vec<&str> = items.iter().map(|li| li.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"], "id order must be stable, never hashmap order");
    }

    #[test]
    fn a_target_binding_round_trips() {
        let mut db = EventStore::in_memory().unwrap();
        let mut it = item("x7", Kind::Orientation);
        // Named project: ground 19 refuses a global item anchored at a source
        // file, and this test is about the binding round-tripping.
        it.project = Some("some-project".to_string());
        it.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "a/b.rs".to_string() }];
        store::declare(&mut db, "s", "l", "a", &it).unwrap();
        let live = live_items(&db);
        assert_eq!(live[0].item.bindings, it.bindings);
    }

    #[test]
    fn projection_read_matches_fold_over_plain_revised_retracted_and_diverged() {
        // THE DEFECT THIS CLOSES (2026-08-03): live_items now reads the
        // head_state projection (live_items_from_projection) instead of
        // folding the whole log (live_items_from_fold) on every call. This
        // pins that the fast path returns EXACTLY what the fold would, across
        // the cases that decide liveness: a plain live item, one whose head
        // moved under a revise, a tombstoned one (must be absent from both),
        // and a diverged one (must be absent from both) - so swapping the
        // read strategy could never quietly change which items are live.
        let mut db = EventStore::in_memory().unwrap();
        // p1: a plain live item. Its text must differ from p2's and p3's
        // (below) so none of these declares trips the near-duplicate gate -
        // all three are live Kind::Rule items at some point in this test.
        let mut p1 = item("p1", Kind::Rule);
        p1.text = "back up the database nightly".to_string();
        store::declare(&mut db, "s", "l", "a", &p1).unwrap();
        // p2: revised - its head must have moved off the original create.
        // The ORIGINAL text only needs to differ from p1's (p3 is declared
        // after p2 is already revised away from it); revise is never
        // subject to the near-duplicate gate, so the revised text is free
        // to be anything, and stays "do the other thing" since that exact
        // string is asserted below.
        let mut original = item("p2", Kind::Rule);
        original.text = "rotate the api keys quarterly".to_string();
        store::declare(&mut db, "s", "l", "a", &original).unwrap();
        let mut updated = original.clone();
        updated.text = "do the other thing".to_string();
        store::revise(&mut db, "s", "l", "a", &original, &updated).unwrap();
        // p3: retracted - a tombstone, must be absent from both paths. Its
        // text must differ from p1's and from p2's now-live revised text.
        let mut p3 = item("p3", Kind::Rule);
        p3.text = "run linting before every commit".to_string();
        store::declare(&mut db, "s", "l", "a", &p3).unwrap();
        db.append_mutate_checked("s", "l", "a", EventKind::FactRetracted, "p3", None, "tombstone")
            .unwrap();
        // p4: diverged - two independent heads (hand-crafted, as in
        // a_diverged_item_is_skipped_not_guessed above), must be absent too.
        db.append_event("s", "l", "a", EventKind::FactCreated, "p4", None, "not json").unwrap();
        db.append_event("s", "l", "a", EventKind::FactRevised, "p4", Some("stale"), "also not json")
            .unwrap();

        assert!(db.heads_projection_current(), "no bypass writes here: projection must be current");
        let fast = live_items_from_projection(&db);
        let slow = live_items_from_fold(&db);
        let fast_ids: Vec<&str> = fast.iter().map(|li| li.id.as_str()).collect();
        let slow_ids: Vec<&str> = slow.iter().map(|li| li.id.as_str()).collect();
        assert_eq!(fast_ids, slow_ids, "the projection read must match the fold, same items, same order");
        assert_eq!(fast_ids, vec!["p1", "p2"], "p3 (retracted) and p4 (diverged) must both be absent");
        assert_eq!(fast[1].item.text, "do the other thing", "p2 must carry the REVISED body, not the original");
    }
}
