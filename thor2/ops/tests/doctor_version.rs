//! `doctor`'s version surface, proven against the REAL compiled binary: THE
//! GAP THIS CLOSES, verified 2026-09-08 by grepping every binary in the
//! workspace for `CARGO_PKG_VERSION` and finding it nowhere - `--version`/
//! `-V` did not exist, and a pasted report carried no build number at all,
//! so a new user's bug report could not say which THOR was running except
//! by build log or git tag.
//!
//! `--version` is checked with no `--db` at all: `--db` is otherwise
//! required, so a passing exit here also proves clap short-circuits the
//! version flag BEFORE the required-argument check, never opening a store.

use std::process::Command;
use thor_core::event_store::EventStore;

#[test]
fn version_flag_prints_name_and_version_and_exits_0_without_a_db() {
    let out = Command::new(env!("CARGO_BIN_EXE_doctor")).arg("--version").output().unwrap();
    assert!(out.status.success(), "status: {:?}, stderr: {}", out.status, String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim(), format!("doctor {}", env!("CARGO_PKG_VERSION")), "full stdout: {stdout:?}");
}

#[test]
fn short_version_flag_answers_identically() {
    let out = Command::new(env!("CARGO_BIN_EXE_doctor")).arg("-V").output().unwrap();
    assert!(out.status.success(), "status: {:?}, stderr: {}", out.status, String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim(), format!("doctor {}", env!("CARGO_PKG_VERSION")), "full stdout: {stdout:?}");
}

/// THE DEFECT THIS PREVENTS: fails if `main`'s `println!("{}",
/// ops::health::version_line())` call is ever reverted while the function
/// itself survives - a pure unit test of `version_line()` alone could not
/// catch that, only running the real binary can.
#[test]
fn an_ordinary_report_still_carries_the_version_as_its_first_line() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("store.db");
    EventStore::new(&db_path).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_doctor")).arg("--db").arg(&db_path).output().unwrap();
    assert!(out.status.success(), "status: {:?}, stderr: {}", out.status, String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let first_line = stdout.lines().next().unwrap_or_default();
    assert_eq!(first_line, format!("doctor {}", env!("CARGO_PKG_VERSION")), "full stdout: {stdout}");
}
