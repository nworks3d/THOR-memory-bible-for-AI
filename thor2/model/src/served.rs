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
//!
//! It is also the read side `model::store::capacity_for_revise` needs: a
//! recent-servings COUNT, windowed against a real clock. `serve::usefulness`
//! already folds this same event kind, but only as (kind, entity_id) pairs -
//! no timestamp, so no window. Reading `served_at` back out belongs here
//! instead of growing that fold, because `model` may never depend on `serve`
//! (`serve` already depends on `model` - see each crate's own Cargo.toml -
//! so the reverse edge would be a cycle) and this is exactly the file that
//! already owns the body shape being read.

use serde::{Deserialize, Serialize};
use thor_core::event_store::{EventKind, EventStore};

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

/// How many days back `served_count` looks by default - see its own doc
/// comment, and `model::store::capacity_for_revise`'s, for why a revise that
/// kept its bindings can trust a count over this window as a measurement
/// rather than a prediction.
pub const SERVED_WINDOW_DAYS: i64 = 30;

/// How many `item_served` events `item_id` has on record in the
/// `window_days` days up to and including `now` (an ISO-8601 UTC timestamp,
/// `served_at`'s own shape - e.g. "2026-09-12T00:00:00Z").
///
/// Fails open to 0 on anything that does not parse the way this crate's own
/// writer produces it - a broken log, a hand-edited body, a `now` that is not
/// this exact shape. Safe to fail open here: the only thing this number ever
/// feeds is the WORDING of an advisory note, never a refusal, and 0 is the
/// same answer this note already gives for "never served" - see
/// `serve::usefulness`'s own folds for the identical fail-open reasoning
/// against the same log.
pub fn served_count(store: &EventStore, item_id: &str, now: &str, window_days: i64) -> usize {
    let Some(now_secs) = unix_from_iso8601(now) else { return 0 };
    let threshold = now_secs - window_days * 86_400;
    let Ok(events) = store.get_events_by_entity(item_id) else { return 0 };
    events
        .iter()
        .filter(|e| e.kind == EventKind::ItemServed)
        .filter_map(|e| serde_json::from_str::<ItemServed>(&e.body).ok())
        .filter_map(|body| unix_from_iso8601(&body.served_at))
        .filter(|&secs| secs >= threshold)
        .count()
}

/// Days since the Unix epoch -> (year, month, day), proleptic Gregorian,
/// inverted: Howard Hinnant's `days_from_civil`.
/// http://howardhinnant.github.io/date_algorithms.html
///
/// `serve::time` already carries `civil_from_days`, the OPPOSITE direction
/// (seconds -> stamp, for writing `served_at` itself), with no chrono
/// dependency for the same reason given there: one conversion does not earn
/// a new dependency. This is the other direction - stamp -> seconds, needed
/// here to compare two of them - and `model` cannot reach `serve`'s copy
/// without depending on `serve` (see this file's own top comment), so it is
/// a second, independent copy of the same well-known algorithm rather than a
/// shared one neither crate is allowed to own.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Parse the one shape `served_at` (and every `now` this module is ever
/// handed) is written in - `YYYY-MM-DDTHH:MM:SSZ`, exactly 20 bytes, always
/// UTC - into seconds since the Unix epoch. `None` on anything else: a
/// fail-open read, never a panic, for a body this crate did not necessarily
/// write itself - see `served_count`'s own doc comment for why 0 is the safe
/// fallback everywhere a `None` here ends up.
fn unix_from_iso8601(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let min: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec)
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

    /// Known values, cross-checked against `serve::time::iso8601_from_unix`'s
    /// own test fixture (`serve/src/time.rs`) - same instants, opposite
    /// direction, so a mistake in either crate's copy of the algorithm would
    /// show up as a disagreement between the two test suites.
    #[test]
    fn unix_from_iso8601_matches_known_timestamps() {
        assert_eq!(unix_from_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_from_iso8601("2023-11-14T22:13:20Z"), Some(1_700_000_000));
        assert_eq!(unix_from_iso8601("2030-01-01T00:00:00Z"), Some(1_893_456_000));
        assert_eq!(unix_from_iso8601("2025-08-02T13:20:00Z"), Some(1_754_140_800));
        assert_eq!(unix_from_iso8601("1974-12-31T00:00:00Z"), Some(157_680_000));
    }

    #[test]
    fn unix_from_iso8601_rejects_a_malformed_stamp() {
        assert_eq!(unix_from_iso8601(""), None);
        assert_eq!(unix_from_iso8601("not-a-date"), None);
        assert_eq!(unix_from_iso8601("2026-13-01T00:00:00Z"), None, "month 13 does not exist");
        assert_eq!(unix_from_iso8601("2026-09-12 00:00:00Z"), None, "a space is not a 'T'");
    }

    fn record_served(store: &mut EventStore, id: &str, served_at: &str) {
        let body = serde_json::to_string(&ItemServed { served_at: served_at.to_string() }).unwrap();
        store.append_event("s", "l", "t", EventKind::ItemServed, id, None, &body).unwrap();
    }

    #[test]
    fn served_count_is_zero_for_an_item_never_served() {
        let store = EventStore::in_memory().unwrap();
        assert_eq!(served_count(&store, "never-served", "2026-09-12T00:00:00Z", SERVED_WINDOW_DAYS), 0);
    }

    #[test]
    fn served_count_ignores_a_different_items_servings() {
        let mut store = EventStore::in_memory().unwrap();
        record_served(&mut store, "other", "2026-09-10T00:00:00Z");
        assert_eq!(served_count(&store, "mine", "2026-09-12T00:00:00Z", SERVED_WINDOW_DAYS), 0);
    }

    #[test]
    fn served_count_counts_only_servings_inside_the_window() {
        let mut store = EventStore::in_memory().unwrap();
        let now = "2026-09-12T00:00:00Z";
        record_served(&mut store, "i1", "2026-08-01T00:00:00Z"); // 42 days back: outside
        record_served(&mut store, "i1", "2026-08-20T00:00:00Z"); // 23 days back: inside
        record_served(&mut store, "i1", "2026-09-10T00:00:00Z"); // 2 days back: inside
        assert_eq!(served_count(&store, "i1", now, SERVED_WINDOW_DAYS), 2);
    }
}
