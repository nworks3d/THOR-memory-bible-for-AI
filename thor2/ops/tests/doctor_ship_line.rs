//! CLI-level proof that `doctor` surfaces ship staleness the way
//! `ops::health::ship_line` computes it. `ops::health`'s own unit tests
//! prove the computation (none/fresh/stale); this proves the real compiled
//! `doctor` binary's `main` actually prints it, unedited, in the same
//! report a person actually reads (see `doctor_version.rs` for the sibling
//! proof that the same binary's version line survives the trip from
//! function to stdout).

use std::path::Path;
use std::process::Command;
use thor_core::event_store::EventStore;

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
/// last succeeded four weeks ago must be named, with both the age and the
/// ceiling it was measured against - never a bare number.
#[test]
fn doctor_names_a_stale_ship_with_its_age_and_the_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    let four_weeks_ago = now_unix() - 28 * 24 * 3600;
    ops::ship_state::record_success_at(&db, 5, four_weeks_ago).unwrap();

    let stdout = run_doctor(&db);
    let line = stdout.lines().find(|l| l.starts_with("ship:"));
    assert!(line.is_some(), "a recorded (if stale) ship must still produce a line:\n{stdout}");
    let line = line.unwrap();
    assert!(line.contains("STALE"), "{line}");
    assert!(
        line.contains(&ops::health::SHIP_STALE_CEILING_HOURS.to_string()),
        "the ceiling must be named, never a bare number: {line}"
    );
}
