//! The stale-rule Stop guard (`serve::stale_guard`), end to end through the
//! real compiled `serve hook` binary - mirrors `capture_guard_hook.rs`'s own
//! shape for exactly the same reason: the pure decision logic already has
//! its own unit tests in `serve::stale_guard`'s own test module; this file
//! proves the WIRING - the real staleness sidecar `serve::absent_guard`
//! writes, the real event store, the real block-marker and mode-config files
//! on disk, and the real hook JSON shape on stdout, for a real Stop payload.
//!
//! The staleness sidecar is written DIRECTLY here (never through a real
//! PreToolUse write that goes stale) - the same choice
//! `capture_guard_hook.rs`'s own `a_fact_whose_currency_cannot_be_proven_
//! never_blocks` test makes for `capture-owed.json`: it isolates this
//! guard's own Stop-time behavior from the unrelated question of how an
//! entry gets INTO the sidecar in the first place, which
//! `absent_check_guard.rs` already covers end to end.

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
/// the stale guard on its own, not a Response Guard match riding along
/// (mirrors `capture_guard_hook.rs`'s own `stop_payload`).
fn stop_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string()
}

/// Write `absent-guard-stale.json` directly beside the store, with exactly
/// one entry - the shape `serve::absent_guard::StaleRecord` serialises to.
fn write_stale_sidecar(dir: &Path, id: &str, outcome: &str, check: &str, file: &str, seq_at_record: i64, count: u64) {
    let sidecar = serde_json::json!({
        id: {
            "outcome": outcome,
            "check": check,
            "file": file,
            "count": count,
            "seq_at_record": seq_at_record,
        }
    });
    std::fs::write(dir.join("absent-guard-stale.json"), sidecar.to_string()).unwrap();
}

/// A minimal, valid Rule - enough to pass `model::store::declare`'s own
/// gate (a Rule needs at least one binding) - declared under `id`. Returns
/// nothing; the caller reads it back via `model::store::show` when it needs
/// to build a revision.
fn declare_test_item(store: &mut thor_core::event_store::EventStore, id: &str) {
    let item = model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: "a rule that will go stale".to_string(),
        bindings: vec![model::item::Binding::Always],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("this rule turns out not to matter".to_string()),
        check: None,
    };
    model::store::declare(store, "s", "l", "a", &item).unwrap();
}

// ------------------------------------------------------------- the block

/// THE DEFECT THIS PREVENTS: a rule the Absent-check guard already recorded
/// as unable to prove its own currency sits in the sidecar forever, never
/// surfaced to anyone - this proves the block actually fires, and that its
/// reason names the id and the outcome so a reader can find and fix it.
#[test]
fn an_outstanding_stale_entry_blocks_turn_end_once_and_names_the_id_and_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\", literal: \"forbidden\" }", "NOTES.md", 0, 1);

    let out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("r1"), "must name the id: {reason}");
    assert!(reason.contains("FAILED"), "must name the outcome: {reason}");
    assert!(reason.contains("NOTES.md"), "must name the file: {reason}");
}

/// THE DEFECT THIS PREVENTS: a `could_not_run` outcome (the check could not
/// even be attempted - an escaping path, no resolvable root) is rendered
/// identically to a `failed` one, hiding which of the two actually happened.
#[test]
fn an_entry_whose_check_could_not_run_is_labeled_distinctly_in_the_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    write_stale_sidecar(dir.path(), "r2", "could_not_run", "Absent { path: \"../escape.txt\" }", "whatever.md", 0, 1);

    let out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("COULD NOT RUN"), "{out}");
}

/// THE DEFECT THIS PREVENTS: the same session gets told about the same
/// outstanding entry on every Stop for the rest of the session - the "once
/// per session, ever" contract this guard exists to keep.
#[test]
fn the_same_session_is_never_blocked_twice() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 0, 1);

    let first = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");

    let second = run_hook(&db, &stop_payload("s1"));
    assert!(second.trim().is_empty(), "the same session must not be blocked twice: {second}");
}

