//! Detection of this memory's own "write a fact" MCP tool calls
//! (`mcp__<server>__remember` / `mcp__<server>__revise`) as the
//! `intent::Action::Remember` moment, end to end through the real compiled
//! `serve hook` binary. The pure suffix-matching logic
//! (`is_remember_moment`/`mcp_tool_suffix` in `serve/src/bin/serve.rs`) has
//! its own unit tests in that same file (`remember_moment_tests`); this file
//! proves the WIRING - a real store, a real Rule bound to
//! `Binding::Moment(Action::Remember)`, and a real PreToolUse payload naming
//! a namespaced MCP tool - actually serves the rule on such a call, and
//! nothing else does.

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

fn tool_payload(session_id: &str, cwd: &Path, tool_name: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": tool_name,
        "tool_input": {},
    })
    .to_string()
}

/// A real store with one live Rule bound to the Remember moment - never
/// derivable from a command or file path (see `intent`'s own closed
/// vocabulary doc comment: `from_command`/`from_path`/`from_draft` none of
/// them produce `Action::Remember`), so the ONLY way this item can ever be
/// served at all is the detection this file proves.
fn fixture_with_remember_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = model::item::Item {
        id: "remember-scope-rule".to_string(),
        kind: model::item::Kind::Rule,
        text: "a written fact stays under 300 characters".to_string(),
        bindings: vec![model::item::Binding::Moment(intent::Action::Remember)],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a 300+ character fact turns out to be fine anyway".to_string()),
        check: None,
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

fn additional_context(out: &str) -> String {
    let v: serde_json::Value =
        serde_json::from_str(out).unwrap_or_else(|e| panic!("expected a context JSON object: {e}: {out}"));
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("expected hookSpecificOutput.additionalContext: {out}"))
        .to_string()
}

/// THE REALISTIC CASE, END TO END: a namespaced call to this memory's own
/// `remember` tool serves a rule bound to the Remember moment - through the
/// real compiled binary, not a unit-level call into the suffix matcher.
#[test]
fn a_namespaced_remember_call_serves_a_rule_bound_to_the_remember_moment() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_remember_rule(dir.path());

    let out = run_hook(&db, &tool_payload("s1", dir.path(), "mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__remember"));
    let context = additional_context(&out);
    assert!(context.contains("remember-scope-rule"), "{context}");
    assert!(context.contains("remember"), "the moment name itself should show up in the header: {context}");
}

/// The other tool name this same detection must cover, on a differently
/// named server too - proves the suffix match, never a literal server name
/// or a literal "remember" hardcoded as the only recognised spelling.
#[test]
fn a_namespaced_revise_call_on_a_different_server_serves_the_same_rule() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_remember_rule(dir.path());

    let out = run_hook(&db, &tool_payload("s1", dir.path(), "mcp__thor2__revise"));
    let context = additional_context(&out);
    assert!(context.contains("remember-scope-rule"), "{context}");
}

/// THE DEFECT THIS PREVENTS: an unrelated tool call - built-in, or a
/// namespaced call to a DIFFERENT tool on the very same server - serves a
/// rule bound only to the Remember moment. The store's own fixture carries
/// nothing else this call could possibly match, so any output at all here
/// would prove a false positive in the detection, not just in a unit test.
#[test]
fn an_unrelated_tool_call_serves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_remember_rule(dir.path());

    let write_out = run_hook(&db, &tool_payload("s1", dir.path(), "Write"));
    assert!(write_out.trim().is_empty(), "an unrelated built-in tool must never serve a Remember-bound rule: {write_out}");

    let lookup_out = run_hook(&db, &tool_payload("s2", dir.path(), "mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__lookup"));
    assert!(
        lookup_out.trim().is_empty(),
        "a different tool on the same MCP server must never serve a Remember-bound rule: {lookup_out}"
    );
}
