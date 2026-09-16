//! Where `sync ship` (`crate::transport::push_once`) records that it last
//! completed without error - a small sidecar beside the store, the same
//! shape every other per-store sidecar in this workspace already takes (see
//! `serve::absent_guard::default_marker_path`/`default_stale_path`): one
//! fixed file name in the store's own directory, never derived from the
//! store's own file name, never inside a project.
//!
//! THE DEFECT THIS FILE EXISTS TO CLOSE. Before it, nothing on disk said
//! when a ship had last actually succeeded. On 2026-08-17 the receiver on
//! the owner's NAS began refusing the hourly ship, the client hung instead
//! of failing (see `transport::CONNECT_TIMEOUT`/`REQUEST_TIMEOUT`), and the
//! scheduled task sat in a ghost "Running" state for four weeks with
//! nothing anywhere - not the task, not a log, not a health check - saying
//! the copy on the NAS had gone stale. Only a person noticing the date by
//! hand found it. `ops::health::ship_line` and the `backup` binary's own
//! one-line summary both read this file so staleness is something a report
//! says out loud instead of something only a calendar catches.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One completed, error-free ship: when, and where the receiver stood
/// immediately afterwards. Also carries the MOST RECENT ATTEMPT, success or
/// not, so a ship that is failing can be told apart from one that simply has
/// nothing new to send - see `ops::health::ship_line`'s own doc comment for
/// what each combination of fields means on the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ShipState {
    /// Unix seconds (`SystemTime::now()` at the moment the LAST SUCCESSFUL
    /// ship completed without error). 0 when this machine has never shipped
    /// successfully - a sidecar can exist with only a failed attempt on it.
    #[serde(default)]
    pub completed_unix: u64,
    /// The receiver's `contiguous_seq` right after the last successful ship
    /// (`transport::PushSummary::final_cursor`) - not a fact about the ship
    /// itself, but the one number that tells someone comparing the two
    /// stores by hand how far the receiver actually got.
    #[serde(default)]
    pub receiver_seq: i64,
    /// The highest local event seq the last successful ship is known to
    /// have covered. Always equal to `receiver_seq` at that same success (a
    /// successful `push_once` only ever returns once the receiver has
    /// caught up to our own tip - see its own doc comment), kept as its own
    /// field so `ship_line` reads a value whose contract is pinned to "how
    /// far our OWN log got shipped" rather than reusing `receiver_seq`'s
    /// documented purpose above. `None` on a sidecar written before this
    /// field existed.
    #[serde(default)]
    pub covered_seq: Option<i64>,
    /// When the MOST RECENT `sync ship` attempt ran, success or failure.
    /// `None` on a sidecar written before this field existed - `ship_line`
    /// treats that the same as "no attempt information", and falls back to
    /// the plain success-age text it always printed.
    #[serde(default)]
    pub last_attempt_unix: Option<u64>,
    /// One line saying why the MOST RECENT attempt failed, or `None` when
    /// it succeeded. A success always clears this (see `record_success_at`)
    /// - so this being `Some` means precisely "the last attempt failed",
    /// never a stale error a later success already overwrote.
    #[serde(default)]
    pub last_attempt_error: Option<String>,
}

/// The sidecar's path: one fixed name beside `db`, never derived from `db`'s
/// own file name - only one ship state is ever kept per store, the same
/// singular shape `backup_to_repo`'s own debounce state takes (there, the
/// state lives in git history instead of a sidecar; here there is no git
/// history to read, so this is the smallest file that fits).
pub fn path_for(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("sync-ship-state.json")
}

