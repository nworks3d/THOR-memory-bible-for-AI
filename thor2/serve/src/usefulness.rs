//! The query side of "was this item ever marked useful" - a plain fold over
//! `ItemMarkedUseful` events in the log, the same shape `serving::serving_stats`
//! already uses for `ItemServed`. Existence only: decay
//! (`crate::decay::DecayContext`) only ever asks "ever, yes or no", never "how
//! many times" - a mark is a permanent, one-way cancellation of decay for that
//! item, not a score.

use std::collections::{HashMap, HashSet};
use thor_core::event_store::{EventKind, EventStore};

/// Every entity id with at least one `ItemMarkedUseful` event in the log.
/// Fails open like every other reader on this boundary (see
/// `serving::serving_stats`): a broken log yields an empty set rather than an
/// error, since this backs a serve-path decision, not a write path.
///
/// Reads `EventStore::event_kinds` (kind + entity_id only), not
/// `get_all_events`: this fold never looks at a body, a hash, or an actor, so
/// there is no reason to pay for a fully materialized `Event` - body and
/// body_ch included - on every row of the whole log to get there.
pub fn ever_marked_useful(store: &EventStore) -> HashSet<String> {
    let Ok(events) = store.event_kinds() else { return HashSet::new() };
    events
        .into_iter()
        .filter(|(kind, _)| *kind == EventKind::ItemMarkedUseful)
        .map(|(_, entity_id)| entity_id)
        .collect()
}

/// How many times each entity has been called noise, folded from
/// `ItemMarkedNoise` events. A COUNT, not existence: one stray judgement must
/// not retire a rule, a repeated one should - see `crate::decay` for the
/// threshold and where it comes from.
///
/// Fails open like every other reader on this boundary: a broken log yields
/// an empty map, so nothing is ever retired because the log could not be
/// read. Reads `EventStore::event_kinds`, same reasoning as
/// `ever_marked_useful` above.
pub fn noise_counts(store: &EventStore) -> HashMap<String, usize> {
    let Ok(events) = store.event_kinds() else { return HashMap::new() };
    let mut out: HashMap<String, usize> = HashMap::new();
    for (kind, entity_id) in events {
        if kind == EventKind::ItemMarkedNoise {
            *out.entry(entity_id).or_default() += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_never_marked_is_absent_from_the_set() {
        let store = EventStore::in_memory().unwrap();
        assert!(ever_marked_useful(&store).is_empty());
    }

    #[test]
    fn a_marked_item_is_present_in_the_set() {
        let mut store = EventStore::in_memory().unwrap();
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "x1").unwrap();
        let marked = ever_marked_useful(&store);
        assert!(marked.contains("x1"));
    }

    #[test]
    fn marking_the_same_item_twice_still_yields_one_membership() {
        let mut store = EventStore::in_memory().unwrap();
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "x1").unwrap();
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-03T00:00:00Z", "x1").unwrap();
        let marked = ever_marked_useful(&store);
        assert_eq!(marked.len(), 1);
        assert!(marked.contains("x1"));
    }

    #[test]
    fn marking_one_item_never_marks_another() {
        let mut store = EventStore::in_memory().unwrap();
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "x1").unwrap();
        let marked = ever_marked_useful(&store);
        assert!(!marked.contains("x2"));
    }
}
