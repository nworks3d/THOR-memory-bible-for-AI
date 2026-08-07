//! The Response Guard, end to end through the real `serve hook` binary on a
//! Stop payload. This is surface 5 - the one that watches the assistant, not
//! the store - and the one whose ABSENCE was the sharpest regression of the
//! whole rebuild: 2.0 was built without it, so for a full session untidy
//! replies went out with nothing to catch them (see serve::respond).
//!
//! Every case drives the actual compiled binary the hook fires, with a real
//! rulebook file beside a real (empty) store, exactly as production does.

use std::io::Write;
use std::process::{Command, Stdio};

const RULEBOOK: &str = r#"[
  {"id":"no-plain-language-tldr",
   "any_of":["md5","commit ","gepusht","fsck","exit=0"],
   "none_of":["tldr","kort:","any_of"],
   "min_chars":600,
   "reminder":"open with a TLDR in plain language"},
  {"id":"ask-user-to-check",
   "any_of":["kun je checken","can you check"],
   "none_of":["any_of"],
   "reminder":"check whether you can reach it yourself first"}
]"#;

/// Run `serve --db <db> hook` with `payload` on stdin, return its stdout.
fn run_hook(db: &std::path::Path, payload: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

/// A store the hook can open, plus the rulebook beside it.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    // Create the store so the hook's other branches would work too.
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), RULEBOOK).unwrap();
    dir
}

fn stop_payload(message: &str, already_fired: bool) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "t",
        "stop_hook_active": already_fired,
        "last_assistant_message": message,
    })
    .to_string()
}

/// THE DEFECT THIS PREVENTS: a long technical answer with a commit hash and no
/// TLDR yields to the user unchallenged. It did, for a whole session.
#[test]
fn a_long_technical_reply_without_tldr_is_blocked() {
    let dir = fixture();
    let msg = format!("the commit fsck run is done, all gepusht. {}", "detail ".repeat(120));
    let out = run_hook(&dir.path().join("thor.db"), &stop_payload(&msg, false));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("TLDR"), "{out}");
}

#[test]
fn the_same_reply_with_a_tldr_yields_silently() {
    let dir = fixture();
    let msg = format!("TLDR: het werkt. the commit fsck run is done, gepusht. {}", "detail ".repeat(120));
    let out = run_hook(&dir.path().join("thor.db"), &stop_payload(&msg, false));
    assert!(out.trim().is_empty(), "a clean reply must produce no decision: {out}");
}

/// Loop safety: once a guard has fired this turn, it must never re-block - a
/// still-imperfect revised answer would otherwise loop forever.
#[test]
fn a_guard_that_already_fired_this_turn_never_re_blocks() {
    let dir = fixture();
    let msg = format!("the commit fsck run is done, all gepusht. {}", "detail ".repeat(120));
    let out = run_hook(&dir.path().join("thor.db"), &stop_payload(&msg, true));
    assert!(out.trim().is_empty(), "stop_hook_active=true must suppress the block: {out}");
}

/// Capability amnesia is caught too - the class that had me ask the owner to
/// do things I could do myself, this very session.
#[test]
fn asking_the_user_to_do_what_i_can_is_blocked() {
    let dir = fixture();
    let out = run_hook(&dir.path().join("thor.db"), &stop_payload("kun je checken of dat bestand er staat?", false));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{out}");
}

/// Fail-open: no rulebook beside the store means the guard blocks nothing,
/// never errors, still exits 0.
#[test]
fn a_missing_rulebook_blocks_nothing_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    // deliberately NO rulebook file beside it
    let msg = format!("the commit fsck run is done, gepusht. {}", "detail ".repeat(120));
    let out = run_hook(&db, &stop_payload(&msg, false));
    assert!(out.trim().is_empty(), "no rulebook = no block: {out}");
}
