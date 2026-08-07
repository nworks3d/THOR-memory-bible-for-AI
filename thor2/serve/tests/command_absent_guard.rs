//! The command guard (`serve::absent_guard`'s THIRD anchor shape, a live
//! Rule/Orientation bound to a `Command` target), end to end through the
//! real compiled `serve hook` binary - mirrors `absent_check_guard.rs`'s own
//! shape for exactly the same reason: the pure decision logic already has
//! its own unit tests in `serve::absent_guard`'s own test module; this file
//! proves the WIRING - the real store, the real marker file on disk (this
//! guard's OWN sidecar, `absent-guard-command-blocked.json`, never the
//! file-based guard's `absent-guard-blocked.json`), and the real hook JSON
//! shape on stdout, for a real Bash PreToolUse payload.
//!
//! The forbidden literal used throughout is the em dash character, written
//! only ever as the Rust unicode escape `\u{2014}`, never as a literal
//! character in this source file, per this workspace's own typography rule -
//! the SAME convention `absent_check_guard.rs` already follows.

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

/// `out` must not be a block decision. NOT the same as "must be empty" - see
/// `absent_check_guard.rs`'s own identically-named helper for why (the
/// touched command may still render as ordinary, non-blocking context).
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

/// A real store with one live Rule anchored to the `Command` target "git
/// commit", carrying a still-current `Check::Absent` forbidding the em dash
/// character - its own check path anchored to a real, on-disk POLICY file,
/// never the command anchor itself, proving currency is independent of the
/// `Command` binding's own value (see `serve::absent_guard`'s own
/// top-of-file doc comment, "THE COMMAND guard"). `COMMIT-POLICY.md` is
/// created on disk right here so the anchor's currency actually holds.
fn fixture_with_em_dash_commit_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("COMMIT-POLICY.md"), "# commit policy\n").unwrap();
    let item = model::item::Item {
        id: "no-em-dash-in-commits".to_string(),
        kind: model::item::Kind::Rule,
        text: "a commit message never carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Command,
            value: "git commit".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in a commit message and nobody notices".to_string()),
        check: Some(model::item::Check::Absent {
            path: "COMMIT-POLICY.md".to_string(),
            literal: "\u{2014}".to_string(),
        }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a rule anchored to the `git commit`
/// subcommand forbidding the em dash character, and a real `Bash` tool call
/// whose command carries one inside the commit message, is blocked - through
/// the real compiled binary, not a unit-level call into the guard's own pure
/// functions.
#[test]
fn a_commit_command_carrying_an_em_dash_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_commit_rule(dir.path());
    let command = "git commit -m \"a line with an em dash \u{2014} right here\"";

    let out = run_hook(&db, &bash_payload("s1", dir.path(), command));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("no-em-dash-in-commits"), "must name the rule id: {reason}");
    assert!(
        reason.contains("a commit message never carries an em dash character"),
        "must carry the rule's own text: {reason}"
    );
    assert!(reason.contains('\u{2014}'), "the quoted fragment must carry the offending character itself: {reason}");
}

/// A DIFFERENT git subcommand, carrying the same forbidden character, is
/// never blocked - the command guard's own realistic proof that MATCHING
/// (never merely the literal's presence) gates the block. Mirrors
/// `a_forbidden_literal_written_to_a_different_file_is_not_blocked`'s own
/// role in `absent_guard`'s unit tests, proven here through the real binary.
#[test]
fn a_non_matching_command_carrying_the_same_literal_is_never_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_commit_rule(dir.path());
    let command = "git push origin main -- \u{2014}";

    let out = run_hook(&db, &bash_payload("s1", dir.path(), command));
    assert_not_blocked(&out);
}

/// At most one block per session: the SAME command shape, sent again in the
/// SAME session with a different (still-violating) message, must not block a
/// second time - mirrors
/// `a_second_write_to_the_same_file_in_the_same_session_is_not_blocked_again`,
/// proving the command guard's own (session, anchor)-keyed marker (see
/// `serve::absent_guard`'s own top-of-file doc comment for why the key is
/// the matched ANCHOR - "git commit" - never the raw, ever-changing command
/// text).
#[test]
fn a_second_run_of_the_same_command_in_the_same_session_is_not_blocked_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_commit_rule(dir.path());

    let first = run_hook(&db, &bash_payload("s1", dir.path(), "git commit -m \"one em dash \u{2014} here\""));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");

    let second =
        run_hook(&db, &bash_payload("s1", dir.path(), "git commit -m \"another em dash \u{2014} here too\""));
    assert_not_blocked(&second);
}

/// A DIFFERENT session gets its own say: the once-per-session marker must
/// never suppress a genuinely new session's own first violation.
#[test]
fn a_different_session_is_not_suppressed_by_the_first_sessions_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_commit_rule(dir.path());

    let first = run_hook(&db, &bash_payload("s1", dir.path(), "git commit -m \"one em dash \u{2014} here\""));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");

    let second =
        run_hook(&db, &bash_payload("s2", dir.path(), "git commit -m \"another em dash \u{2014} here too\""));
    let v2: serde_json::Value =
        serde_json::from_str(&second).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {second}"));
    assert_eq!(v2["decision"], "block", "{second}");
}

