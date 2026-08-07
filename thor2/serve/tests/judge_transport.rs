//! The judge transport (`serve::judge`), end to end through the real
//! compiled `serve hook` binary and the `fake_judge` test fixture (test
//! fixture only, `serve/src/bin/fake_judge.rs` - never a real judge). Pure
//! parsing/config logic already has its own unit tests in
//! `serve/src/judge.rs`; this file proves the things only a real external
//! process can prove: a genuine timeout, a genuine unparseable answer, and -
//! critically - that the judge is never even started at UserPromptSubmit.
//!
//! `serve/tests/capture_guard_hook.rs` covers the end-to-end C1/C2/C3 wiring
//! shape (including a judge BLOCK verdict actually blocking, and a paid debt
//! overriding it); this file is narrower and specifically about the
//! transport's own failure modes.

use std::io::Write;
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

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    // No fallback rulebook either, in most of these tests: several of them
    // want to isolate the JUDGE transport's own failure behavior from the
    // fallback rulebook's, so both must be absent to see the pure "no
    // classification at all" case. Tests that need the fallback write their
    // own rulebook file explicitly.
    dir
}

fn judge_config(dir: &Path, args: &[&str], timeout_ms: u64) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let config = serde_json::json!({
        "command": env!("CARGO_BIN_EXE_fake_judge"),
        "args": args,
        "timeout_ms": timeout_ms,
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
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string()
}

/// THE DEFECT THIS PREVENTS: no judge config file exists at all (the quiet,
/// ordinary case - SPEC: "this must be the quiet, ordinary case, not an
/// error") and, with no fallback rulebook either, Stop must neither block
/// nor produce any error output.
///
/// Pinned to `mode: judge` explicitly (OPTION4-IMPLEMENTATION.md): since
/// self-adjudicate is now the DEFAULT whenever no judge is configured, this
/// exact scenario (no judge config, no on-disk rulebook) would otherwise now
/// go through self-adjudicate mode instead - and self mode's own compiled-in
/// narrow trigger default WOULD fire on this exact prompt and block once, by
/// design (see `capture_guard_self_adjudicate.rs`). This test's own point was
/// always narrower than that: the JUDGE transport's own missing-config
/// fail-open behavior, still a real, still-tested property - so it forces
/// judge mode to keep testing exactly that, undisturbed by the new default.
#[test]
fn a_missing_judge_config_never_blocks_and_never_errors() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    std::fs::write(dir.path().join("guard-capture-mode.json"), r#"{"mode":"judge"}"#).unwrap();
    // Deliberately: no guard-judge-config.json, no guard-capture-rulebook.json.

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    let stop_out = run_hook(&db, &stop_payload("s1"));
    assert!(stop_out.trim().is_empty(), "no judge config and no fallback: must ALLOW silently, got: {stop_out}");
}

/// THE DEFECT THIS PREVENTS: a judge command that runs long past its
/// configured timeout is waited on anyway, either blocking the turn forever
/// or eventually returning a stale/late verdict that gets treated as real.
/// The command here sleeps for 5 seconds; timeout_ms is 200 - the whole hook
/// call must return well under the sleep duration, and never block.
#[test]
fn a_judge_command_that_times_out_never_blocks() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    judge_config(dir.path(), &["sleep", "5000", "block", "should never be read"], 200);

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));

    let start = std::time::Instant::now();
    let stop_out = run_hook(&db, &stop_payload("s1"));
    let elapsed = start.elapsed();

    assert!(stop_out.trim().is_empty(), "a timed-out judge call must never block turn end: {stop_out}");
    assert!(
        elapsed < std::time::Duration::from_millis(4000),
        "the hook must return once its configured timeout elapses, not wait for the slow command: took {elapsed:?}"
    );
}

/// THE DEFECT THIS PREVENTS: the judge command runs and exits promptly, but
/// its output does not parse into BLOCK/WARN/ALLOW - and that garbage is
/// silently treated as some verdict (worst case, a block) instead of "no
/// verdict at all".
#[test]
fn a_judge_answer_that_does_not_parse_never_blocks() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    judge_config(dir.path(), &["garbage"], 5000);

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    let stop_out = run_hook(&db, &stop_payload("s1"));
    assert!(stop_out.trim().is_empty(), "an unparseable judge answer must never block turn end: {stop_out}");
}

/// THE DEFECT THIS PREVENTS: the judge is invoked at UserPromptSubmit
/// instead of Stop, paying its multi-second latency in front of every prompt
/// - exactly what SPEC-ENFORCEMENT.md's redesign exists to avoid. Proven
/// directly: the configured judge writes a sentinel file the instant it is
/// invoked, at ANY mode. If UserPromptSubmit ever called it, the sentinel
/// would exist right after that call; it must not.
#[test]
fn the_judge_is_never_invoked_at_user_prompt_submit() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    let sentinel = dir.path().join("judge-was-invoked.marker");
    judge_config(dir.path(), &["sentinel", sentinel.to_str().unwrap(), "allow"], 5000);

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));
    assert!(!sentinel.exists(), "the judge must never be invoked at UserPromptSubmit (C1)");

    // Sanity check that the fixture actually works and WOULD have left
    // proof behind: Stop (C2) is where classification happens, so the
    // sentinel must exist after it.
    run_hook(&db, &stop_payload("s1"));
    assert!(sentinel.exists(), "the judge must be invoked at Stop (C2) - the fixture itself must be exercised");
}

/// The debt-paid short-circuit (an optimization, not just a correctness
/// requirement): when the debt is already paid, the judge should not even be
/// consulted - proven the same way, with a sentinel. If this ever regresses
/// to "always ask the judge, then discard the answer if paid", this test
/// still would not catch a WRONG discard (that is `capture.rs`'s own unit
/// test's job), but it does prove the latency-saving skip actually happens.
#[test]
fn a_paid_debt_skips_the_judge_call_entirely() {
    let dir = fixture();
    let db = dir.path().join("thor.db");
    let sentinel = dir.path().join("judge-was-invoked.marker");
    judge_config(dir.path(), &["sentinel", sentinel.to_str().unwrap(), "block", "irrelevant"], 5000);

    run_hook(&db, &prompt_payload("s1", dir.path(), "from now on always run the linter first"));

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
    assert!(stop_out.trim().is_empty(), "a paid debt must not block: {stop_out}");
    assert!(!sentinel.exists(), "a paid debt must skip the judge call entirely, saving the round trip");
}
