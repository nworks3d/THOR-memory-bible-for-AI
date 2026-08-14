//! Wires `respond::guard_verdict`'s WARN tier into `hook_once`'s `Stop` arm -
//! end to end, through the real compiled `serve hook` binary, the same
//! discipline every sibling Stop-hook test file in this directory uses
//! (`response_guard_stop_hook.rs` for BLOCK, `capture_guard_hook.rs` /
//! `capture_guard_self_adjudicate.rs` for Lane C, `stale_guard_stop_hook.rs`
//! for the maintenance guard).
//!
//! THE DEFECT THIS WHOLE FILE EXISTS TO PREVENT: before this wiring, a
//! rulebook rule could carry `"tier":"warn"` and `respond::guard_verdict`
//! would faithfully compute its text into `GuardVerdict::warn_reason` - and
//! then nothing ever read that field. The tier looked like a real capability
//! and was not one. Each test below is named after one way that could still
//! go wrong even once something DOES read it: the warn could leak into a
//! block, or it could swallow/delay a block that should have fired after it,
//! or a block could stop being immediate once the warn path exists alongside
//! it, or the ordinary untiered case could regress.
//!
//! All rulebook text and prompt/reply text here is generic and synthetic,
//! never copied from a real session, matching this workspace's own stance on
//! test fixtures (see `capture_guard_self_adjudicate.rs`'s own doc comment).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `serve --db <db> hook` with `payload` on stdin, return its stdout.
/// Asserts the hook always exits 0 and never writes to stderr - same
/// discipline `capture_guard_self_adjudicate.rs` and `stale_guard_stop_hook.rs`
/// already hold this binary to.
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

fn stop_payload(session_id: &str, last_assistant_message: &str, already_fired: bool) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": already_fired,
        "last_assistant_message": last_assistant_message,
    })
    .to_string()
}

fn subagent_stop_payload(session_id: &str, last_assistant_message: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": last_assistant_message,
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
    })
    .to_string()
}

/// Drives Lane C's C1 (`UserPromptSubmit`) with a real, decision-shaped
/// prompt, so `capture_stop_check` finds a genuine unpaid debt at Stop - the
/// exact phrase `capture_guard_self_adjudicate.rs`'s own
/// `the_default_mode_blocks_once_on_an_owed_capture_with_nothing_configured_at_all`
/// already proves trips the compiled-in narrow trigger default under the
/// default (nothing-configured) `SelfAdjudicate` mode.
fn flag_a_real_capture_debt(db: &Path, session_id: &str, cwd: &Path) {
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "prompt": "from now on always run the linter first",
    })
    .to_string();
    run_hook(db, &payload);
}

/// Write `absent-guard-stale.json` directly beside the store, with exactly
/// one outstanding entry - the same fixture shape and the same isolation
/// choice `stale_guard_stop_hook.rs`'s own `write_stale_sidecar` uses (the
/// staleness sidecar is written directly rather than through a real
/// PreToolUse write that goes stale, so this file's tests are about the
/// Stop-time ordering, not about how an entry gets into the sidecar).
fn write_stale_sidecar(dir: &Path, id: &str) {
    let sidecar = serde_json::json!({
        id: {
            "outcome": "failed",
            "check": "Absent { path: \"NOTES.md\", literal: \"forbidden\" }",
            "file": "NOTES.md",
            "count": 1,
            "seq_at_record": 0,
        }
    });
    std::fs::write(dir.join("absent-guard-stale.json"), sidecar.to_string()).unwrap();
}

fn fresh_store(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    db
}

const WARN_ONLY_RULEBOOK: &str = r#"[
  {"id":"prefer-warn","tier":"warn",
   "any_of":["ik zou liever dit anders zien"],"none_of":["any_of"],
   "reminder":"phrase this as a preference, not a directive"}
]"#;

const BLOCK_TIER_RULEBOOK: &str = r#"[
  {"id":"hard-block","tier":"block",
   "any_of":["dat nooit meer doen"],"none_of":["any_of"],
   "reminder":"this is a hard rule, not a suggestion"}
]"#;

const UNTIERED_RULEBOOK: &str = r#"[
  {"id":"no-tier-at-all",
   "any_of":["mystery phrase"],"none_of":["any_of"],
   "reminder":"no tier key at all, must still block"}
]"#;

const WARN_MESSAGE: &str = "ik zou liever dit anders zien, maar het is jouw project.";

// -------------------------------------------------------------------- warn

