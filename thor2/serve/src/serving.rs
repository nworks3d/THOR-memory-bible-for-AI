//! The query side of R3: fold every `ItemServed` event in the log into, per
//! item id, how many times it fired and when it last fired. This is what
//! makes "declared but never delivered" answerable (see `audit.rs`), and it
//! is a plain fold over already-hashed fields - no sidecar, nothing to fall
//! out of sync with the log it reads.

use model::served::ItemServed;
use std::collections::HashMap;
use thor_core::event_store::{EventKind, EventStore};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServingStats {
    pub times_served: usize,
    /// ISO-8601; comparable lexically because that is also chronological
    /// order for this format.
    pub last_served_at: Option<String>,
}

/// Fold every `ItemServed` event into per-entity counts and last-fired time.
/// Fails open like every reader here: a broken log yields an empty map
/// rather than an error, since this backs `audit`, a diagnostic surface, not
/// a write path.
pub fn serving_stats(store: &EventStore) -> HashMap<String, ServingStats> {
    let Ok(events) = store.get_all_events() else { return HashMap::new() };
    let mut out: HashMap<String, ServingStats> = HashMap::new();
    for event in events {
        if event.kind != EventKind::ItemServed {
            continue;
        }
        let served_at: Option<String> =
            serde_json::from_str::<ItemServed>(&event.body).ok().map(|b| b.served_at);
        let entry = out.entry(event.entity_id.clone()).or_default();
        entry.times_served += 1;
        if let Some(at) = served_at {
            if entry.last_served_at.as_deref().is_none_or(|prev| at.as_str() > prev) {
                entry.last_served_at = Some(at);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_multiple_deliveries_of_the_same_item() {
        let mut store = EventStore::in_memory().unwrap();
        for ts in ["2026-08-01T00:00:00Z", "2026-08-02T00:00:00Z"] {
            crate::deliver::record_delivery(&mut store, "s", "l", "serve", ts, &["x1".to_string()]);
        }
        let stats = serving_stats(&store);
        assert_eq!(stats["x1"].times_served, 2);
        assert_eq!(stats["x1"].last_served_at.as_deref(), Some("2026-08-02T00:00:00Z"));
    }

    #[test]
    fn last_served_at_is_the_maximum_not_the_last_written() {
        // Events could in principle be folded out of chronological order
        // (e.g. a replayed/shipped log); "last fired" must mean the maximum
        // timestamp, not "whichever event happened to be seen last".
        let mut store = EventStore::in_memory().unwrap();
        crate::deliver::record_delivery(&mut store, "s", "l", "serve", "2026-08-02T00:00:00Z", &["x1".to_string()]);
        crate::deliver::record_delivery(&mut store, "s", "l", "serve", "2026-08-01T00:00:00Z", &["x1".to_string()]);
        let stats = serving_stats(&store);
        assert_eq!(stats["x1"].last_served_at.as_deref(), Some("2026-08-02T00:00:00Z"));
    }

    #[test]
    fn an_item_never_served_has_no_entry() {
        let store = EventStore::in_memory().unwrap();
        assert!(serving_stats(&store).is_empty());
    }
}
