//! The Absent-check guard (`serve::absent_guard`), end to end through the
//! real compiled `serve hook` binary - mirrors `capture_guard_hook.rs`'s own
//! shape for exactly the same reason: the pure decision logic already has
//! its own unit tests in `serve::absent_guard`'s own test module; this file
//! proves the WIRING - the real store, the real marker file on disk, and the
//! real hook JSON shape on stdout, for a real Write/Edit PreToolUse payload.
//!
//! The forbidden literal used throughout is the em dash character, written
//! only ever as the Rust unicode escape `\u{2014}`, never as a literal
//! character in this source file, per this workspace's own typography rule.

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

/// `out` must not be a block decision. NOT the same as "must be empty": the
/// touched file may still carry the SAME item as an ordinary `Target::Path`
/// binding, which the pre-existing informational surface (surface 2) renders
/// as context on every touch regardless of this guard - that is the exact
/// "whisper" this guard exists ALONGSIDE, not something it silences. Only a
/// real `{"decision":"block",...}` counts as this guard (or anything else)
/// actually blocking the call.
fn assert_not_blocked(out: &str) {
    if out.trim().is_empty() {
        return;
    }
    let v: serde_json::Value =
        serde_json::from_str(out).unwrap_or_else(|e| panic!("expected valid JSON or empty output, got a parse error {e}: {out}"));
    assert_ne!(v.get("decision"), Some(&serde_json::Value::String("block".to_string())), "must not be a block decision: {out}");
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

/// A real store with one live Rule anchored to `NOTES.md` (relative to
/// `dir`), carrying a still-current `Check::Absent` forbidding the em dash
/// character. `NOTES.md` is created on disk right here so the anchor's
/// currency actually holds.
fn fixture_with_em_dash_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("NOTES.md"), "# notes\n").unwrap();
    let item = model::item::Item {
        id: "no-em-dash-in-notes".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md never carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in NOTES.md and nobody notices".to_string()),
        check: Some(model::item::Check::Absent { path: "NOTES.md".to_string(), literal: "\u{2014}".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a rule anchored to a markdown file
/// forbidding the em dash character, and a real `Write` tool call whose
/// proposed content carries one, is blocked - through the real compiled
/// binary, not a unit-level call into the guard's own pure functions.
#[test]
fn a_write_introducing_an_em_dash_into_the_anchored_file_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "a line with an em dash \u{2014} right here"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("no-em-dash-in-notes"), "must name the rule id: {reason}");
    assert!(reason.contains("NOTES.md never carries an em dash character"), "must carry the rule's own text: {reason}");
    assert!(reason.contains('\u{2014}'), "the quoted fragment must carry the offending character itself: {reason}");
}

/// At most one block per session per file: the SAME write, sent again in the
/// SAME session, must not block a second time.
#[test]
fn a_second_write_to_the_same_file_in_the_same_session_is_not_blocked_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let first = run_hook(&db, &write_payload("s1", dir.path(), &notes, "one em dash \u{2014} here"));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");

    let second = run_hook(&db, &write_payload("s1", dir.path(), &notes, "another em dash \u{2014} here too"));
    assert_not_blocked(&second);
}

/// The once-per-session marker keys on (session, file), never on session
/// alone: a DIFFERENT file in the same session, carrying no rule of its own,
/// must never be blocked - proves the marker is not suppressing the whole
/// session's writes wholesale.
#[test]
fn a_different_file_in_the_same_session_is_not_touched_by_the_first_files_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");
    let other = dir.path().join("OTHER.md");

    let first = run_hook(&db, &write_payload("s1", dir.path(), &notes, "one em dash \u{2014} here"));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");

    let second = run_hook(&db, &write_payload("s1", dir.path(), &other, "an em dash \u{2014} in an unrelated file"));
    assert!(second.trim().is_empty(), "an unrelated, unanchored file must never be blocked by this guard: {second}");
}

/// The Edit tool's own payload shape: the replacement half (`new_string`),
/// never the removed half (`old_string`) - proves the same guard reads the
/// correct field for a different tool, through the real binary.
#[test]
fn an_edit_introducing_an_em_dash_via_new_string_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let out = run_hook(&db, &edit_payload("s1", dir.path(), &notes, "old text", "new text with an em dash \u{2014} in it"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains('\u{2014}'));
}

/// Sanity check that the fixture is meaningful: a write with no forbidden
/// character at all must never be blocked - without this, a bug that
/// blocked EVERY write to NOTES.md regardless of content would pass every
/// test above undetected.
#[test]
fn a_write_with_no_forbidden_character_is_never_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "an entirely ordinary line, no such character"));
    assert_not_blocked(&out);
}

