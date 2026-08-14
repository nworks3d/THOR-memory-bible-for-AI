//! Lane C's capture guard (SPEC-ENFORCEMENT.md section 2), end to end through
//! the real compiled `serve hook` binary, exactly like
//! `response_guard_stop_hook.rs` does for the Response Guard. The pure
//! decision logic (`serve::capture`, `serve::judge`) already has its own unit
//! tests; this file proves the WIRING - the real rulebook/judge-config file,
//! the real marker file on disk, the real store, and the real hook JSON
//! shape on stdout - ties those pure functions together correctly across C1
//! (UserPromptSubmit), C2 (Stop) and C3 (PreToolUse).
//!
//! Redesigned 2026-08-05 (`JUDGE-TRANSPORT.md`): classification now happens
//! at Stop via `serve::judge`, not at UserPromptSubmit via the rulebook. The
//! `fake_judge` binary (test fixture only, `serve/src/bin/fake_judge.rs`) is
//! used here as the configured judge command so these tests exercise a REAL
//! external process, not a stub - `serve/tests/judge_transport.rs` covers the
//! transport-specific defects (timeout, unparseable answer, never invoked at
//! UserPromptSubmit) in more detail; this file covers the end-to-end C1/C2/C3
//! wiring shape.
//!
//! All prompt text here is generic and synthetic (a linter policy) - never
//! copied from a real session, per this workspace's own stance on test
//! fixtures.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// The fallback rulebook - only ever reachable here when no judge config is
/// written for a given test, or the judge produces no verdict.
const CAPTURE_RULEBOOK: &str = r#"[
  {"id":"decision-standing-rule","tier":"block",
   "any_of":["from now on","vanaf nu","never again"],
   "none_of":["any_of","for example","bijvoorbeeld","als voorbeeld","?"]},
  {"id":"style-house-style","tier":"block",
   "any_of":["the new primary color","de nieuwe huisstijl"],
   "none_of":["any_of","for example","bijvoorbeeld","als voorbeeld","?"]},
  {"id":"decision-soft","tier":"warn",
   "any_of":["i would prefer","ik wil liever"],
   "none_of":["any_of","?"]}
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
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
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

/// Writes `guard-judge-config.json` beside the store, pointed at the
/// `fake_judge` test fixture with the given mode/args and a generous timeout
/// (these tests are not timing anything except the dedicated timeout test).
fn configure_fake_judge(dir: &Path, mode_args: &[&str]) {
    let args: Vec<String> = mode_args.iter().map(|s| s.to_string()).collect();
    let config = serde_json::json!({
        "command": env!("CARGO_BIN_EXE_fake_judge"),
        "args": args,
        "timeout_ms": 5000,
    });
    std::fs::write(dir.join("guard-judge-config.json"), config.to_string()).unwrap();
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
    // last_assistant_message deliberately empty: the Response Guard has
    // nothing to say either way, so this proves the capture check on its
    // own, not a Response Guard match riding along.
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

/// C1 -> C2, end to end, with a judge configured to BLOCK: the prompt is
/// recorded unclassified at C1, and Stop asks the judge, which blocks turn
/// end, naming its own citation and the "judge" source.
#[test]
fn c1_records_and_c2_blocks_on_a_judge_block_verdict() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    configure_fake_judge(dir.path(), &["block", "from now on always run the linter first"]);

    let prompt_out =
        run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    // UserPromptSubmit's own injection surfaces resolve to nothing here (no
    // items declared at all), so its own output is empty - the marker write
    // is a side effect, not reflected in this call's stdout. The judge must
    // also never have been consulted yet at this point (see
    // judge_transport.rs's dedicated sentinel test for direct proof).
    assert!(prompt_out.trim().is_empty(), "no items declared, so no injection block: {prompt_out}");

    let stop_out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&stop_out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{stop_out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("from now on always run the linter first"), "{reason}");
    assert!(reason.contains("judge"), "must name the judge as the source: {reason}");
}

/// With NO judge configured at all, the same prompt that used to block
/// outright now only ever reaches the fallback rulebook - which is
/// warn-only, so Stop must never block, even though the rulebook rule itself
/// is tagged "block" in its own JSON.
///
/// Pinned to `mode: judge` explicitly (OPTION4-IMPLEMENTATION.md): with no
/// judge configured, self-adjudicate is now the DEFAULT, and self mode WOULD
/// block once on this exact prompt (that is its whole point) - so this test
/// forces judge mode to keep testing its own specific, still-real property:
/// the JUDGE-mode fallback rulebook can only ever warn, never block. Self
/// mode's own "blocks once when owed" behavior has its own test in
/// `capture_guard_self_adjudicate.rs`.
#[test]
fn with_no_judge_configured_the_fallback_rulebook_never_blocks() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    std::fs::write(dir.path().join("guard-capture-mode.json"), r#"{"mode":"judge"}"#).unwrap();
    // Deliberately no guard-judge-config.json written.

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    let stop_out = run_hook(&db, &stop_payload("s1"));
    assert!(stop_out.trim().is_empty(), "no judge configured: the fallback must only ever warn, never block: {stop_out}");
}

