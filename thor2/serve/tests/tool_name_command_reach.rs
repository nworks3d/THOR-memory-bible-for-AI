//! The tool-name reach (`hook_once`'s PreToolUse branch in
//! `serve/src/bin/serve.rs`), end to end through the real compiled `serve
//! hook` binary.
//!
//! MEASURED: a Rule bound to Target command "Agent" (or "Artifact",
//! "SendUserFile") was only ever matched inside the requires guard - the
//! general injection surface built its `ServeInput` from a Bash-style
//! "command" field alone, so such a rule was never SERVED when the tool it
//! names was actually called, and nine pinned rules had to stay on Always
//! only to be seen at all. A non-shell tool call now offers its own bare
//! NAME as a command, the same way a shell command offers its own text.

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

fn tool_payload(session_id: &str, cwd: &Path, tool_name: &str, tool_input: serde_json::Value) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": tool_name,
        "tool_input": tool_input,
    })
    .to_string()
}

fn additional_context(out: &str) -> Option<String> {
    if out.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value =
        serde_json::from_str(out).unwrap_or_else(|e| panic!("expected a context JSON object: {e}: {out}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().map(str::to_string)
}

/// A real store with one live Orientation bound to `Target { Command,
/// "Agent" }` - the exact shape the report measured nine pinned rules
/// forced into `Always` to work around.
fn fixture_with_agent_bound_orientation(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = model::item::Item {
        id: "agent-swarm-cheap-model".to_string(),
        kind: model::item::Kind::Orientation,
        text: "name a cheap model on every agent you spawn".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Command,
            value: "Agent".to_string(),
        }],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a swarm without that field turns out to be cheap anyway".to_string()),
        check: None,
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a real Agent tool call (no "command"
/// field in its own tool_input - it carries a "prompt" instead) serves the
/// rule bound to Command "Agent" - through the real compiled binary.
#[test]
fn a_command_bound_rule_is_served_on_the_named_tools_own_call() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_agent_bound_orientation(dir.path());

    let out = run_hook(
        &db,
        &tool_payload("s1", dir.path(), "Agent", serde_json::json!({"prompt": "summarise these three files"})),
    );
    let context = additional_context(&out).unwrap_or_else(|| panic!("expected context: {out}"));
    assert!(context.contains("agent-swarm-cheap-model"), "{context}");
}

/// THE DEFECT THIS PREVENTS: a Command anchor naming a specific tool must
/// not turn into a blanket rule that fires on every tool call. A Read call
/// - unrelated, and carrying no "command" field either - must not serve an
/// item bound only to Command "Agent".
#[test]
fn a_command_bound_rule_is_not_served_on_an_unrelated_tools_call() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_agent_bound_orientation(dir.path());

    let out = run_hook(&db, &tool_payload("s1", dir.path(), "Read", serde_json::json!({"file_path": "NOTES.md"})));
    let context = additional_context(&out);
    assert!(
        context.as_deref().map_or(true, |c| !c.contains("agent-swarm-cheap-model")),
        "an Agent-bound rule must not fire on an unrelated tool: {out}"
    );
}

/// A forbidden-literal check against a bare tool name must stay harmless:
/// this exercises the general serving path with a Command-anchored rule
/// that ALSO carries a hard-guard-shaped `Absent` check, proving the bare
/// tool name reaching the informational surface never trips a BLOCK - the
/// hard guard (`command_guard_block`) only ever reads a real "command"
/// field, never `tool_name`, so it cannot see this call at all.
#[test]
fn a_forbidden_literal_check_against_a_bare_tool_name_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("POLICY.md"), "# policy\n").unwrap();
    let item = model::item::Item {
        id: "no-em-dash-in-agent-calls".to_string(),
        kind: model::item::Kind::Rule,
        text: "an Agent call never carries an em dash".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Command,
            value: "Agent".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in an Agent call and nobody notices".to_string()),
        check: Some(model::item::Check::Absent { path: "POLICY.md".to_string(), literal: "\u{2014}".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();

    let out = run_hook(&db, &tool_payload("s1", dir.path(), "Agent", serde_json::json!({"prompt": "summarise"})));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected JSON: {e}: {out}"));
    assert_ne!(
        v.get("decision"),
        Some(&serde_json::Value::String("block".to_string())),
        "a bare tool name reaching the informational surface must never block: {out}"
    );
}
