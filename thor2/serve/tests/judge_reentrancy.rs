//! The judge reentrancy guard (`serve::reentry`, see its own doc comment and
//! `JUDGE-TRANSPORT.md`'s "Judge reentrancy" section for the whole hazard
//! this closes), proven end to end against the real compiled `serve` binary
//! and the `fake_judge` test fixture's `respawn` mode - the one thing a pure
//! unit test cannot exercise: a REAL nested process tree, marked the exact
//! way an actual headless `claude` CLI child session would be.
//!
//! `serve::reentry`'s own unit tests (`serve/src/reentry.rs`) cover the pure
//! depth/parsing logic; `judge::tests::the_judge_refuses_to_spawn_when_the_
//! mark_is_already_present` (`serve/src/judge.rs`) covers the judge
//! transport's own hard refusal in isolation, by mutating this ONE process'
//! own environment directly. Every test in THIS file instead sets
//! `THOR_JUDGE_DEPTH` only on a spawned `Command` (never on the test process
//! itself), so nothing here needs to worry about racing other tests that
//! happen to run in parallel in the same binary.

use model::item::{Binding, Item, Kind, Severity};
use model::store;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

const JUDGE_DEPTH_ENV: &str = "THOR_JUDGE_DEPTH";

fn always_rule(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: "a standing rule that must always be injected at session start".to_string(),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Irreversible),
        project: None,
        // Gate ground 11: a synthetic standing rule has no literal to catch.
        tags: vec![format!("{}a test fixture with nothing literal to catch", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some("this rule turns out to be wrong for this synthetic fixture".to_string()),
        check: None,
    }
}

fn session_start_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
    })
    .to_string()
}

fn prompt_payload(session_id: &str, text: &str) -> String {
    serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session_id,
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

fn pretool_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "tool_name": "Read",
        "tool_input": {},
    })
    .to_string()
}

/// Runs `serve --db <db> hook`, optionally marking the CHILD process (never
/// this test process) with `THOR_JUDGE_DEPTH`, the same way `judge::
/// run_judge_command` marks the real judge command it spawns.
fn run_hook(db: &Path, payload: &str, marked_depth: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(depth) = marked_depth {
        cmd.env(JUDGE_DEPTH_ENV, depth);
    }
    let mut child = cmd.spawn().expect("spawn serve");
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    child.wait_with_output().expect("wait for serve")
}

fn fixture_with_always_rule() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        store::declare(&mut store, "setup", "setup", "setup", &always_rule("standing-rule")).unwrap();
    }
    (dir, db)
}

fn times_served(db: &Path, id: &str) -> usize {
    let store = EventStore::new(db).unwrap();
    serve::audit::audit_rows(&store).into_iter().find(|r| r.id == id).map(|r| r.stats.times_served).unwrap_or(0)
}

// --------------------------------------------------------------------------

/// THE DEFECT THIS PREVENTS: a `serve hook` process that is itself a
/// descendant of a judge invocation (marked via `THOR_JUDGE_DEPTH`, inherited
/// exactly the way a real child Claude Code session's own hook process would
/// be) still injects a standing rule at SessionStart, or still records a
/// capture debt at UserPromptSubmit - polluting the judge's own context and
/// the store's own usage statistics, the exact hazard `JUDGE-TRANSPORT.md`'s
/// "Judge reentrancy" section exists to close.
#[test]
fn a_marked_hook_invocation_injects_nothing_and_writes_nothing() {
    let (dir, db) = fixture_with_always_rule();

    // SessionStart: an ordinary, unmarked call injects the standing rule and
    // records its delivery - proven for contrast in
    // `an_unmarked_ordinary_hook_invocation_is_completely_unaffected` below.
    // Marked, it must produce nothing at all.
    let out = run_hook(&db, &session_start_payload("s1"), Some("1"));
    assert!(out.stdout.is_empty(), "a marked SessionStart must inject nothing, got: {:?}", out.stdout);
    assert_eq!(times_served(&db, "standing-rule"), 0, "a marked SessionStart must never record a delivery");

    // UserPromptSubmit, with a plainly decision-shaped prompt: an ordinary,
    // unmarked call flags a capture debt (C1) - proven below. Marked, it
    // must write no marker file at all.
    let out = run_hook(&db, &prompt_payload("s1", "from now on always run the linter first"), Some("1"));
    assert!(out.stdout.is_empty(), "a marked UserPromptSubmit must inject nothing, got: {:?}", out.stdout);
    let marker_path = dir.path().join("capture-owed.json");
    assert!(!marker_path.exists(), "a marked UserPromptSubmit must never write a capture marker at all");

    // PreToolUse and Stop: neither surface produces output here either (no
    // file touched, no debt owed, nothing declared for Stop to judge) - the
    // point is that the marked process does not even reach far enough to
    // try, proven together with the exit-status test below.
    let out = run_hook(&db, &pretool_payload("s1"), Some("1"));
    assert!(out.stdout.is_empty());
    let out = run_hook(&db, &stop_payload("s1"), Some("1"));
    assert!(out.stdout.is_empty());
}