/// A DIFFERENT session, with the SAME entry still outstanding, must still be
/// told - the marker is session-keyed, never global.
#[test]
fn a_different_session_is_still_blocked_by_the_same_outstanding_entry() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 0, 1);

    let first = run_hook(&db, &stop_payload("s1"));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&first).unwrap()["decision"], "block", "{first}");

    let second = run_hook(&db, &stop_payload("s2"));
    let v: serde_json::Value = serde_json::from_str(&second).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {second}"));
    assert_eq!(v["decision"], "block", "a different session must still be told: {second}");
}

// -------------------------------------------------------------- settlement

/// THE DEFECT THIS PREVENTS: an item the owner already revised since the
/// staleness was recorded keeps being reported as if nothing had happened.
#[test]
fn an_entry_whose_item_was_revised_after_it_was_recorded_no_longer_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_test_item(&mut store, "r1"); // seq 1

    // Recorded right after creation (seq 1) - before any revise.
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 1, 1);

    // The owner revises it afterward (seq 2).
    let existing = model::store::show(&store, "r1").unwrap();
    let mut updated = existing.clone();
    updated.text = "a rule that was fixed".to_string();
    model::store::revise(&mut store, "s", "l", "a", &existing, &updated).unwrap();
    drop(store);

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "a revised item must no longer block: {out}");
}

/// THE DEFECT THIS PREVENTS: an item the owner already retracted since the
/// staleness was recorded keeps being reported as if it still applied.
#[test]
fn an_entry_whose_item_was_retracted_after_it_was_recorded_no_longer_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_test_item(&mut store, "r1"); // seq 1

    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 1, 1);

    model::store::retract(&mut store, "s", "l", "a", "r1", "the anchor moved and this rule no longer applies").unwrap();
    drop(store);

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "a retracted item must no longer block: {out}");
}

/// THE DEFECT THIS PREVENTS (precision of "since it was recorded"): a
/// revise that happened BEFORE the staleness was last recorded must NOT
/// settle it - otherwise any item ever revised for any unrelated reason,
/// however long ago, would silently clear every future staleness against
/// it, defeating the guard.
#[test]
fn an_item_revised_before_the_entry_was_recorded_still_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_test_item(&mut store, "r1"); // seq 1

    let existing = model::store::show(&store, "r1").unwrap();
    let mut updated = existing.clone();
    updated.text = "revised for a reason unrelated to the later staleness".to_string();
    model::store::revise(&mut store, "s", "l", "a", &existing, &updated).unwrap(); // seq 2

    // Recorded AFTER the revise above - the revise cannot possibly be what
    // settles this occurrence.
    let now_seq = store.get_next_seq().unwrap() - 1; // 2
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", now_seq, 1);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("a revise that happened BEFORE the entry was recorded must not settle it: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
}

/// Nothing in the sidecar at all - the ordinary, overwhelmingly common case.
#[test]
fn nothing_outstanding_means_no_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    // No staleness sidecar written at all.

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "nothing stale must never block: {out}");
}

// ------------------------------------------------------------- off switch

/// THE DEFECT THIS PREVENTS: with no documented way to turn this guard off,
/// the only recourse for a deployment that wants it disabled is deleting or
/// editing the sidecar itself - this proves the documented switch works.
#[test]
fn the_off_switch_disables_the_guard_entirely() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 0, 1);
    std::fs::write(dir.path().join("stale-guard-mode.json"), r#"{"mode":"off"}"#).unwrap();

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "the off switch must disable this guard entirely: {out}");
}

// ------------------------------------------------------------- fail-open

/// THE DEFECT THIS PREVENTS: a corrupt (unparseable) staleness sidecar
/// crashes the hook or, worse, is misread as something that must block.
#[test]
fn a_corrupt_stale_sidecar_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("absent-guard-stale.json"), "{ not json").unwrap();

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "a corrupt sidecar must never block: {out}");
}