// ------------------------------------------------------ staleness sidecar
//
// The staleness half of the doctrine (`serve::absent_guard`'s own module doc
// comment): a rule that matched the file being written and carried a check
// that could not prove its own currency is recorded to a sidecar beside the
// store, never affecting the block decision. The pure scan/fold logic
// already has its own unit tests in `serve::absent_guard`'s own test module;
// these prove the WIRING - the real store, the real sidecar file on disk,
// and a real Write PreToolUse payload, through the real compiled binary.

/// The SAME fixture as `fixture_with_em_dash_rule` above, except `NOTES.md`
/// is deliberately never created - so the rule's own currency can never be
/// proven.
fn fixture_with_em_dash_rule_but_no_anchored_file(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = model::item::Item {
        id: "no-em-dash-in-notes".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md never carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in NOTES.md and nobody notices".to_string()),
        check: Some(model::item::Check::Absent { path: "NOTES.md".to_string(), literal: "\u{2014}".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a rule whose anchored file was deleted
/// (or never restored) never blocks a write that would otherwise have
/// tripped it - the unit-level guarantee
/// `a_rule_whose_anchored_path_no_longer_exists_never_blocks` already proves
/// in `serve::absent_guard`'s own test module - but here it ALSO leaves a
/// record in the staleness sidecar beside the store, through the real
/// compiled binary: the exact wiring a pure unit test cannot prove on its
/// own.
#[test]
fn a_write_that_could_not_prove_its_rules_currency_is_allowed_and_recorded_in_the_stale_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule_but_no_anchored_file(dir.path());
    let notes = dir.path().join("NOTES.md");
    let stale_path = db.parent().unwrap().join("absent-guard-stale.json");
    assert!(!stale_path.exists(), "fixture sanity: nothing recorded before the hook ever runs");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "a line with an em dash \u{2014} right here"));
    assert_not_blocked(&out);

    let stale_text = std::fs::read_to_string(&stale_path)
        .unwrap_or_else(|e| panic!("the staleness sidecar must exist and be readable after the hook ran: {e}"));
    let stale: serde_json::Value = serde_json::from_str(&stale_text).unwrap();
    let record = &stale["no-em-dash-in-notes"];
    assert_eq!(record["outcome"].as_str(), Some("failed"), "{stale_text}");
    // "the file that was being written" is recorded exactly as the hook
    // payload named it - Claude Code's own `file_path` is absolute, and
    // nothing here normalizes it, the same stance the LOCATION guard's own
    // block message already takes for this same field (see
    // `location_block_message`).
    assert_eq!(record["file"].as_str(), Some(notes.to_string_lossy().as_ref()), "{stale_text}");
    assert_eq!(record["count"].as_u64(), Some(1), "{stale_text}");
    assert!(record["check"].as_str().unwrap().contains("Absent"), "{stale_text}");
}

/// The marker file the BLOCK path already uses (`absent-guard-blocked.json`)
/// must stay untouched by this feature when nothing ever blocks - proven by
/// confirming it is never even created by a call that only ever records
/// staleness.
#[test]
fn recording_staleness_never_creates_the_existing_block_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule_but_no_anchored_file(dir.path());
    let notes = dir.path().join("NOTES.md");
    let blocked_marker = db.parent().unwrap().join("absent-guard-blocked.json");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "a line with an em dash \u{2014} right here"));
    assert_not_blocked(&out);

    assert!(!blocked_marker.exists(), "a write that never blocked must never create the block marker either");
}

/// A second write in the same session, still against the same never-created
/// anchor, increments the sidecar's own count rather than duplicating the
/// row - through the real binary, mirroring the pure-function-level
/// `a_second_stale_occurrence_increments_the_count_instead_of_duplicating`
/// test in `serve::absent_guard`'s own test module.
#[test]
fn a_second_write_in_the_same_session_increments_the_stale_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule_but_no_anchored_file(dir.path());
    let notes = dir.path().join("NOTES.md");
    let stale_path = db.parent().unwrap().join("absent-guard-stale.json");

    run_hook(&db, &write_payload("s1", dir.path(), &notes, "one em dash \u{2014} here"));
    run_hook(&db, &write_payload("s1", dir.path(), &notes, "another em dash \u{2014} here too"));

    let stale_text = std::fs::read_to_string(&stale_path).unwrap();
    let stale: serde_json::Value = serde_json::from_str(&stale_text).unwrap();
    assert_eq!(stale["no-em-dash-in-notes"]["count"].as_u64(), Some(2), "{stale_text}");
}
