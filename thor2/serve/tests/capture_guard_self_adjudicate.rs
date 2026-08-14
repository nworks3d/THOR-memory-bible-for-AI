//! `CaptureMode::SelfAdjudicate` (OPTION4-IMPLEMENTATION.md), end to end
//! through the real compiled `serve hook` binary - the same pattern
//! `capture_guard_hook.rs`/`capture_guard_subagent_gate.rs` already use for
//! the judge path and the subagent gate. The pure decision logic
//! (`capture::decide_self_adjudicate`, `capture::resolve_capture_mode`,
//! `capture::SELF_ADJUDICATE_MESSAGE`) already has its own unit tests in
//! `serve/src/capture.rs`; this file proves the WIRING - mode selection from
//! real config files, the real marker file on disk, and the real hook JSON
//! shape on stdout.
//!
//! All prompt text here is generic and synthetic (a linter policy), never
//! copied from a real session, per this workspace's own stance on test
//! fixtures.

use std::io::Write;
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

/// A fresh store, NOTHING else configured: no `guard-capture-mode.json`, no
/// `guard-judge-config.json`, no `guard-capture-rulebook.json`. This is the
/// exact undeployed-default shape (OPTION4-IMPLEMENTATION.md): self mode by
/// inference, narrow trigger set by compiled-in default
/// (`capture::NARROW_TRIGGER_DEFAULT`).
fn bare_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    dir
}

fn prompt_payload(session_id: &str, cwd: &Path, text: &str) -> String {
    serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "prompt": text,
    })
    .to_string()
}

fn stop_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string()
}

fn pretool_write_payload(session_id: &str, cwd: &Path, file_path: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": { "file_path": file_path },
    })
    .to_string()
}

fn subagent_prompt_payload(session_id: &str, cwd: &Path, text: &str) -> String {
    serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "prompt": text,
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
    })
    .to_string()
}

fn subagent_stop_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
    })
    .to_string()
}

/// THE DEFECT THIS PREVENTS (two defects, proven together since they share
/// one scenario): (1) self mode is not actually the default when nothing at
/// all is configured - the whole point of this task; (2) even once selected,
/// self mode never actually blocks an owed, unpaid capture. No mode config,
/// no judge config, no rulebook file: the compiled-in narrow trigger default
/// must still fire on this prompt, and self-adjudicate mode must still block
/// once, quoting the exact matched phrase.
#[test]
fn the_default_mode_blocks_once_on_an_owed_capture_with_nothing_configured_at_all() {
    let dir = bare_fixture();
    let db = dir.path().join("thor.db");

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    let stop_out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&stop_out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "self mode must be the default and must block once when owed: {stop_out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("from now on always run the linter first"), "{reason}");
    assert!(reason.contains("NEW durable decision"), "must be the self-adjudicate message, not the judge one: {reason}");
}

/// THE DEFECT THIS PREVENTS: the same flag keeps blocking turn end every
/// time Stop happens to run again, instead of spending its one block and
/// then staying quiet.
#[test]
fn self_adjudicate_mode_never_blocks_twice_for_the_same_flag() {
    let dir = bare_fixture();
    let db = dir.path().join("thor.db");

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    let first = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&first).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{first}");

    let second = run_hook(&db, &stop_payload("s1"));
    assert!(second.trim().is_empty(), "a flag that already blocked once must never block again: {second}");
}

/// THE DEFECT THIS PREVENTS: a decision the owner stated IS recorded (a real
/// remember/revise landed before Stop), but self-adjudicate mode still
/// blocks and asks the assistant to re-litigate something it already did.
#[test]
fn a_paid_debt_never_blocks_even_in_self_adjudicate_mode() {
    let dir = bare_fixture();
    let db = dir.path().join("thor.db");

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));

    {
        let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
        let item = model::item::Item {
            id: "lint-policy".to_string(),
            kind: model::item::Kind::Rule,
            text: "always run the linter first".to_string(),
            bindings: vec![model::item::Binding::Always],
            severity: Some(model::item::Severity::HouseStyle),
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("running the linter first turns out to be unnecessary".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "s1", "s1", "assistant", &item).unwrap();
    }

    let stop_out = run_hook(&db, &stop_payload("s1"));
    assert!(stop_out.trim().is_empty(), "a paid debt must not block turn end in self-adjudicate mode either: {stop_out}");
}

