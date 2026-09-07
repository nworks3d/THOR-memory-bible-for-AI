//! `respond::guard_verdict`'s `list_request_any_of` exemption (FALSE BLOCK A,
//! reported by the owner from other sessions, fixed 2026-09-07) - end to end,
//! through the real compiled `serve hook` binary, the same discipline every
//! sibling Stop-hook test file in this directory uses
//! (`response_guard_stop_hook.rs` for the plain BLOCK path,
//! `response_guard_warn_stop_hook.rs` for the WARN tier).
//!
//! WHAT IS SPECIFICALLY UNTESTED ANYWHERE ELSE, and the entire reason this
//! file exists rather than folding into `response_guard_stop_hook.rs`:
//! `respond::last_user_prompt` (the pure JSONL parser) has its own unit
//! tests in `respond.rs`, and `respond::guard_verdict`'s list exemption has
//! its own unit tests there too, given the prompt text directly - but
//! nothing before this file ever exercised
//! `serve/src/bin/serve.rs::last_user_prompt_from_payload`, the function
//! that reads a Stop payload's `transcript_path`, reads THAT file off disk,
//! and hands its text to the parser. A bug in that one function - the wrong
//! payload key, a path that never gets opened, the result silently dropped -
//! would be invisible to every other test in this suite, because none of
//! them ever pass a `transcript_path` at all.
//!
//! All rulebook, transcript and prompt/reply text here is generic and
//! synthetic, never copied from a real session, matching this workspace's
//! own stance on test fixtures (see `capture_guard_self_adjudicate.rs`'s own
//! doc comment).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `serve --db <db> hook` with `payload` on stdin, return its stdout.
/// Same discipline every sibling Stop-hook test file in this directory holds
/// this binary to: always exit 0, never write to stderr.
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

fn stop_payload(last_assistant_message: &str, transcript_path: Option<&Path>) -> String {
    let mut v = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "t",
        "stop_hook_active": false,
        "last_assistant_message": last_assistant_message,
    });
    if let Some(p) = transcript_path {
        v["transcript_path"] = serde_json::Value::String(p.to_string_lossy().to_string());
    }
    v.to_string()
}

/// A single-line-per-entry Claude Code transcript whose LAST real user turn
/// is `last_user_text` - one plain `"type":"user"` entry, exactly the shape
/// `respond::last_user_prompt`'s own doc comment describes as the common
/// case. Written to `dir` and returned as a path, never beside the store
/// itself: a real `transcript_path` lives wherever Claude Code's own project
/// transcripts live, never required to sit next to `guard-response-rulebook.json`.
fn write_transcript(dir: &Path, last_user_text: &str) -> std::path::PathBuf {
    let path = dir.join("transcript.jsonl");
    let jsonl = format!(
        "{}\n{}\n",
        serde_json::json!({"type":"user","message":{"role":"user","content":"een eerdere, niet ter zake doende vraag"}}),
        serde_json::json!({"type":"user","message":{"role":"user","content": last_user_text}}),
    );
    std::fs::write(&path, jsonl).unwrap();
    path
}

const LIST_RULEBOOK: &str = r#"[
  {"id":"answer-is-too-long","tier":"block",
   "any_of":[],
   "none_of":["\n> ","```"],
   "min_chars":1000,
   "list_request_any_of":["lijst","overzicht","opsomming","rapport","alle ","welke ","list","overview","report","all the"],
   "reminder":"This answer is over 1000 characters."}
]"#;

fn fresh_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), LIST_RULEBOOK).unwrap();
    (dir, db)
}

fn long_list_reply() -> String {
    let mut s = String::from("Hier is de lijst met alle stappen die je moet volgen:\n");
    for i in 1..=40 {
        s.push_str(&format!("- stap {i}: rond dit onderdeel van de taak af en noteer het resultaat\n"));
    }
    s
}

/// THE DEFECT THIS PREVENTS (FALSE BLOCK A), proven through the real
/// binary's own transcript-reading code path, not just the pure parser: a
/// list the owner explicitly asked for, read back out of a REAL transcript
/// file via `transcript_path`, must not be blocked for its length.
#[test]
fn a_transcript_prompt_that_asked_for_a_list_exempts_a_genuinely_long_list_reply() {
    let (dir, db) = fresh_fixture();
    let transcript = write_transcript(dir.path(), "Kun je een lijst geven van alle stappen die ik moet volgen?");
    let out = run_hook(&db, &stop_payload(&long_list_reply(), Some(&transcript)));
    assert!(out.trim().is_empty(), "a genuine, requested list must not be blocked: {out}");
}

/// Same transcript mechanism, but the LAST real user turn never asked for a
/// list - the exemption must not fire just because a transcript happens to
/// be readable; it needs the owner to actually have asked, in the prompt
/// that led to THIS reply.
#[test]
fn a_transcript_prompt_that_never_asked_for_a_list_still_blocks_the_same_long_reply() {
    let (dir, db) = fresh_fixture();
    let transcript = write_transcript(dir.path(), "Hoe los ik dit prestatieprobleem op?");
    let out = run_hook(&db, &stop_payload(&long_list_reply(), Some(&transcript)));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON: {out}");
    assert_eq!(v["decision"], "block", "{out}");
}

/// Fail-open, proven end to end: a `transcript_path` that does not resolve
/// to a real file must never crash the hook or itself cause a block - it
/// must behave exactly like no `transcript_path` at all, which means the
/// exemption cannot apply and the length rule fires normally.
#[test]
fn an_unreadable_transcript_path_fails_open_and_the_length_rule_still_fires() {
    let (dir, db) = fresh_fixture();
    let missing = dir.path().join("this-file-does-not-exist.jsonl");
    let out = run_hook(&db, &stop_payload(&long_list_reply(), Some(&missing)));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON: {out}");
    assert_eq!(v["decision"], "block", "an unreadable transcript must not silently allow a long reply: {out}");
}

/// A Stop payload with no `transcript_path` at all (an older Claude Code
/// build, or a synthetic caller) must behave exactly as it did before this
/// field existed: no crash, and the length rule still fires on a long reply
/// with no list exemption available.
#[test]
fn no_transcript_path_at_all_still_blocks_the_long_reply() {
    let (_dir, db) = fresh_fixture();
    let out = run_hook(&db, &stop_payload(&long_list_reply(), None));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON: {out}");
    assert_eq!(v["decision"], "block", "{out}");
}