/// A tool call that is not a command at all (a real `Write`) is unaffected by
/// this guard - even one whose file CONTENT happens to carry the literal
/// text "git commit", proving this guard never mistakes a mention for an
/// invocation: it never looks at file content at all, only `tool_input`'s
/// own "command" field.
#[test]
fn a_write_tool_call_is_unaffected_by_the_command_guard_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_commit_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let out = run_hook(
        &db,
        &write_payload(
            "s1",
            dir.path(),
            &notes,
            "a reminder to run git commit \u{2014} later, once things are ready",
        ),
    );
    assert_not_blocked(&out);
}

/// The marker file this guard writes is its OWN sidecar
/// (`absent-guard-command-blocked.json`), never the content/location guard's
/// shared `absent-guard-blocked.json` - proven by checking the right file
/// gets created and the wrong one does not.
#[test]
fn the_command_guard_writes_its_own_marker_file_never_the_shared_one() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_commit_rule(dir.path());
    let command_marker = db.parent().unwrap().join("absent-guard-command-blocked.json");
    let shared_marker = db.parent().unwrap().join("absent-guard-blocked.json");

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "git commit -m \"one em dash \u{2014} here\""));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "block", "{out}");

    assert!(command_marker.exists(), "the command guard must write its own marker file");
    assert!(!shared_marker.exists(), "a command block must never touch the file-based guard's own marker");
}

// ------------------------------------------------------ staleness sidecar
//
// The staleness half of the doctrine (`serve::absent_guard`'s own module doc
// comment): a rule that matched the command being run and carried a check
// that could not prove its own currency is recorded to a sidecar beside the
// store, never affecting the block decision. The pure scan/fold logic
// already has its own unit tests in `serve::absent_guard`'s own test module;
// these prove the WIRING - the real store, the real sidecar file on disk,
// and a real Bash PreToolUse payload, through the real compiled binary.

/// The SAME fixture as `fixture_with_em_dash_commit_rule` above, except
/// `COMMIT-POLICY.md` is deliberately never created - so the rule's own
/// currency can never be proven.
fn fixture_with_em_dash_commit_rule_but_no_anchored_file(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = model::item::Item {
        id: "no-em-dash-in-commits".to_string(),
        kind: model::item::Kind::Rule,
        text: "a commit message never carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Command,
            value: "git commit".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in a commit message and nobody notices".to_string()),
        check: Some(model::item::Check::Absent {
            path: "COMMIT-POLICY.md".to_string(),
            literal: "\u{2014}".to_string(),
        }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a rule whose anchored file was deleted
/// (or never restored) never blocks a command that would otherwise have
/// tripped it - the unit-level guarantee
/// `a_command_rule_whose_check_cannot_run_never_blocks_and_is_recorded_as_stale`
/// already proves in `serve::absent_guard`'s own test module - but here it
/// ALSO leaves a record in the staleness sidecar beside the store, through
/// the real compiled binary: the exact wiring a pure unit test cannot prove
/// on its own.
#[test]
fn a_command_that_could_not_prove_its_rules_currency_is_allowed_and_recorded_in_the_stale_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_commit_rule_but_no_anchored_file(dir.path());
    let stale_path = db.parent().unwrap().join("absent-guard-stale.json");
    assert!(!stale_path.exists(), "fixture sanity: nothing recorded before the hook ever runs");

    let command = "git commit -m \"a line with an em dash \u{2014} right here\"";
    let out = run_hook(&db, &bash_payload("s1", dir.path(), command));
    assert_not_blocked(&out);

    let stale_text = std::fs::read_to_string(&stale_path)
        .unwrap_or_else(|e| panic!("the staleness sidecar must exist and be readable after the hook ran: {e}"));
    let stale: serde_json::Value = serde_json::from_str(&stale_text).unwrap();
    let record = &stale["no-em-dash-in-commits"];
    assert_eq!(record["outcome"].as_str(), Some("failed"), "{stale_text}");
    // "the command that was run" is recorded exactly as the hook payload
    // named it - the same stance the file-based guard's own staleness sidecar
    // already takes for its own "file" field (see `absent_check_guard.rs`'s
    // own matching test).
    assert_eq!(record["file"].as_str(), Some(command), "{stale_text}");
    assert_eq!(record["count"].as_u64(), Some(1), "{stale_text}");
    assert!(record["check"].as_str().unwrap().contains("Absent"), "{stale_text}");
}

/// A second command in the same session, still against the same
/// never-created anchor, increments the sidecar's own count rather than
/// duplicating the row - through the real binary, mirroring the
/// pure-function-level `a_second_stale_occurrence_increments_the_count_
/// instead_of_duplicating` test in `serve::absent_guard`'s own test module.
#[test]
fn a_second_command_in_the_same_session_increments_the_stale_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_commit_rule_but_no_anchored_file(dir.path());
    let stale_path = db.parent().unwrap().join("absent-guard-stale.json");

    run_hook(&db, &bash_payload("s1", dir.path(), "git commit -m \"one em dash \u{2014} here\""));
    run_hook(&db, &bash_payload("s1", dir.path(), "git commit -m \"another em dash \u{2014} here too\""));

    let stale_text = std::fs::read_to_string(&stale_path).unwrap();
    let stale: serde_json::Value = serde_json::from_str(&stale_text).unwrap();
    assert_eq!(stale["no-em-dash-in-commits"]["count"].as_u64(), Some(2), "{stale_text}");
}
