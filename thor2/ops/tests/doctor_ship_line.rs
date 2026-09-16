//! CLI-level proof that `doctor` surfaces ship staleness the way
//! `ops::health::ship_line` computes it. `ops::health`'s own unit tests
//! prove the computation (none/fresh/stale); this proves the real compiled
//! `doctor` binary's `main` actually prints it, unedited, in the same
//! report a person actually reads (see `doctor_version.rs` for the sibling
//! proof that the same binary's version line survives the trip from
//! function to stdout).

use std::path::Path;
use std::process::Command;
use thor_core::event_store::{EventKind, EventStore};

fn run_doctor(db: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_doctor")).arg("--db").arg(db).output().unwrap();
    assert!(
        out.status.success(),
        "doctor without --gate always exits 0: status {:?}, stderr {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// THE "SAYS NOTHING" CASE, spelled out in the task this closes: a machine
/// that has never shipped must not get a "ship: not configured" line either
/// - that would be an alarm invented for a machine that was never meant to
/// ship at all (the NAS receiver itself, a laptop, a cloud sandbox).
#[test]
fn doctor_says_nothing_about_ship_when_none_was_ever_configured() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();

    let stdout = run_doctor(&db);
    assert!(
        !stdout.lines().any(|l| l.starts_with("ship:")),
        "a store that never shipped must print no ship line at all:\n{stdout}"
    );
}

#[test]
fn doctor_names_a_fresh_ship_briefly() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    ops::ship_state::record_success_at(&db, 5, now_unix()).unwrap();

    let stdout = run_doctor(&db);
    let line = stdout.lines().find(|l| l.starts_with("ship:"));
    assert!(line.is_some(), "a recorded ship must produce a line:\n{stdout}");
    let line = line.unwrap();
    assert!(line.contains("fresh"), "{line}");
    assert!(!line.contains("STALE"), "{line}");
}

/// THE EXACT INCIDENT THIS CLOSES, echoed almost to the day: a ship that
/// last succeeded four weeks ago, with real changes waiting behind it, must
/// be named - how many are waiting, the age, and that the hourly ship has
/// not run since. Never worded as a failure (no "STALE", no "failing
/// silently"): the machine may simply be asleep, and this line's whole job
/// is to let the owner tell that apart from a broken task.
#[test]
fn doctor_names_waiting_changes_past_the_ceiling_without_calling_it_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    store.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "body").unwrap();
    store.append_event("s", "l", "act", EventKind::FactCreated, "e2", None, "body").unwrap();
    drop(store);
    let four_weeks_ago = now_unix() - 28 * 24 * 3600;
    // Covered only seq 0 - both events above are still waiting.
    ops::ship_state::record_success_at(&db, 0, four_weeks_ago).unwrap();

    let stdout = run_doctor(&db);
    let line = stdout.lines().find(|l| l.starts_with("ship:"));
    assert!(line.is_some(), "a recorded (if stale) ship must still produce a line:\n{stdout}");
    let line = line.unwrap();
    assert!(line.contains("2 change"), "the count must be named: {line}");
    assert!(line.contains("not yet on the replica"), "{line}");
    assert!(line.contains("has not run since"), "{line}");
    assert!(!line.contains("STALE"), "waiting changes are not worded as a failure: {line}");
    assert!(!line.contains("failing silently"), "waiting changes are not worded as a failure: {line}");
}

/// BACKWARD COMPATIBILITY, through the real binary: a sidecar written before
/// this split existed has no `last_attempt_unix` at all, and must still fall
/// back to exactly the STALE wording `doctor` has always printed.
#[test]
fn doctor_falls_back_to_stale_wording_for_an_old_two_field_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    let four_weeks_ago = now_unix() - 28 * 24 * 3600;
    std::fs::write(
        ops::ship_state::path_for(&db),
        format!(r#"{{"completed_unix":{four_weeks_ago},"receiver_seq":5}}"#),
    )
    .unwrap();

    let stdout = run_doctor(&db);
    let line = stdout.lines().find(|l| l.starts_with("ship:"));
    assert!(line.is_some(), "an old sidecar must still produce a line:\n{stdout}");
    let line = line.unwrap();
    assert!(line.contains("STALE"), "{line}");
    assert!(
        line.contains(&ops::health::SHIP_STALE_CEILING_HOURS.to_string()),
        "the ceiling must be named, never a bare number: {line}"
    );
}

/// THE ALARM CASE, through the real binary: the most recent attempt failed,
/// and `doctor` must say so - when, the reason, and that a success is on
/// record (not the plain fresh/STALE text a healthy or merely-quiet ship
/// would print).
#[test]
fn doctor_alarms_on_a_failed_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    let now = now_unix();
    ops::ship_state::record_success_at(&db, 5, now - 5 * 3600).unwrap();
    ops::ship_state::record_failure_at(&db, "receiver rejected the shared secret (401)", now).unwrap();

    let stdout = run_doctor(&db);
    let line = stdout.lines().find(|l| l.starts_with("ship:"));
    assert!(line.is_some(), "a failed attempt must still produce a line:\n{stdout}");
    let line = line.unwrap();
    assert!(line.contains("FAILED"), "{line}");
    assert!(line.contains("receiver rejected the shared secret (401)"), "{line}");
    assert!(line.contains("last succeeded 5h ago"), "{line}");
}
