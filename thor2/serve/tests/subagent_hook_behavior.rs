//! Renamed 2026-08-05 from `subagent_session_start_suppression.rs`: that file
//! tested a `SessionStart` subagent GATE that was removed from
//! `serve/src/bin/serve.rs` as dead code. Claude Code's own hooks
//! documentation, verified after that gate was written, confirms
//! `SessionStart` fires ONLY in the main session - it never fires inside a
//! Task-tool subagent at all, so a subagent check on that arm could never
//! execute (see `INJECTION-FRAMING.md`'s own addendum, and
//! `payload_is_from_a_subagent`'s doc comment in `serve.rs`, which now gates
//! Lane C's capture guard instead - the three surfaces that actually DO fire
//! inside a subagent; see `serve/tests/capture_guard_subagent_gate.rs`).
//!
//! What remains here are the two tests that describe behavior that is still
//! real and unrelated to that removed gate:
//!
//! - the owner's own main session still gets its standing-rules block on
//!   `SessionStart` (this was always true and remains true with no gate on
//!   that arm at all);
//! - a subagent's own moment-of-action (`PreToolUse`) surface still fires
//!   normally - `render::FRAMING_LINE` (a real, live fix, untouched by this
//!   file's rename) is what makes that surface safe to read as background
//!   rather than an order, and this test keeps proving it.

use model::item::{Binding, Item, Kind, Severity};
use model::store;
use std::io::Write;
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

fn always_rule(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("standing rule {id}"),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Irreversible),
        project: None,
        // Gate ground 11: this file tests subagent hook behaviour, and a
        // generic "standing rule N" has no literal to catch.
        tags: vec![format!("{}a test fixture with nothing literal to catch", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some(format!("standing rule {id} turns out to be wrong")),
        check: None,
    }
}

fn moment_rule(id: &str, action: intent::Action) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("never {} without checking first", action.as_str()),
        bindings: vec![Binding::Moment(action)],
        severity: Some(Severity::Irreversible),
        project: None,
        // Same as always_rule above.
        tags: vec![format!("{}a test fixture with nothing literal to catch", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out to be safe without checking first")),
        check: None,
    }
}

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
fn the_owners_own_main_session_still_gets_the_standing_rules_block() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("store.db");
    {
        let mut db = EventStore::new(&db_path).unwrap();
        store::declare(&mut db, "s", "l", "a", &always_rule("g1")).unwrap();
    }

    let payload = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "SessionStart",
        // no "agent_id" at all - the ordinary shape, and (per the doc
        // comment above) the ONLY shape SessionStart ever actually arrives
        // in from Claude Code itself.
    })
    .to_string();

    let (status, stdout, stderr) = run_hook(&db_path, &payload);
    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty());
    let out = String::from_utf8(stdout).unwrap();
    assert!(out.contains("standing rule g1"), "the main session must still see it, got: {out}");
}

/// The narrow scope of the ORIGINAL fix this file used to test (step 2,
/// `render::FRAMING_LINE`), still real and still worth pinning: a subagent's
/// own PreToolUse (moment of action) is untouched. Suppressing safety-
/// relevant, command-specific facts for a subagent about to run something
/// irreversible would be a regression, not a fix - `FRAMING_LINE` already
/// makes this surface safe to read as background rather than an order.
#[test]
fn a_subagents_moment_of_action_still_fires_normally() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("store.db");
    {
        let mut db = EventStore::new(&db_path).unwrap();
        store::declare(&mut db, "s", "l", "a", &moment_rule("m1", intent::Action::Push)).unwrap();
    }

    let payload = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "PreToolUse",
        "agent_id": "a1dca2c0feb7f44fb",
        "tool_name": "Bash",
        "tool_input": { "command": "git push --force origin main" },
    })
    .to_string();

    let (status, stdout, stderr) = run_hook(&db_path, &payload);
    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty());
    let out = String::from_utf8(stdout).unwrap();
    assert!(out.contains("never push without checking first"), "expected the moment block, got: {out}");
    assert!(out.contains("Background facts about the owner's setup"), "expected the framing line, got: {out}");
}