/// THE DEFECT THIS PREVENTS: a rule marked `tier: "warn"` fires and its text
/// is simply lost - the exact gap this task exists to close. A matching
/// reply with nothing else pending must produce visible text, on the `Stop`
/// event, that carries no `"decision"` key at all (see `HookOutput::Warn`'s
/// own doc comment in `serve.rs` for why that specific absence is the whole
/// safety argument for this shape).
#[test]
fn a_warn_tier_rule_produces_visible_text_and_does_not_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_store(dir.path());
    std::fs::write(dir.path().join("guard-response-rulebook.json"), WARN_ONLY_RULEBOOK).unwrap();

    let out = run_hook(&db, &stop_payload("s1", WARN_MESSAGE, false));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected JSON output, got {out:?}: {e}"));

    assert!(v.get("decision").is_none(), "a warn must never carry a \"decision\" key: {out}");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "Stop", "{out}");
    let text = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a string additionalContext: {out}"));
    assert!(text.contains("preference"), "{text}");
}

/// THE DEFECT THIS PREVENTS: `additionalContext` alone reaches the model,
/// which cannot act on it until a later turn (the turn is already over by
/// the time a `Stop` warn fires), while the owner - the only audience who
/// can still do something about it now - never sees anything at all. Per
/// Claude Code's own hooks documentation (`HookOutput::Warn`'s own doc
/// comment in `serve.rs` spells out exactly what that documentation settles
/// and what it still leaves open), the field that reaches the owner is a
/// different, top-level `systemMessage`. Both fields must carry the
/// identical text, neither replaces the other, and neither ever adds a
/// `"decision"` key.
#[test]
fn a_warn_emits_a_top_level_system_message_and_still_carries_additional_context() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_store(dir.path());
    std::fs::write(dir.path().join("guard-response-rulebook.json"), WARN_ONLY_RULEBOOK).unwrap();

    let out = run_hook(&db, &stop_payload("s1", WARN_MESSAGE, false));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected JSON output, got {out:?}: {e}"));

    assert!(v.get("decision").is_none(), "a warn must never carry a \"decision\" key: {out}");
    let system_message = v["systemMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a top-level systemMessage: {out}"));
    let additional_context = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a string additionalContext: {out}"));
    assert!(system_message.contains("preference"), "{system_message}");
    assert_eq!(
        system_message, additional_context,
        "systemMessage and additionalContext must carry the identical text: {out}"
    );
}

/// THE DEFECT THIS PREVENTS: a warn-tier match at Stop silently eats a real,
/// unpaid Lane-C capture debt instead of merely riding along in front of it -
/// the ordering guarantee this task's whole wiring exists to keep for the
/// capture check specifically.
#[test]
fn a_warn_does_not_suppress_a_later_capture_guard_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_store(dir.path());
    std::fs::write(dir.path().join("guard-response-rulebook.json"), WARN_ONLY_RULEBOOK).unwrap();
    flag_a_real_capture_debt(&db, "s1", dir.path());

    let out = run_hook(&db, &stop_payload("s1", WARN_MESSAGE, false));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "the capture guard's block must still fire after a warn: {out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.contains("from now on always run the linter first"),
        "must be the capture guard's own block, naming its own quote, not lost: {reason}"
    );
}

/// Same guarantee, proven for the OTHER surface that sits after the Response
/// Guard in the `Stop` arm: the stale-rule maintenance guard. A warn must
/// neither suppress nor merely delay its block past this same Stop call.
#[test]
fn a_warn_does_not_delay_the_maintenance_guards_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_store(dir.path());
    std::fs::write(dir.path().join("guard-response-rulebook.json"), WARN_ONLY_RULEBOOK).unwrap();
    write_stale_sidecar(dir.path(), "r1");

    let out = run_hook(&db, &stop_payload("s1", WARN_MESSAGE, false));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "the maintenance guard's block must still fire in this same Stop call: {out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("r1"), "must be the maintenance guard's own block, naming its id: {reason}");
}

// ------------------------------------------------------------------- block

/// THE DEFECT THIS PREVENTS: introducing the warn path anywhere weakens the
/// existing guarantee that a BLOCK returns immediately - either by letting a
/// later surface's block overwrite or merge with it, or by letting Lane C /
/// the maintenance guard run at all when they should never even be reached.
/// Both the capture guard and the maintenance guard are armed here so either
/// would ALSO block if reached; the capture marker's raw bytes are compared
/// before and after to prove `capture_stop_check` never even ran (it
/// rewrites that file on every real invocation - see its own doc comment),
/// not merely that its text lost some race.
#[test]
fn a_block_still_returns_immediately_and_nothing_after_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_store(dir.path());
    std::fs::write(dir.path().join("guard-response-rulebook.json"), BLOCK_TIER_RULEBOOK).unwrap();

    flag_a_real_capture_debt(&db, "s1", dir.path());
    let marker_path = dir.path().join("capture-owed.json");
    let marker_before = std::fs::read_to_string(&marker_path).expect("C1 must have written a marker");
    write_stale_sidecar(dir.path(), "r1");

    let out = run_hook(&db, &stop_payload("s1", "dat nooit meer doen, begrepen?", false));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("hard rule"), "must be the Response Guard's own block: {reason}");
    assert!(!reason.contains("from now on"), "must not be the capture guard's reason: {reason}");
    assert!(!reason.contains("r1"), "must not be the maintenance guard's reason: {reason}");

    let marker_after = std::fs::read_to_string(&marker_path).unwrap();
    assert_eq!(
        marker_before, marker_after,
        "capture_stop_check must never have run or touched the marker file once the Response Guard blocked"
    );
}

