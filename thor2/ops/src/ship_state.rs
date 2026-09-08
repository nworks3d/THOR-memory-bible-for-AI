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
/// immediately afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShipState {
    /// Unix seconds (`SystemTime::now()` at the moment the ship completed
    /// without error).
    pub completed_unix: u64,
    /// The receiver's `contiguous_seq` right after this ship
    /// (`transport::PushSummary::final_cursor`) - not a fact about the ship
    /// itself, but the one number that tells someone comparing the two
    /// stores by hand how far the receiver actually got.
    pub receiver_seq: i64,
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
pub fn record_success(db: &Path, receiver_seq: i64) -> std::io::Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    record_success_at(db, receiver_seq, now)
}

/// `record_success`, with the completion time given explicitly - the seam a
/// test (or `ops::health`'s own tests) uses to write a state that is already
/// hours old without actually waiting for them.
pub fn record_success_at(db: &Path, receiver_seq: i64, completed_unix: u64) -> std::io::Result<()> {
    let state = ShipState { completed_unix, receiver_seq };
    // `Vec<u8>` serialization of a two-field struct cannot fail; unwrap_or
    // only so a future field that COULD fail to serialize degrades to an
    // empty object (parsed back as `None` by `read`) rather than panicking a
    // scheduled task on its way out the door.
    let json = serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string());
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
        assert_eq!(read(&db), Some(ShipState { completed_unix: 1_000_000, receiver_seq: 42 }));
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
        assert_eq!(read(&db), Some(ShipState { completed_unix: 2_000_000, receiver_seq: 2 }));
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
