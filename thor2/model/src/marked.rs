//! The shape of an `item_marked_useful` event - the other half of R3's
//! observability doctrine, extended for decay: "this item actually helped"
//! needs to be an event in the log for the same reason `ItemServed` is, so
//! "was this item EVER marked useful" is a query over the log
//! (`serve::usefulness::ever_marked_useful`), never a sidecar flag that can
//! fall out of sync with it.
//!
//! Wired by crate `serve` (`serve::mark::record_useful`). This module stays
//! the single definition of the body shape, exactly like `served` does for
//! `ItemServed`.

use serde::{Deserialize, Serialize};

/// The event kind name this body is meant for
/// (`thor_core::event_store::EventKind::ItemMarkedUseful::as_str()`).
pub const ITEM_MARKED_USEFUL_KIND: &str = "item_marked_useful";

/// The body of an `item_marked_useful` event. Same shape as `ItemServed` for
/// the same reason: the log's own `entity_id` already says WHICH item, so
/// nothing else needs to be carried. Decay only ever asks "was this item
/// EVER marked useful" (existence, not a count) - `marked_at` is kept purely
/// as an audit trail for a human reading the raw log, mirroring `ItemServed`'s
/// own `served_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemMarkedUseful {
    /// ISO-8601 timestamp of the moment this item was marked useful.
    pub marked_at: String,
}

/// The event kind name a noise judgement's body is meant for
/// (`thor_core::event_store::EventKind::ItemMarkedNoise::as_str()`).
pub const ITEM_MARKED_NOISE_KIND: &str = "item_marked_noise";

/// The body of an `item_marked_noise` event: the reader saying "this did not
/// belong where it fired".
///
/// Unlike `ItemMarkedUseful`, decay asks for a COUNT of these, not their
/// existence - one stray judgement should not retire a rule, a repeated one
/// should. The log's own event count answers that, so this body still carries
/// nothing but its timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemMarkedNoise {
    /// ISO-8601 timestamp of the moment this item was called noise.
    pub marked_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_marked_useful_round_trips() {
        let body = ItemMarkedUseful { marked_at: "2026-08-02T12:00:00Z".to_string() };
        let json = serde_json::to_string(&body).unwrap();
        let back: ItemMarkedUseful = serde_json::from_str(&json).unwrap();
        assert_eq!(body, back);
    }
}
