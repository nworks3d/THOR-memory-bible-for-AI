//! R3: delivery is observable. Every item actually shown in a block writes an
//! `ItemServed` event (the variant already exists in `thor_core::event_store`,
//! unwired until this crate) so "when did this last fire" and "how often" are
//! answerable, and "declared but never delivered" is a query (see `audit.rs`).
//!
//! This is called ONLY from the `hook` channel - the real production
//! delivery boundary. `check`/`why` call the same select+render path to
//! PREVIEW a block but must not count as a real firing, or "how often did
//! this actually fire" would be inflated by every dry run a human tried.
//! This split is a design choice the brief itself does not spell out (it says
//! "elke levering", every delivery) - flagged here rather than assumed silently.
//!
//! Fails open exactly like the rest of the read/serve boundary: a failed
//! write here must never stop the block that was already decided from
//! reaching stdout, and must never make `hook` speak or exit non-zero.

use model::served::ItemServed;
use thor_core::event_store::{EventKind, EventStore};

/// Record one `ItemServed` event per shown item id. Errors are swallowed
/// (fail open): a broken log must not turn a successful selection into a
/// failed delivery, and this is a telemetry side effect, not the delivery
/// itself.
pub fn record_delivery(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    served_at: &str,
    ids: &[String],
) {
    for id in ids {
        let body = match serde_json::to_string(&ItemServed { served_at: served_at.to_string() }) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let _ = store.append_event(session_id, lineage_id, actor, EventKind::ItemServed, id, None, &body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serving::serving_stats;

    #[test]
    fn recording_delivery_is_queryable_immediately_after_the_write() {
        let mut store = EventStore::in_memory().unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["x1".to_string()]);
        let stats = serving_stats(&store);
        let s = stats.get("x1").expect("must be queryable right after the write, per R3");
        assert_eq!(s.times_served, 1);
        assert_eq!(s.last_served_at.as_deref(), Some("2026-08-02T12:00:00Z"));
    }

    #[test]
    fn recording_delivery_never_touches_the_items_head_set() {
        // ItemServed is head-neutral (core::cas/auditor already treat it that
        // way); this pins it from the serve side too: recording a delivery
        // must never change what `live_items` reports for that entity.
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
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["x2".to_string()]);
        let live = crate::live::live_items(&store);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].item, item);
    }

    #[test]
    fn recording_delivery_for_an_unknown_id_never_panics() {
        // hook is fail-open at the boundary; the delivery write for an id
        // that was never declared must be a harmless no-op-ish event, not a
        // crash (defense in depth even though the caller only ever passes
        // ids that select() just returned).
        let mut store = EventStore::in_memory().unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["ghost".to_string()]);
        let stats = serving_stats(&store);
        assert_eq!(stats.get("ghost").map(|s| s.times_served), Some(1));
    }
}