/// THE DEFECT THIS PREVENTS: switching `hook_once` from `respond::block_reason`
/// to `respond::guard_verdict` silently changes behaviour for the ordinary,
/// untiered rule shape every rulebook shipped before this task actually uses
/// (`respond.rs`'s own `an_unknown_or_missing_tier_defaults_to_block` proves
/// the pure default; this proves the WIRING still reaches it).
#[test]
fn a_rule_with_no_tier_still_blocks_exactly_as_today() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_store(dir.path());
    std::fs::write(dir.path().join("guard-response-rulebook.json"), UNTIERED_RULEBOOK).unwrap();

    let out = run_hook(&db, &stop_payload("s1", "a mystery phrase appears here", false));
    let v: serde_json::Value = serde_json::from_str(&out).expect("a decision JSON");
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("no tier key at all"), "{out}");
}

// ---------------------------------------------------------------- subagent

/// THE DEFECT THIS PREVENTS: the subagent branch this task's own change
/// touches (`if payload_is_from_a_subagent(&payload) { ... }`) starts
/// dropping a pending warn instead of returning it, the moment the branch's
/// return value stopped being an unconditional `None`. `JUDGE-TRANSPORT.md`
/// documents the Response Guard as UNCHANGED by subagent status (only Lane C
/// is gated) - this proves that still holds for the warn tier specifically,
/// not only for block.
#[test]
fn a_pending_warn_still_reaches_a_subagents_stop() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_store(dir.path());
    std::fs::write(dir.path().join("guard-response-rulebook.json"), WARN_ONLY_RULEBOOK).unwrap();

    let out = run_hook(&db, &subagent_stop_payload("s1", WARN_MESSAGE));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected JSON output, got {out:?}: {e}"));
    assert!(v.get("decision").is_none(), "a warn must never carry a \"decision\" key: {out}");
    let text = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a string additionalContext: {out}"));
    assert!(text.contains("preference"), "{text}");
}

// -------------------------------------------------------- other surfaces

/// THE DEFECT THIS PREVENTS: giving `Warn` its own top-level `systemMessage`
/// by editing the wrong match arm in `cmd_hook`, so it leaks onto `Context`
/// too - the shape `SessionStart`, `UserPromptSubmit` and `PreToolUse` all
/// still share (see `HookOutput::Context`'s own doc comment). All three
/// render through that one match arm, so proving it here for `PreToolUse`
/// (the one this file can trigger with the least fixture, same fixture shape
/// `hook_fails_open.rs`'s own `the_same_binary_does_print_a_block_when_nothing_is_wrong`
/// already uses) proves it for all three - this task's own instructions were
/// to leave that arm untouched.
#[test]
fn a_context_surface_still_renders_with_no_system_message() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
        let item = model::item::Item {
            id: "never-force-push".to_string(),
            kind: model::item::Kind::Rule,
            text: "never force-push to main".to_string(),
            bindings: vec![model::item::Binding::Moment(intent::Action::Push)],
            severity: Some(model::item::Severity::Irreversible),
            project: None,
            // Gate ground 11: this file tests the response guard, and the rule
            // is bound to a moment with no literal to catch.
            tags: vec![format!("{}a test fixture with nothing literal to catch", model::store::NO_LITERAL_REASON_PREFIX)],
            expires: None,
            key: None,
            falsifier: Some("a force-push to main lands clean with nobody reverting it".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    }

    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s1",
        "tool_name": "Bash",
        "tool_input": { "command": "git push --force origin main" },
    })
    .to_string();

    let out = run_hook(&db, &payload);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected JSON output, got {out:?}: {e}"));
    assert!(v.get("systemMessage").is_none(), "a Context surface must never carry systemMessage: {out}");
    assert!(v.get("decision").is_none(), "a Context surface must never carry a decision key: {out}");
    let context = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("expected hookSpecificOutput.additionalContext: {out}"));
    assert!(context.contains("never force-push to main"), "{context}");
}
