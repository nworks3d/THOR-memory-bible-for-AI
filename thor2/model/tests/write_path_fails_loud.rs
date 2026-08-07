//! R5's other half, tested against the REAL compiled binary: a write must
//! never fail silently. Mirrors `serve/tests/hook_fails_open.rs`, which
//! proves the read/serve boundary fails OPEN on the same kind of broken
//! store; this proves the write boundary (`model declare`) fails LOUD on it -
//! non-zero exit, a non-empty stderr message, and nothing appended.

use std::process::Command;

#[test]
fn declare_against_a_corrupt_store_fails_loudly_not_silently() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("corrupt.db");
    std::fs::write(&db_path, b"this is not a sqlite database, just garbage bytes").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_model"))
        .arg("--db")
        .arg(&db_path)
        .arg("declare")
        .args(["--id", "x1", "--kind", "rule", "--text", "never force-push to main"])
        .arg("--always")
        .output()
        .expect("failed to run the model binary");

    assert_ne!(out.status.code(), Some(0), "a write against a broken store must not exit 0");
    assert!(!out.stderr.is_empty(), "a broken write must say something on stderr, never stay silent");
    assert!(out.stdout.is_empty(), "nothing was written, so there is nothing to confirm on stdout");
}

#[test]
fn declare_of_a_refused_item_fails_loudly_and_writes_nothing() {
    // The gate-refusal half of "write and declare never silent": run through
    // the real binary (not just the library's own gate tests) so the CLI's
    // own exit-code/stderr contract is what gets proven, not just the
    // function it calls.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("fine.db");

    let out = Command::new(env!("CARGO_BIN_EXE_model"))
        .arg("--db")
        .arg(&db_path)
        .arg("declare")
        .args(["--id", "x2", "--kind", "rule", "--text", "no binding at all"])
        // deliberately no --moment / --target / --always: ground 1
        .output()
        .expect("failed to run the model binary");

    assert_eq!(out.status.code(), Some(1), "a refused declare must exit 1, not 0");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("refused"), "stderr must say it was refused, got: {stderr}");

    let show = Command::new(env!("CARGO_BIN_EXE_model"))
        .arg("--db")
        .arg(&db_path)
        .arg("show")
        .args(["--id", "x2"])
        .output()
        .expect("failed to run the model binary");
    assert_ne!(show.status.code(), Some(0), "nothing may have been written for a refused item");
}
