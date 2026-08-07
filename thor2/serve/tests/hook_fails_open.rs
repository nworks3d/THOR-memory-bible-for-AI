//! R5, tested against the REAL compiled binary, not just the library
//! function: a corrupt store must make `serve hook` print nothing to stdout
//! and exit 0. This is the boundary test CONTRACT.md's R5 asks for - "a
//! corrupt store makes the hook exit 0 and print nothing".

use std::io::Write;
use std::process::{Command, Stdio};

fn run_hook(db_path: &std::path::Path, stdin_payload: &str) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db_path)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start the serve binary");
    child.stdin.take().unwrap().write_all(stdin_payload.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("failed to wait on the serve binary");
    (out.status, out.stdout, out.stderr)
}

#[test]
fn a_corrupt_store_makes_the_hook_exit_0_and_print_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("corrupt.db");
    // Not a SQLite file at all: PRAGMA journal_mode = WAL (run inside
    // EventStore::new, right after open) fails on this with SQLITE_NOTADB,
    // so EventStore::new returns Err and hook_once's `?` turns that into "no
    // block" before anything is ever printed.
    std::fs::write(&db_path, b"this is not a sqlite database, just garbage bytes").unwrap();

    let payload = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "git push --force origin main" }
    })
    .to_string();

    let (status, stdout, stderr) = run_hook(&db_path, &payload);
    assert_eq!(status.code(), Some(0), "hook must exit 0 even against a corrupt store");
    assert!(stdout.is_empty(), "hook must print zero bytes to stdout on failure, got: {:?}", stdout);
    // stderr is not part of the R5 contract (only "no byte on stdout and exit
    // 0" is), but the panic hook installed in cmd_hook also suppresses it -
    // recorded here as an observed fact, not a separate requirement.
    assert!(stderr.is_empty(), "observed: stderr was also silent, got: {:?}", stderr);
}

#[test]
fn malformed_json_on_stdin_also_fails_open() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("fine.db");
    // A perfectly good, empty store - the failure here is only the payload.
    thor_core::event_store::EventStore::new(&db_path).unwrap();

    let (status, stdout, stderr) = run_hook(&db_path, "{ this is not valid json");
    assert_eq!(status.code(), Some(0));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn a_missing_parent_directory_also_fails_open() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("no").join("such").join("dir").join("store.db");
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "git push --force origin main" }
    })
    .to_string();
    let (status, stdout, _stderr) = run_hook(&db_path, &payload);
    assert_eq!(status.code(), Some(0));
    assert!(stdout.is_empty());
}

/// Sanity check that the three failures above are meaningful: against a
/// GOOD store with a real matching item, the same binary DOES print a block.
/// Without this, "prints nothing" could just mean "the binary is broken".
#[test]
fn the_same_binary_does_print_a_block_when_nothing_is_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("good.db");
    {
        let mut store = thor_core::event_store::EventStore::new(&db_path).unwrap();
        let item = model::item::Item {
            id: "never-force-push".to_string(),
            kind: model::item::Kind::Rule,
            text: "never force-push to main".to_string(),
            bindings: vec![model::item::Binding::Moment(intent::Action::Push)],
            severity: Some(model::item::Severity::Irreversible),
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("a force-push to main lands clean with nobody reverting it".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    }

    let payload = serde_json::json!({
        "session_id": "s1",
        "tool_name": "Bash",
        "tool_input": { "command": "git push --force origin main" }
    })
    .to_string();
    let (status, stdout, stderr) = run_hook(&db_path, &payload);
    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty());
    let out = String::from_utf8(stdout).unwrap();
    assert!(out.contains("never force-push to main"), "expected the block in stdout, got: {out}");
    assert!(out.contains("hookSpecificOutput"));
}
