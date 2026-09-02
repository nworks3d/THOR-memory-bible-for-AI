//! The shell write hole (`serve::input::shell_write_targets`), wired into
//! `command_guard_block` (`serve/src/bin/serve.rs`), end to end through the
//! real compiled `serve hook` binary - mirrors `command_absent_guard.rs`'s
//! own shape: the pure parsing logic already has its own unit tests in
//! `serve::input`'s own test module; this file proves the WIRING - a real
//! store, a real Bash PreToolUse payload, and the real hook JSON shape on
//! stdout.
//!
//! MEASURED: a rule anchored at a file with a `contains` or a location check
//! refuses an Edit/Write of that file, but `rm file`, `truncate -s 0 file`
//! and `echo x > file` from Bash walked straight past it - a shell write is
//! a write.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn run_hook(db: &Path, payload: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

fn bash_payload(session_id: &str, cwd: &Path, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "run a command" },
    })
    .to_string()
}

/// `out` must not be a block decision - see `absent_check_guard.rs`'s own
/// identically-named helper for why this is not the same as "must be empty".
fn assert_not_blocked(out: &str) {
    if out.trim().is_empty() {
        return;
    }
    let v: serde_json::Value = serde_json::from_str(out)
        .unwrap_or_else(|e| panic!("expected valid JSON or empty output, got a parse error {e}: {out}"));
    assert_ne!(
        v.get("decision"),
        Some(&serde_json::Value::String("block".to_string())),
        "must not be a block decision: {out}"
    );
}

// ------------------------------------------------------------ contains rule
//
// A rule anchored to a real file with a `Check::Contains` - the shape that
// already refuses an Edit/Write which drops the required literal
// (`absent_guard::find_missing_required`). `rm`/`truncate -s 0` are the two
// shell writes whose resulting content is KNOWN to be empty (`Removed`/
// `Emptied`), so this is the check they get fed through, with "" standing in
// for what the file would hold afterward.

fn fixture_with_contains_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("NOTES.md"), "the agreed rule: never force-push main\n").unwrap();
    let item = model::item::Item {
        id: "notes-keeps-the-rule".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md always keeps the force-push rule".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the rule goes missing from NOTES.md and nobody notices".to_string()),
        check: Some(model::item::Check::Contains {
            path: "NOTES.md".to_string(),
            literal: "never force-push main".to_string(),
        }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: `rm` on a file whose required literal
/// would vanish with it is refused - through the real compiled binary, not
/// a unit-level call into `find_missing_required` itself.
#[test]
fn rm_on_a_file_with_a_contains_rule_is_refused_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_contains_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "rm NOTES.md"));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("notes-keeps-the-rule"), "must name the rule id: {reason}");
    assert!(reason.contains("never force-push main"), "must name the literal that would be lost: {reason}");
    assert!(reason.contains("rm NOTES.md"), "must name the command: {reason}");
    assert!(reason.contains("removed"), "must say what the command would have done: {reason}");
    assert!(reason.contains("Nothing was done"), "{reason}");
}

/// The second shell write whose result is knowably empty: `truncate -s 0`
/// refuses the same way `rm` does, and says EMPTIED rather than REMOVED.
#[test]
fn truncate_s_zero_on_a_file_with_a_contains_rule_is_refused_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_contains_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "truncate -s 0 NOTES.md"));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("notes-keeps-the-rule"), "{reason}");
    assert!(reason.contains("emptied"), "must say emptied, not removed: {reason}");
}

/// A plain read is not a write at all: `shell_write_targets` finds nothing
/// for `cat`, so the guarded file is never even examined.
#[test]
fn a_plain_read_is_never_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_contains_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "cat NOTES.md"));
    assert_not_blocked(&out);
}

// ------------------------------------------------------------- location rule
//
// A rule protecting a whole path as a LOCATION (`severity: Irreversible`
// plus a `Check::PathExists` for the SAME path a binding names - see
// `serve::absent_guard::location_anchor`'s own doc comment for the
// three-condition rule). Unlike the `Contains` fixture above, this arm fires
// on EVERY shell write effect, `Rewritten` included, which is what makes
// `echo x > <file>` - a redirect WITH a producer - refusable at all: it is
// never fed to `find_missing_required`, but it is still a write to a
// protected PLACE, and the location guard does not care what the write
// leaves behind, only where it lands.

fn fixture_with_location_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("NOTES.md"), "# notes\n").unwrap();
    let item = model::item::Item {
        id: "notes-is-frozen".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md is frozen, never touch it from a shell".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("NOTES.md turns out to be safe to touch after all".to_string()),
        check: Some(model::item::Check::PathExists { path: "NOTES.md".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a redirect into a location-protected file
/// is refused, proving `echo x > <file>` reaches the SAME location guard a
/// real Write/Edit already goes through.
#[test]
fn a_redirect_into_a_location_protected_file_is_refused_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_location_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "echo x > NOTES.md"));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("notes-is-frozen"), "{reason}");
    assert!(reason.contains("echo x > NOTES.md"), "must name the command: {reason}");
}

/// THE DEFECT THIS PREVENTS: a redirect is refused ONLY when a rule at that
/// exact place says so - never a blanket ban on `>` itself. A redirect into
/// a DIFFERENT, unprotected file must sail through.
#[test]
fn a_redirect_into_an_unprotected_file_is_not_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_location_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "echo x > OTHER.md"));
    assert_not_blocked(&out);
}
