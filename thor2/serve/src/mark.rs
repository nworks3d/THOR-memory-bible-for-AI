//! Record that a served item actually helped - a deliberate, explicit action
//! by whoever consumed it, never an automatic side effect of delivery (that
//! is `deliver::record_delivery`'s job). Unlike delivery telemetry, marking
//! is a "write and declare" action (R5): a person or agent is asking THOR to
//! remember this, so a failure here is surfaced, never swallowed.
//!
//! This is the write half of the decay doctrine (`crate::decay`): an item
//! called noise twice since anyone last called it useful stops reaching an
//! injection surface, and recording a mark here - at any point, even after
//! decay has already taken hold - clears the noise recorded before it,
//! because decay is re-derived from the log fresh on every call
//! (`crate::usefulness::noise_since_last_useful`), never a persisted flag to
//! flip back. It does NOT clear what comes after: the latest verdict is the
//! one that counts, which is what makes a second opinion possible at all.

use model::marked::{ItemMarkedNoise, ItemMarkedUseful};
use thor_core::event_store::{Event, EventKind, EventStore};

/// Append one `item_marked_useful` event for `id`. Errors are surfaced (never
/// swallowed): marking is an intentional write, not passive telemetry.
pub fn record_useful(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    marked_at: &str,
    id: &str,
) -> anyhow::Result<Event> {
    let body = serde_json::to_string(&ItemMarkedUseful { marked_at: marked_at.to_string() })?;
    store.append_event(session_id, lineage_id, actor, EventKind::ItemMarkedUseful, id, None, &body)
}

/// Append one `item_marked_noise` event for `id`: the reader saying this item
/// did not belong where it fired. Errors are surfaced, exactly like
/// `record_useful` - a judgement is an intentional write.
///
/// This is the signal `crate::decay` needs and never had. Decay by silence
/// was falsified on its first production day (see that module); an explicit
/// negative is a real observation, where the absence of a positive is not.
pub fn record_noise(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    marked_at: &str,
    id: &str,
) -> anyhow::Result<Event> {
    let body = serde_json::to_string(&ItemMarkedNoise { marked_at: marked_at.to_string() })?;
    store.append_event(session_id, lineage_id, actor, EventKind::ItemMarkedNoise, id, None, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usefulness::ever_marked_useful;

    #[test]
    fn recording_a_mark_is_queryable_immediately_after_the_write() {
        let mut store = EventStore::in_memory().unwrap();
        record_useful(&mut store, "s", "l", "a", "2026-08-02T12:00:00Z", "x1").unwrap();
        assert!(ever_marked_useful(&store).contains("x1"));
    }

    #[test]
    fn recording_a_mark_never_touches_the_items_head_set() {
        // Same guarantee record_delivery already pins for ItemServed: marking
        // must never change what live_items reports for that entity.
        use model::item::{Binding, Item, Kind};
        let mut store = EventStore::in_memory().unwrap();
        let item = Item {
            id: "x2".to_string(),
            kind: Kind::Rule,
            text: "do the thing".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this item turns out to be wrong for this synthetic fixture".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        record_useful(&mut store, "s", "l", "a", "2026-08-02T12:00:00Z", "x2").unwrap();
        let live = crate::live::live_items(&store);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].item, item);
    }

    #[test]
    fn marking_an_unknown_id_never_panics() {
        // Marking may arrive for an id this store never declared (a stale
        // reference from an earlier session) - it must be a harmless, real
        // event, not a crash.
        let mut store = EventStore::in_memory().unwrap();
        record_useful(&mut store, "s", "l", "a", "2026-08-02T12:00:00Z", "ghost").unwrap();
        assert!(ever_marked_useful(&store).contains("ghost"));
    }
}
