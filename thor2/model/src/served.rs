//! The shape of an `item_served` event, defined here so a serve path only
//! has to call `EventStore::append_event` with it, never redesign what it
//! carries.
//!
//! Wired by crate `serve` (`serve::deliver::record_delivery`), from the
//! `hook` channel only - the real production delivery boundary; `check`/
//! `why` preview the same block without counting as a firing. This module
//! stays the single definition of the body shape; `serve::serving` is the
//! query side (per-item count and last-fired time) that folds these events
//! back out of the log.

use serde::{Deserialize, Serialize};

/// The event kind name this body is meant for
/// (`thor_core::event_store::EventKind::ItemServed::as_str()`).
pub const ITEM_SERVED_KIND: &str = "item_served";

/// The body of an `item_served` event. `served_at` alone answers "when was
/// this item last fired": the log's own `entity_id` column already says
/// WHICH item, so it is not repeated here, and "how often" is answered by
/// counting events of this kind for that entity_id - no counter field is
/// needed in the body for that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemServed {
    /// ISO-8601 timestamp of the moment this item was handed to a gate.
    pub served_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_served_round_trips() {
        let body = ItemServed { served_at: "2026-08-02T12:00:00Z".to_string() };
        let json = serde_json::to_string(&body).unwrap();
        let back: ItemServed = serde_json::from_str(&json).unwrap();
        assert_eq!(body, back);
    }
}
