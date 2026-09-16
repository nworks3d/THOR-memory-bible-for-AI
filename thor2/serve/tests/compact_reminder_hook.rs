//! Proof, against the REAL compiled `serve` binary (same style as
//! `serve/tests/session_start_project_resolution.rs` and
//! `serve/tests/subagent_hook_behavior.rs`), of the reminder added after a
//! context compaction: a replay on two real sessions found the one real
//! miss was a "done"/"tested" claim made right after a `/compact`, with
//! nothing re-checked. Claude Code's own SessionStart payload carries
//! `"source": "compact"` exactly then - see
//! `serve::session_start::compact_reminder`'s own doc comment for the rest
//! of the reasoning, including why the subagent check here is defense in
//! depth rather than a live gate.

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
        // Gate ground 11: this file tests the compact reminder, not teeth,
        // and a generic "standing rule N" has no literal to catch.
        tags: vec![format!("{}a test fixture with nothing literal to catch", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some(format!("standing rule {id} turns out to be wrong")),
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
    let _ = child.stdin.take().unwrap().write_all(stdin_payload.as_bytes());
    let out = child.wait_with_output().expect("failed to wait on the serve binary");
    (out.status, out.stdout, out.stderr)
}

fn session_start_payload(fields: serde_json::Value) -> String {
    let mut payload = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "SessionStart",
    });
    for (k, v) in fields.as_object().expect("fields must be a JSON object") {
        payload[k] = v.clone();
    }
    payload.to_string()
}

fn seed_store(db_path: &std::path::Path, rule_id: &str) {
    let mut db = EventStore::new(db_path).unwrap();
    store::declare(&mut db, "s", "l", "a", &always_rule(rule_id)).unwrap();
}

#[test]
fn a_compact_sessionstart_in_the_main_session_includes_the_reminder() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("store.db");
    seed_store(&db_path, "g1");

    let payload = session_start_payload(serde_json::json!({"source": "compact"}));
    let (status, stdout, stderr) = run_hook(&db_path, &payload);
    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty());
    let out = String::from_utf8(stdout).unwrap();
    assert!(out.contains("standing rule g1"), "the rules block must still be there: {out}");
    assert!(
        out.contains(serve::session_start::COMPACT_REMINDER),
        "expected the compact reminder in the SessionStart output, got: {out}"
    );
}

#[test]
fn startup_resume_and_clear_sources_never_include_the_reminder() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("store.db");
    seed_store(&db_path, "g2");

    for source in ["startup", "resume", "clear"] {
        let payload = session_start_payload(serde_json::json!({"source": source}));
        let (status, stdout, stderr) = run_hook(&db_path, &payload);
        assert_eq!(status.code(), Some(0));
        assert!(stderr.is_empty());
        let out = String::from_utf8(stdout).unwrap();
        assert!(
            !out.contains(serve::session_start::COMPACT_REMINDER),
            "source {source} must never include the reminder, got: {out}"
        );
    }
}

#[test]
fn a_subagent_payload_never_includes_the_reminder_even_with_source_compact() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("store.db");
    seed_store(&db_path, "g3");

    // SessionStart never actually carries agent_id from Claude Code itself
    // (see `payload_is_from_a_subagent`'s own doc comment in bin/serve.rs) -
    // this is defense in depth, proven the same way regardless.
    let payload = session_start_payload(serde_json::json!({"source": "compact", "agent_id": "a1dca2c0feb7f44fb"}));
    let (status, stdout, stderr) = run_hook(&db_path, &payload);
    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty());
    let out = String::from_utf8(stdout).unwrap();
    assert!(
        !out.contains(serve::session_start::COMPACT_REMINDER),
        "a subagent payload must never include the reminder, got: {out}"
    );
}

/// "the existing SessionStart output is otherwise unchanged": strip the new
/// paragraph back out of the compact payload's own block and the result
/// must equal the plain payload's block, byte for byte - not merely "still
/// contains the rule text somewhere".
#[test]
fn the_sessionstart_output_for_a_compact_payload_is_otherwise_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("store.db");
    seed_store(&db_path, "g4");

    let plain_payload = session_start_payload(serde_json::json!({}));
    let (plain_status, plain_stdout, _) = run_hook(&db_path, &plain_payload);
    assert_eq!(plain_status.code(), Some(0));
    let plain_json: serde_json::Value = serde_json::from_slice(&plain_stdout).unwrap();
    let plain_block = plain_json["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string();

    let compact_payload = session_start_payload(serde_json::json!({"source": "compact"}));
    let (compact_status, compact_stdout, _) = run_hook(&db_path, &compact_payload);
    assert_eq!(compact_status.code(), Some(0));
    let compact_json: serde_json::Value = serde_json::from_slice(&compact_stdout).unwrap();
    let compact_block = compact_json["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string();

    let compact_minus_reminder =
        compact_block.replace(serve::session_start::COMPACT_REMINDER, "").trim_end().to_string();
    assert_eq!(
        compact_minus_reminder,
        plain_block.trim_end(),
        "the compact payload's block, with the new paragraph removed, must equal the plain payload's block exactly"
    );
    assert_eq!(
        plain_json["hookSpecificOutput"]["hookEventName"], compact_json["hookSpecificOutput"]["hookEventName"],
        "the event name must be unaffected"
    );
    assert!(
        compact_json.get("systemMessage").is_none(),
        "this fixture has no anchored items, so no decay notice should appear either: {compact_json}"
    );
}