/// Read the last recorded successful ship, or `None` when this machine has
/// never shipped (no sidecar) or the sidecar cannot be parsed. Fail-open,
/// the same stance every sidecar reader in this workspace takes (see
/// `absent_guard::read_blocked`/`read_stale`): a missing or corrupt sidecar
/// means "nothing recorded yet", never a crash and never a false alarm.
pub fn read(db: &Path) -> Option<ShipState> {
    let text = std::fs::read_to_string(path_for(db)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Record that a ship completed without error just now, at `receiver_seq`.
/// Overwrites whatever was recorded before - only the LAST success matters
/// to a staleness check, never a history of every one that ever happened.
/// Also clears any failure recorded by `record_failure` - a recovered ship
/// must stop alarming, not keep naming a reason that no longer applies.
pub fn record_success(db: &Path, receiver_seq: i64) -> std::io::Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    record_success_at(db, receiver_seq, now)
}

/// `record_success`, with the completion time given explicitly - the seam a
/// test (or `ops::health`'s own tests) uses to write a state that is already
/// hours old without actually waiting for them.
pub fn record_success_at(db: &Path, receiver_seq: i64, completed_unix: u64) -> std::io::Result<()> {
    let state = ShipState {
        completed_unix,
        receiver_seq,
        // A successful `push_once` only ever returns once the receiver has
        // caught up to our own local tip, so `receiver_seq` at this exact
        // moment already IS "the highest local seq this ship covered" - see
        // `ShipState::covered_seq`'s own doc comment.
        covered_seq: Some(receiver_seq),
        last_attempt_unix: Some(completed_unix),
        last_attempt_error: None,
    };
    write(db, &state)
}

/// Record that a `sync ship` attempt just failed, with a one-line `reason`.
/// Preserves whatever the LAST SUCCESS recorded (`completed_unix`/
/// `receiver_seq`/`covered_seq` are untouched) - a failed attempt ships
/// nothing, so it must never be mistaken for one that did. Fails open on a
/// sidecar that cannot be read back (a fresh, empty `ShipState`, the same
/// as a machine that has never shipped at all) rather than losing the
/// failure it exists to record.
pub fn record_failure(db: &Path, reason: &str) -> std::io::Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    record_failure_at(db, reason, now)
}

/// `record_failure`, with the attempt time given explicitly - the same test
/// seam `record_success_at` has, and for the same reason.
pub fn record_failure_at(db: &Path, reason: &str, attempted_unix: u64) -> std::io::Result<()> {
    let mut state = read(db).unwrap_or_default();
    state.last_attempt_unix = Some(attempted_unix);
    state.last_attempt_error = Some(one_line(reason));
    write(db, &state)
}