/// Safety-model parallel to `capture_guard_hook.rs`'s own
/// `a_fact_whose_currency_cannot_be_proven_never_blocks`: an outstanding
/// entry is on record, but the store it must be checked against cannot even
/// be opened - so settlement can never be proven either way, and that must
/// be ALLOW, never BLOCK.
#[test]
fn a_store_that_will_not_open_never_blocks_even_with_an_outstanding_entry() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    std::fs::write(&db, b"not a sqlite database, just garbage bytes").unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 0, 1);

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "a store that will not open must never block: {out}");
}

/// A malformed once-per-session marker must never crash or, worse, be
/// misread as "already blocked forever" - it reads back as fresh instead,
/// and this guard still blocks normally.
#[test]
fn a_malformed_block_marker_never_prevents_a_real_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 0, 1);
    std::fs::write(dir.path().join("stale-guard-blocked.json"), "{ not json").unwrap();

    let out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
}

// --------------------------------------------------- ordering / precedence

const RESPONSE_RULEBOOK: &str = r#"[
  {"id":"no-plain-language-tldr",
   "any_of":["md5","commit ","gepusht","fsck","exit=0"],
   "none_of":["tldr","kort:","any_of"],
   "min_chars":600,
   "reminder":"open with a TLDR in plain language"}
]"#;

/// THE DEFECT THIS PREVENTS: adding a third Stop-time check disturbs the
/// first one - the Response Guard must still fire exactly as it did before
/// this guard existed, even with an outstanding stale entry ALSO present,
/// and its own reason (not this guard's) must be what comes back.
#[test]
fn the_response_guards_own_behavior_at_stop_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), RESPONSE_RULEBOOK).unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 0, 1);

    let msg = format!("the commit fsck run is done, all gepusht. {}", "detail ".repeat(120));
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s1",
        "stop_hook_active": false,
        "last_assistant_message": msg,
    })
    .to_string();

    let out = run_hook(&db, &payload);
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("TLDR"), "the Response Guard's own reason must still win: {out}");
}

/// Loop safety: `stop_hook_active: true` must still suppress every check
/// that can HOLD the turn, this guard included. Since 2026-08-09 the Response
/// Guard is the one exception, and only in its warning form - see
/// `a_second_pass_still_says_what_it_found_as_a_warning` for why going fully
/// blind on a second pass was a defect. A warning holds nothing, so the loop
/// safety this test exists for is unaffected.
#[test]
fn a_guard_that_already_fired_this_turn_still_suppresses_everything_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), RESPONSE_RULEBOOK).unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 0, 1);

    let msg = format!("the commit fsck run is done, all gepusht. {}", "detail ".repeat(120));
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s1",
        "stop_hook_active": true,
        "last_assistant_message": msg,
    })
    .to_string();

    let out = run_hook(&db, &payload);
    assert!(!out.contains("\"decision\":\"block\""), "a second pass must hold nothing: {out}");
    assert!(!out.contains("r1"), "and the stale-rule guard must stay silent, exactly as before: {out}");
}

const CAPTURE_RULEBOOK: &str = r#"[
  {"id":"decision-generic","tier":"block",
   "any_of":["from now on","never again"],
   "none_of":["any_of","for example","?"]}
]"#;

/// THE DEFECT THIS PREVENTS: Lane C's capture guard, which already sits
/// between the Response Guard and this new guard, is disturbed by this
/// guard's addition - its own block must still win, and this guard must
/// never even run when Lane C already had something to say.
#[test]
fn the_capture_guards_own_behavior_at_stop_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-capture-mode.json"), r#"{"mode":"self"}"#).unwrap();
    std::fs::write(dir.path().join("guard-capture-rulebook.json"), CAPTURE_RULEBOOK).unwrap();
    write_stale_sidecar(dir.path(), "r1", "failed", "Absent { path: \"NOTES.md\" }", "NOTES.md", 0, 1);

    let prompt_payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s1",
        "cwd": dir.path().to_string_lossy(),
        "prompt": "from now on always run the linter first",
    })
    .to_string();
    run_hook(&db, &prompt_payload);

    let out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("durable decision"), "the capture guard's own reason must still win: {out}");
}
