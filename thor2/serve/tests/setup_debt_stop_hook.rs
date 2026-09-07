//! THE FOURTH DEBT (`serve::setup_debt`, `serve/src/bin/serve.rs`), end to
//! end through the real compiled `serve hook` binary - mirrors
//! `stale_guard_stop_hook.rs`'s own shape for the same reason: the pure
//! decision logic already has its own unit tests in `serve.rs`'s own
//! `setup_debt_tests` module; this file proves the WIRING - the real event
//! store, the real hook JSON shape on stdout, and above all the one property
//! that cannot be proven at the unit level at all, because it lives in
//! `hook_once`'s payload dispatch rather than in `setup_debt` itself: this
//! debt must NEVER hold a subagent's own Stop.

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

/// A Stop payload with an EMPTY last assistant message, deliberately: the
/// Response Guard has nothing to say either way, so every test below proves
/// the setup debt on its own, not a Response Guard match riding along
/// (mirrors `stale_guard_stop_hook.rs`'s own `stop_payload`).
fn stop_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string()
}

/// The same shape, plus `agent_id` - Claude Code's own documented signal
/// (per `payload_is_from_a_subagent`'s doc comment in `serve.rs`) that a Stop
/// payload arrived from inside a Task-tool subagent rather than the owner's
/// own main session.
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

/// The exact shape `ops::install::working_contract` seeds the real note as -
/// written out by hand (`serve/tests` has no dependency on `ops`), but it has
/// to be declared through the real gate, the same way installing for real
/// does, so this is what an end-to-end run actually produces.
fn declare_setup_note(store: &mut thor_core::event_store::EventStore) {
    let item = model::item::Item {
        id: model::store::SETUP_NOTE_ID.to_string(),
        kind: model::item::Kind::Rule,
        text: "walk the owner through setup once, then retract this note".to_string(),
        bindings: vec![model::item::Binding::Always],
        severity: None,
        project: None,
        tags: vec!["working-contract".to_string()],
        expires: None,
        key: None,
        falsifier: Some("this note is still served after the owner already answered".to_string()),
        check: None,
    };
    model::store::declare(store, "install", "install", "installer", &item).unwrap();
}

// ------------------------------------------------------------- the block

/// THE DEFECT THIS PREVENTS: setup being only prose in the note's own body,
/// exactly as easy to skip as any other sentence a busy session is handed.
#[test]
fn a_live_setup_note_holds_the_turn_and_names_both_conditions() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("First session with a new owner"), "{reason}");
    assert!(reason.contains(model::store::OWNER_SETUP_ANSWERS_ID), "must name where the answers go: {reason}");
    assert!(reason.contains(model::store::SETUP_NOTE_ID), "must name the note itself: {reason}");
    assert!(reason.to_lowercase().contains("does not want"), "a reluctant owner must be told this is a valid answer: {reason}");
}

/// THE HARD CONSTRAINT: a subagent's own Stop must never be held over a
/// setup conversation it has no owner in the room to have.
#[test]
fn a_subagents_stop_is_never_held_even_with_a_live_setup_note() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    drop(store);

    let out = run_hook(&db, &subagent_stop_payload("s1"));
    assert!(out.trim().is_empty(), "a subagent's Stop must never be held for setup: {out}");
}

/// A store that never had the note seeded at all - an old install, or one
/// that predates this feature - must never be held either.
#[test]
fn a_store_with_no_seeded_note_is_never_held() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "nothing was ever seeded, so there is nothing to ask: {out}");
}

/// Once the note is retracted through the real gate (answers on record
/// first, exactly as the gate requires - see `model::gate::retract`), the
/// same session's next Stop is silent.
///
/// The answers item below carries no project - deliberately, not merely
/// left blank: a real first session writes it on a store that has never
/// named one, and `model::gate::declare`'s own ground 21 exempts this exact
/// id from needing one for exactly that reason (see that ground's doc
/// comment). A store with a seeded project would never exercise that
/// exemption at all.
#[test]
fn retracting_the_note_through_the_real_gate_silences_the_next_stop() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);

    let answers = model::item::Item {
        id: model::store::OWNER_SETUP_ANSWERS_ID.to_string(),
        kind: model::item::Kind::Report,
        text: "reply length: short. language: Dutch. lanes: work only.".to_string(),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    model::store::declare(&mut store, "s1", "s1", "agent", &answers).unwrap();
    model::store::retract(&mut store, "s1", "s1", "agent", model::store::SETUP_NOTE_ID, "owner walked through setup")
        .expect("the gate must accept this retract once the answers are on record");
    drop(store);

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "a retracted note must never hold the turn: {out}");
}