/// Collapse `reason` to one line - the sidecar promises `ship_line`'s reader
/// "a one-line reason", not however many lines an underlying error (an
/// `anyhow` chain, in practice) happened to carry.
fn one_line(reason: &str) -> String {
    reason.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn write(db: &Path, state: &ShipState) -> std::io::Result<()> {
    // `Vec<u8>` serialization of this struct cannot fail in practice;
    // unwrap_or only so a future field that COULD fail to serialize
    // degrades to an empty object (parsed back as defaults by `read`)
    // rather than panicking a scheduled task on its way out the door.
    let json = serde_json::to_string(state).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path_for(db), json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_is_none_when_this_machine_has_never_shipped() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert_eq!(read(&db), None);
    }

    #[test]
    fn a_recorded_success_reads_back_identical() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_success_at(&db, 42, 1_000_000).unwrap();
        assert_eq!(
            read(&db),
            Some(ShipState {
                completed_unix: 1_000_000,
                receiver_seq: 42,
                covered_seq: Some(42),
                last_attempt_unix: Some(1_000_000),
                last_attempt_error: None,
            })
        );
    }

    /// Only the LAST success is ever kept - a staleness check has no use for
    /// a history, and keeping one would be the "smallest state that fits"
    /// rule's opposite.
    #[test]
    fn a_second_success_overwrites_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_success_at(&db, 1, 1_000_000).unwrap();
        record_success_at(&db, 2, 2_000_000).unwrap();
        assert_eq!(
            read(&db),
            Some(ShipState {
                completed_unix: 2_000_000,
                receiver_seq: 2,
                covered_seq: Some(2),
                last_attempt_unix: Some(2_000_000),
                last_attempt_error: None,
            })
        );
    }

    /// THE BACKWARD-COMPATIBILITY GUARANTEE step 1 requires: a sidecar
    /// written by the code before this file grew the new fields must still
    /// parse, with every new field defaulting rather than failing to parse
    /// at all.
    #[test]
    fn an_old_sidecar_without_the_new_fields_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        std::fs::write(path_for(&db), r#"{"completed_unix":1000000,"receiver_seq":7}"#).unwrap();
        let state = read(&db).expect("an old two-field sidecar must still parse");
        assert_eq!(state.completed_unix, 1_000_000);
        assert_eq!(state.receiver_seq, 7);
        assert_eq!(state.covered_seq, None);
        assert_eq!(state.last_attempt_unix, None);
        assert_eq!(state.last_attempt_error, None);
    }

    /// A failed attempt must never move what the last SUCCESS recorded -
    /// only the attempt fields do, so a reader can always tell "when did we
    /// last actually get data off this machine" from "when did we last try".
    #[test]
    fn a_failed_attempt_preserves_the_last_success_but_records_the_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_success_at(&db, 42, 1_000_000).unwrap();
        record_failure_at(&db, "receiver rejected the shared secret (401)", 1_003_600).unwrap();
        let state = read(&db).unwrap();
        assert_eq!(state.completed_unix, 1_000_000, "the last SUCCESS must not move");
        assert_eq!(state.receiver_seq, 42);
        assert_eq!(state.covered_seq, Some(42));
        assert_eq!(state.last_attempt_unix, Some(1_003_600));
        assert_eq!(state.last_attempt_error.as_deref(), Some("receiver rejected the shared secret (401)"));
    }

    /// The recovery half: a success after a failure must clear the failure,
    /// or a fixed ship would keep alarming about a reason that no longer
    /// applies.
    #[test]
    fn a_success_after_a_failure_clears_the_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_failure_at(&db, "connection refused", 1_000_000).unwrap();
        record_success_at(&db, 9, 1_000_500).unwrap();
        let state = read(&db).unwrap();
        assert_eq!(state.last_attempt_error, None, "a recovered ship must stop alarming");
        assert_eq!(state.last_attempt_unix, Some(1_000_500));
        assert_eq!(state.completed_unix, 1_000_500);
    }

    /// A machine whose very FIRST attempt fails has no prior success to
    /// preserve - `completed_unix` stays at its default (0), which
    /// `ship_line` reads as "no successful ship yet" rather than a bogus
    /// multi-decade age.
    #[test]
    fn a_failed_first_attempt_with_no_prior_success_leaves_completed_unix_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_failure_at(&db, "timed out", 1_000_000).unwrap();
        let state = read(&db).unwrap();
        assert_eq!(state.completed_unix, 0);
        assert_eq!(state.receiver_seq, 0);
        assert_eq!(state.covered_seq, None);
        assert_eq!(state.last_attempt_error.as_deref(), Some("timed out"));
    }

    /// The sidecar promises "a one-line reason" - a multi-line error (an
    /// `anyhow` chain, in practice) must collapse to one before it lands on
    /// disk, not just at the point `ship_line` happens to render it.
    #[test]
    fn a_multiline_reason_collapses_to_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_failure_at(&db, "line one\nline two\n  line three", 1_000_000).unwrap();
        let state = read(&db).unwrap();
        let reason = state.last_attempt_error.unwrap();
        assert_eq!(reason.lines().count(), 1);
        assert_eq!(reason, "line one line two line three");
    }

    /// Fail-open, like every other sidecar reader in this workspace: a file
    /// that exists but is not valid JSON must read as "nothing recorded",
    /// never as a crash that takes a scheduled task down with it.
    #[test]
    fn a_corrupt_sidecar_reads_as_no_state_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        std::fs::write(path_for(&db), b"not json").unwrap();
        assert_eq!(read(&db), None);
    }

    #[test]
    fn the_sidecar_is_a_fixed_name_beside_the_store_not_derived_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("some-oddly-named-store.db");
        assert_eq!(path_for(&db), dir.path().join("sync-ship-state.json"));
    }
}