/// C1 -> (a real remember lands) -> C2: the debt is paid, so Stop must not
/// block EVEN THOUGH the configured judge would say BLOCK if asked - the
/// store-sequence check governs, not the judge.
#[test]
fn recording_the_decision_before_stop_pays_the_debt_even_with_a_judge_configured_to_block() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    configure_fake_judge(dir.path(), &["block", "from now on always run the linter first"]);

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));

    // A real remember: append a fact_created event to the SAME store the
    // hook will read at Stop.
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
    assert!(stop_out.trim().is_empty(), "a paid debt must not block turn end, even if the judge would say BLOCK: {stop_out}");
}

/// C3, restored 2026-08-05 (see `capture::sink_verdict`'s own doc comment
/// and `JUDGE-TRANSPORT.md`'s "C3 restored" section): a mirror-sink write
/// gets a WARN the instant C1's own cheap rulebook match is recorded
/// (`provisional_tier`), well before this turn's Stop has judged anything -
/// but that warn never blocks the tool call. Only once Stop has run AND the
/// judge itself said BLOCK does C3 actually block, and only then.
#[test]
fn c3_warns_provisionally_then_blocks_once_stop_confirms_it() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    configure_fake_judge(dir.path(), &["block", "from now on always run the linter first"]);

    // Nothing owed yet: an edit to CLAUDE.md is ordinary work.
    let out = run_hook(&db, &pretool_write_payload("s1", dir.path(), "CLAUDE.md"));
    assert!(out.trim().is_empty(), "no owed capture: CLAUDE.md must not be blocked: {out}");

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));

    // C1 has recorded the prompt AND its own cheap rulebook match
    // (provisional). Stop has not run yet, so this must WARN (visible,
    // non-blocking context), never block.
    let out = run_hook(&db, &pretool_write_payload("s1", dir.path(), "CLAUDE.md"));
    assert!(
        out.contains("not yet confirmed by the judge"),
        "a provisional rulebook match must warn within the same turn: {out}"
    );
    assert!(!out.contains("\"decision\":\"block\""), "a provisional warning must never actually block: {out}");

    // Stop runs, the judge says BLOCK, the marker now carries an ACTUAL,
    // unpaid, judge-decided Block.
    let stop_out = run_hook(&db, &stop_payload("s1"));
    let v: serde_json::Value = serde_json::from_str(&stop_out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{stop_out}");

    // Owed and unpaid, judge-decided: CLAUDE.md is now actually blocked.
    let out = run_hook(&db, &pretool_write_payload("s1", dir.path(), "CLAUDE.md"));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("mirror"), "{out}");

    // Owed and unpaid, but an ordinary source file: never blocked or warned.
    let out = run_hook(&db, &pretool_write_payload("s1", dir.path(), "src/lib.rs"));
    assert!(out.trim().is_empty(), "an unrelated file must never be caught by C3: {out}");
}

/// Safety-model test 9, in wiring form: the marker says a capture is owed,
/// but the store it must be checked against cannot be read (corrupt file) -
/// so whether the debt was paid can never be proven, and the invariant
/// (SPEC-ENFORCEMENT.md 1.1/1.2) says that must be ALLOW, never BLOCK. No
/// judge config is even needed here: the store failure short-circuits
/// before classification would ever be attempted.
#[test]
fn a_fact_whose_currency_cannot_be_proven_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    // A store that exists as a file but is not a readable SQLite database.
    std::fs::write(&db, b"not a sqlite database, just garbage bytes").unwrap();
    // A real, well-formed, already-judged, unpaid block-tier marker for this
    // session - crafted directly so this test does not depend on C1/C2
    // (which would themselves need a working store).
    let marker = serde_json::json!({
        "s1": {
            "prompt": "from now on always run the linter first",
            "tier": "block",
            "quote": "from now on always run the linter first",
            "seq_at_flag": 0,
            "blocked_once": false,
            "rule_id": "judge",
            "house_style": false,
        }
    });
    std::fs::write(dir.path().join("capture-owed.json"), marker.to_string()).unwrap();

    let out = run_hook(&db, &stop_payload("s1"));
    assert!(out.trim().is_empty(), "currency cannot be proven against a corrupt store: must ALLOW, got: {out}");
}
