//! Lane C's capture guard must NEVER fire inside a Task-tool subagent - fixed
//! 2026-08-05 after Claude Code's own hooks documentation confirmed:
//! `SessionStart` fires ONLY in the main session, but `UserPromptSubmit`,
//! `Stop` and `PreToolUse` all DO fire inside a subagent, and only in that
//! case do their payloads carry `agent_id`/`agent_type`. A subagent's own
//! task prompt is written by an orchestrating agent, not the owner, and is
//! routinely full of the very words ("always", "never", "from now on") this
//! guard watches for - flagging it would deadlock the subagent's own Stop on
//! a "decision" the owner never made, in the exact delegated-task workflow
//! this project runs on.
//!
//! Proven here against the real compiled `serve hook` binary, across all
//! three capture-guard surfaces (C1/C2/C3), and cross-checked against the
//! owner's own main session still working exactly as before - this fix
//! narrows one thing, it is not a kill switch.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CAPTURE_RULEBOOK: &str = r#"[
  {"id":"decision-standing-rule","tier":"block",
   "any_of":["from now on","vanaf nu","never again"],
   "none_of":["any_of","for example","bijvoorbeeld","als voorbeeld","?"]}
]"#;

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

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-capture-rulebook.json"), CAPTURE_RULEBOOK).unwrap();
    dir
}

fn configure_fake_judge_block(dir: &Path) {
    let config = serde_json::json!({
        "command": env!("CARGO_BIN_EXE_fake_judge"),
        "args": ["block", "from now on always run the linter first"],
        "timeout_ms": 5000,
    });
    std::fs::write(dir.join("guard-judge-config.json"), config.to_string()).unwrap();
}

fn marker_path(dir: &Path) -> PathBuf {
    dir.join("capture-owed.json")
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

fn subagent_pretool_write_payload(session_id: &str, cwd: &Path, file_path: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": { "file_path": file_path },
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
    })
    .to_string()
}

/// THE DEFECT THIS PREVENTS: a subagent's own delegated task prompt (full of
/// "always"/"never"/"from now on", written by an orchestrating agent, not
/// the owner) creates a capture debt exactly like the owner's own prompt
/// would - C1's whole job, misapplied to the wrong author.
#[test]
fn a_subagent_prompt_never_creates_a_capture_debt() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    configure_fake_judge_block(dir.path());

    run_hook(&db, &subagent_prompt_payload("sub1", dir.path(), "from now on always run the linter first"));

    let marker_file = marker_path(dir.path());
    if marker_file.exists() {
        let text = std::fs::read_to_string(&marker_file).unwrap();
        let markers: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&text).unwrap_or_default();
        assert!(markers.get("sub1").is_none(), "a subagent's own prompt must never be flagged: {text}");
    }
    // A marker file that was never even created is an equally valid proof
    // that nothing was flagged.
}

/// THE DEFECT THIS PREVENTS: a subagent's Stop is blocked over a "decision"
/// it never made, deadlocking the exact delegated-task workflow this project
/// runs on. Proven even when a debt SOMEHOW exists for that session id
/// (crafted directly here, bypassing C1 entirely) - defense in depth: the
/// gate must hold regardless of how the marker got there.
#[test]
fn a_subagent_turn_is_never_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    configure_fake_judge_block(dir.path());

    let marker = serde_json::json!({
        "sub1": {
            "prompt": "from now on always run the linter first",
            "seq_at_flag": 0,
            "blocked_once": false,
        }
    });
    std::fs::write(marker_path(dir.path()), marker.to_string()).unwrap();

    let out = run_hook(&db, &subagent_stop_payload("sub1"));
    assert!(out.trim().is_empty(), "a subagent's Stop must never be blocked by the capture guard: {out}");
}

/// The narrow scope of this fix, C3's half: a subagent's own tool call at a
/// mirror sink is never touched by the capture guard either, even with a
/// real, ACTUAL judge-decided Block on record for that session id.
#[test]
fn a_subagent_sink_write_is_never_touched_by_capture() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();

    let marker = serde_json::json!({
        "sub1": {
            "prompt": "from now on always run the linter first",
            "seq_at_flag": 0,
            "blocked_once": false,
            "tier": "block",
            "quote": "from now on always run the linter first",
            "rule_id": "judge",
            "house_style": false,
        }
    });
    std::fs::write(marker_path(dir.path()), marker.to_string()).unwrap();

    let out = run_hook(&db, &subagent_pretool_write_payload("sub1", dir.path(), "CLAUDE.md"));
    assert!(out.trim().is_empty(), "a subagent's own tool call must never be touched by C3: {out}");
}

/// Scoped, not a kill switch: the OWNER's own main session (no `agent_id` at
/// all) still gets flagged and blocked normally, in the SAME store - proving
/// this fix narrows exactly one thing.
#[test]
fn the_owners_own_main_session_is_still_flagged_and_blocked_normally() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    configure_fake_judge_block(dir.path());

    let main_prompt = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "main1",
        "cwd": dir.path().to_string_lossy(),
        "prompt": "from now on always run the linter first",
    })
    .to_string();
    run_hook(&db, &main_prompt);

    let main_stop = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "main1",
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string();
    let out = run_hook(&db, &main_stop);
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "the owner's own main session must still be blocked normally: {out}");
}