/// THE DEFECT THIS PREVENTS: self-adjudicate mode is supposed to cost
/// "nothing beyond one extra reply" - no external process, no model call.
/// Proven with a sentinel: even with mode EXPLICITLY forced to `self` AND a
/// real, working judge command configured (one that would write a sentinel
/// file the instant it is invoked, exactly like `judge_transport.rs`'s own
/// proof), self mode must never touch it.
#[test]
fn self_adjudicate_mode_never_spawns_a_process() {
    let dir = bare_fixture();
    let db = dir.path().join("thor.db");
    std::fs::write(dir.path().join("guard-capture-mode.json"), r#"{"mode":"self"}"#).unwrap();
    let sentinel = dir.path().join("judge-was-invoked.marker");
    let config = serde_json::json!({
        "command": env!("CARGO_BIN_EXE_fake_judge"),
        "args": ["sentinel", sentinel.to_str().unwrap(), "block", "from now on always run the linter first"],
        "timeout_ms": 5000,
    });
    std::fs::write(dir.path().join("guard-judge-config.json"), config.to_string()).unwrap();

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    let stop_out = run_hook(&db, &stop_payload("s1"));

    assert!(!sentinel.exists(), "self-adjudicate mode must never spawn the configured judge command");
    let v: serde_json::Value = serde_json::from_str(&stop_out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "self mode must still block on its own rulebook signal: {stop_out}");
}

/// THE DEFECT THIS PREVENTS: a real, usable judge transport silently gets
/// ignored in favor of the new self-adjudicate default. With NO explicit mode
/// config at all, a configured judge (proven invoked via the same sentinel
/// technique) must still win - every deployment that only ever wrote
/// `guard-judge-config.json`, before mode selection existed, must keep
/// working unchanged.
#[test]
fn a_configured_judge_still_wins_over_self_mode() {
    let dir = bare_fixture();
    let db = dir.path().join("thor.db");
    // Deliberately: no guard-capture-mode.json at all.
    let sentinel = dir.path().join("judge-was-invoked.marker");
    let config = serde_json::json!({
        "command": env!("CARGO_BIN_EXE_fake_judge"),
        "args": ["sentinel", sentinel.to_str().unwrap(), "block", "from now on always run the linter first"],
        "timeout_ms": 5000,
    });
    std::fs::write(dir.path().join("guard-judge-config.json"), config.to_string()).unwrap();

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    let stop_out = run_hook(&db, &stop_payload("s1"));

    assert!(sentinel.exists(), "a configured judge must still be invoked when no mode override says otherwise");
    let v: serde_json::Value = serde_json::from_str(&stop_out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{stop_out}");
    assert!(v["reason"].as_str().unwrap().contains("judge"), "must be the JUDGE reason, not self-adjudicate's: {stop_out}");
}

/// THE DEFECT THIS PREVENTS: `off` is supposed to be a full kill switch - not
/// "the judge is skipped" or "self mode is skipped", but nothing at all,
/// anywhere the capture guard could otherwise act. Proven against a marker
/// crafted directly with an ACTUAL, unpaid, judge-decided Block (the
/// strongest signal this guard can ever produce) so this is not merely "off
/// happens to have nothing to block yet" - both C2 (Stop) and C3
/// (PreToolUse, mirror sink) must ignore it completely.
#[test]
fn off_mode_never_blocks_anything() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-capture-mode.json"), r#"{"mode":"off"}"#).unwrap();

    let marker = serde_json::json!({
        "s1": {
            "prompt": "from now on always run the linter first",
            "seq_at_flag": 0,
            "blocked_once": false,
            "tier": "block",
            "quote": "from now on always run the linter first",
            "rule_id": "judge",
            "house_style": false,
        }
    });
    std::fs::write(dir.path().join("capture-owed.json"), marker.to_string()).unwrap();

    let stop_out = run_hook(&db, &stop_payload("s1"));
    assert!(stop_out.trim().is_empty(), "off mode must never block Stop, even against an actual judge-decided Block: {stop_out}");

    let sink_out = run_hook(&db, &pretool_write_payload("s1", dir.path(), "CLAUDE.md"));
    assert!(sink_out.trim().is_empty(), "off mode must never block a mirror-sink write either: {sink_out}");
}

/// THE DEFECT THIS PREVENTS: a subagent's own delegated task prompt (full of
/// "always"/"never"/"from now on", written by an orchestrating agent, not
/// the owner) gets flagged and blocked by self-adjudicate mode exactly like
/// the owner's own prompt would - the subagent gate (`payload_is_from_a_
/// subagent`, `serve/src/bin/serve.rs`) must hold regardless of which mode
/// is active.
#[test]
fn a_subagent_is_never_blocked_in_self_adjudicate_mode() {
    let dir = bare_fixture();
    let db = dir.path().join("thor.db");
    // No judge config at all: self is the default mode for this scenario.

    run_hook(&db, &subagent_prompt_payload("sub1", dir.path(), "from now on always run the linter first"));

    let marker_path = dir.path().join("capture-owed.json");
    if marker_path.exists() {
        let text = std::fs::read_to_string(&marker_path).unwrap();
        let markers: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&text).unwrap_or_default();
        assert!(markers.get("sub1").is_none(), "a subagent's own prompt must never be flagged: {text}");
    }

    let stop_out = run_hook(&db, &subagent_stop_payload("sub1"));
    assert!(stop_out.trim().is_empty(), "a subagent's Stop must never be blocked by self-adjudicate mode: {stop_out}");
}