/// THE DEFECT THIS PREVENTS: a marked hook invocation is distinguishable
/// from an ordinary hook call by its EXIT STATUS or stderr - reading as a
/// crash or a refusal instead of "a hook that had nothing to say", the one
/// shape every real Claude Code hook (SessionStart, UserPromptSubmit,
/// PreToolUse, Stop) is allowed to take.
#[test]
fn a_marked_hook_invocation_exits_successfully_not_as_an_error() {
    let (_dir, db) = fixture_with_always_rule();

    for payload in [
        session_start_payload("s1"),
        prompt_payload("s1", "from now on always run the linter first"),
        pretool_payload("s1"),
        stop_payload("s1"),
    ] {
        let out = run_hook(&db, &payload, Some("1"));
        assert!(out.status.success(), "a marked hook call must exit 0, not error, for payload {payload}: {out:?}");
        assert!(
            out.stderr.is_empty(),
            "a marked hook call must never write to stderr, for payload {payload}: {:?}",
            out.stderr
        );
    }

    // A deeper mark (as if this were somehow a second level of nesting) must
    // refuse exactly the same way - the guard is "depth > 0", not "depth ==
    // exactly 1".
    let out = run_hook(&db, &session_start_payload("s1"), Some("2"));
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

/// THE DEFECT THIS PREVENTS: the guard is meant to fire ONLY for a process
/// descended from a judge call - an ORDINARY hook invocation, with no
/// `THOR_JUDGE_DEPTH` in its environment at all (every real Claude Code hook
/// today, since the judge is not switched on anywhere yet), must behave
/// EXACTLY as it did before this guard existed. This is the single most
/// important property in the whole task: invisible in normal operation.
#[test]
fn an_unmarked_ordinary_hook_invocation_is_completely_unaffected() {
    let (dir, db) = fixture_with_always_rule();

    let out = run_hook(&db, &session_start_payload("s1"), None);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("standing rule that must always be injected"),
        "an unmarked SessionStart must still inject: {stdout}"
    );
    assert_eq!(times_served(&db, "standing-rule"), 1, "an unmarked SessionStart must still record its delivery");

    let out = run_hook(&db, &prompt_payload("s1", "from now on always run the linter first"), None);
    assert!(out.status.success());
    let marker_path = dir.path().join("capture-owed.json");
    assert!(marker_path.exists(), "an unmarked UserPromptSubmit must still flag a capture debt as before");
}

/// THE DEFECT THIS PREVENTS: THOR's Stop hook spawns the judge, whose child
/// session's own Stop hook (simulated here by `fake_judge`'s `respawn` mode -
/// see its own doc comment in `serve/src/bin/fake_judge.rs`) fires ANOTHER
/// judge call, unbounded - each level spawning processes and, with a real
/// hosted model configured, burning real tokens. Proven here against a REAL
/// process tree, not a mock: the configured "judge" command re-invokes the
/// real, compiled `serve hook` binary the exact way a nested Claude Code
/// session's own Stop hook would, feeding it a genuine, owed capture marker
/// for the SAME session - so without the guard, the inner call would have
/// every reason to spawn the judge again. The reentrancy guard must stop it
/// cold at depth 1 instead of letting it recurse.
#[test]
fn a_judge_child_that_re_invokes_serve_terminates_instead_of_recursing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();

    let counter_file = dir.path().join("judge-invoked.count");
    let inner_stdout_file = dir.path().join("inner-stdout.bin");
    let inner_exit_file = dir.path().join("inner-exit.txt");

    let judge_args = vec![
        "respawn".to_string(),
        env!("CARGO_BIN_EXE_serve").to_string(),
        db.to_string_lossy().to_string(),
        "s1".to_string(),
        counter_file.to_string_lossy().to_string(),
        inner_stdout_file.to_string_lossy().to_string(),
        inner_exit_file.to_string_lossy().to_string(),
        "allow".to_string(),
    ];
    let config = serde_json::json!({
        "command": env!("CARGO_BIN_EXE_fake_judge"),
        "args": judge_args,
        "timeout_ms": 10_000,
    });
    std::fs::write(dir.path().join("guard-judge-config.json"), config.to_string()).unwrap();

    // Flag a real, unpaid capture debt for session s1 first - the
    // precondition for the OUTER Stop call below to reach `classify_capture`
    // (and hence the judge) at all.
    let out = run_hook(&db, &prompt_payload("s1", "from now on always run the linter first"), None);
    assert!(out.status.success());

    let start = std::time::Instant::now();
    let stop_out = run_hook(&db, &stop_payload("s1"), None);
    let elapsed = start.elapsed();

    assert!(stop_out.status.success(), "the outer Stop call must still exit 0: {stop_out:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "the whole call must terminate promptly, not hang or recurse: took {elapsed:?}"
    );

    let invocations = std::fs::read_to_string(&counter_file).unwrap_or_default();
    let invocation_count = invocations.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        invocation_count, 1,
        "the judge command must be invoked EXACTLY ONCE - a second line here would mean the inner, \
         marked serve process reached the judge again instead of the guard stopping it: {invocations:?}"
    );

    let inner_stdout = std::fs::read(&inner_stdout_file).unwrap_or_default();
    assert!(inner_stdout.is_empty(), "the inner, marked serve invocation must print nothing: {inner_stdout:?}");
    let inner_exit = std::fs::read_to_string(&inner_exit_file).unwrap_or_default();
    assert_eq!(inner_exit.trim(), "0", "the inner, marked serve invocation must exit 0, not as an error");
}
