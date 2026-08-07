//! The Location guard (`serve::absent_guard::find_location_violation`), end
//! to end through the real compiled `serve hook` binary - mirrors
//! `absent_check_guard.rs`'s own shape for exactly the same reason: the pure
//! decision logic already has its own unit tests in `serve::absent_guard`'s
//! own test module; this file proves the WIRING - the real store, the real
//! marker file on disk, the real ordering against the content guard, and the
//! real hook JSON shape on stdout, for a real Write/Edit PreToolUse payload.

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
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

fn write_payload(session_id: &str, cwd: &Path, file_path: &Path, content: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": { "file_path": file_path.to_string_lossy(), "content": content },
    })
    .to_string()
}

fn edit_payload(session_id: &str, cwd: &Path, file_path: &Path, old_string: &str, new_string: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Edit",
        "tool_input": {
            "file_path": file_path.to_string_lossy(),
            "old_string": old_string,
            "new_string": new_string,
        },
    })
    .to_string()
}

/// `out` must not be a block decision. NOT the same as "must be empty" - see
/// `absent_check_guard.rs`'s own twin of this helper for why: the touched
/// file may still carry the same item as an ordinary `Target` binding, which
/// the pre-existing informational surface (surface 2) renders as context
/// regardless of this guard. Only a real `{"decision":"block",...}` counts
/// as this guard (or anything else) actually blocking the call.
fn assert_not_blocked(out: &str) {
    if out.trim().is_empty() {
        return;
    }
    let v: serde_json::Value =
        serde_json::from_str(out).unwrap_or_else(|e| panic!("expected valid JSON or empty output, got a parse error {e}: {out}"));
    assert_ne!(v.get("decision"), Some(&serde_json::Value::String("block".to_string())), "must not be a block decision: {out}");
}

/// A real store with one live Rule anchored to the DIRECTORY `frozen`
/// (relative to `dir`), carrying a still-current `Check::PathExists` for
/// that same directory and `severity: Irreversible`. `frozen` is created on
/// disk right here so the anchor's currency actually holds.
fn fixture_with_frozen_directory_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::create_dir(dir.join("frozen")).unwrap();
    let item = model::item::Item {
        id: "never-touch-frozen".to_string(),
        kind: model::item::Kind::Rule,
        text: "frozen/ is a frozen archive and must never be written to".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Dir,
            value: "frozen".to_string(),
        }],
        severity: Some(model::item::Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a write to frozen/ lands with no harm done".to_string()),
        check: Some(model::item::Check::PathExists { path: "frozen".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a rule anchored to a protected directory,
/// and a real `Write` tool call whose target file lives inside it, is
/// blocked - through the real compiled binary, not a unit-level call into
/// the guard's own pure functions.
#[test]
fn a_write_inside_a_protected_directory_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_frozen_directory_rule(dir.path());
    let target = dir.path().join("frozen").join("cli.rs");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &target, "fn main() {}"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.starts_with("[THOR]"), "{reason}");
    assert!(reason.contains("never-touch-frozen"), "must name the rule id: {reason}");
    assert!(reason.contains("frozen/ is a frozen archive"), "must carry the rule's own text: {reason}");
    assert!(reason.contains("frozen"), "must name the protected path: {reason}");
    assert!(reason.contains("cli.rs"), "must name the refused file: {reason}");
}

/// The Edit tool's own payload shape blocks exactly the same way - the
/// location guard never even looks at content, so this proves it fires for
/// BOTH tools this guard covers, through the real binary.
#[test]
fn an_edit_inside_a_protected_directory_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_frozen_directory_rule(dir.path());
    let target = dir.path().join("frozen").join("cli.rs");
    std::fs::write(&target, "old").unwrap();

    let out = run_hook(&db, &edit_payload("s1", dir.path(), &target, "old", "new"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("never-touch-frozen"));
}

/// Sanity check that the fixture is meaningful: a write OUTSIDE the
/// protected directory must never be blocked - without this, a bug that
/// blocked EVERY write regardless of location would pass the tests above
/// undetected.
#[test]
fn a_write_outside_the_protected_directory_is_never_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_frozen_directory_rule(dir.path());
    let target = dir.path().join("elsewhere.rs");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &target, "fn main() {}"));
    assert_not_blocked(&out);
}

/// At most one block per session per file: the SAME write, sent again in the
/// SAME session, must not block a second time - proves the location guard
/// reuses the SAME marker mechanism the content guard already relies on
/// (mirrors `absent_check_guard.rs`'s own twin of this test), not a second
/// one.
#[test]
fn a_second_write_to_the_same_file_in_the_same_session_is_not_blocked_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_frozen_directory_rule(dir.path());
    let target = dir.path().join("frozen").join("cli.rs");

    let first = run_hook(&db, &write_payload("s1", dir.path(), &target, "fn main() {}"));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");

    let second = run_hook(&db, &write_payload("s1", dir.path(), &target, "fn other() {}"));
    assert_not_blocked(&second);
}

/// THE ORDERING THIS PINS (requirement 5): when a location rule and a
/// content rule would BOTH fire for the same write, exactly one block
/// happens, and it is the LOCATION one - the broader refusal. A reason
/// carrying the content rule's id instead (or alongside) would mean the
/// wrong one won, or both fired.
#[test]
fn when_both_a_location_and_a_content_rule_would_fire_the_location_block_wins() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_frozen_directory_rule(dir.path());
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let target = dir.path().join("frozen").join("NOTES.md");
    std::fs::write(&target, "# notes\n").unwrap();
    let content_item = model::item::Item {
        id: "no-em-dash-in-notes".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md never carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "frozen/NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in NOTES.md and nobody notices".to_string()),
        check: Some(model::item::Check::Absent {
            path: "frozen/NOTES.md".to_string(),
            literal: "\u{2014}".to_string(),
        }),
    };
    model::store::declare(&mut store, "s", "l", "a", &content_item).unwrap();

    let out = run_hook(&db, &write_payload("s1", dir.path(), &target, "a line with an em dash \u{2014} here"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("never-touch-frozen"), "the LOCATION rule must win: {reason}");
    assert!(!reason.contains("no-em-dash-in-notes"), "the content rule must not also fire: {reason}");
}

/// The pre-existing content guard still fires exactly as before when NO
/// location rule applies to the file at all - proves the new location check
/// added ahead of it in `absent_guard_block` never swallows or changes a
/// content-only block.
#[test]
fn the_content_guard_still_fires_on_its_own_when_no_location_rule_applies() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("NOTES.md"), "# notes\n").unwrap();
    let item = model::item::Item {
        id: "no-em-dash-in-notes".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md never carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Path, value: "NOTES.md".to_string() }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in NOTES.md and nobody notices".to_string()),
        check: Some(model::item::Check::Absent { path: "NOTES.md".to_string(), literal: "\u{2014}".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    let notes = dir.path().join("NOTES.md");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "a line with an em dash \u{2014} right here"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("no-em-dash-in-notes"));
}
