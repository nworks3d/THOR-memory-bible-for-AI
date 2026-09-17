//! THE EVALUATION DEBT (`serve::usefulness::eval_debt_owed` for the pure
//! predicate, `serve::usefulness::update_eval_debt_state`/`record_eval_
//! debt_asked` for the sidecar writes, `bin/serve.rs`'s own `evaluation_
//! debt` for the Stop-hook wiring), end to end through the real compiled
//! `serve hook` binary - mirrors `setup_debt_stop_hook.rs`'s own shape for
//! the same reason: the pure decision logic already has its own unit tests
//! (`usefulness.rs`'s `eval_debt_predicate_tests`/`eval_debt_state_tests`,
//! `bin/serve.rs`'s `evaluation_debt_tests`); this file proves the WIRING -
//! the real event store, the real sidecar file on disk, the real hook JSON
//! shape on stdout, the "this session has actually worked here long enough"
//! gate, and above all the two properties that cannot be proven at the unit
//! level at all, because they live in `hook_once`'s payload dispatch rather
//! than in `evaluation_debt` itself: this debt must NEVER hold a subagent's
//! own Stop, and it must speak BEFORE the judgement debt when both are due.
//!
//! THE TRIGGER WAS REWRITTEN FOUR TIMES, three of them on 2026-09-16. First,
//! a single global "newest verdict" clock - which any verdict on ANY item
//! applying to a checkout reset, global items included - was replaced with
//! a per-project sidecar (`eval-debt-state.json`, `serve::usefulness::
//! ProjectEvalState`) tracking how long a backlog of ten-or-more items had
//! sat continuously at or over that ceiling, and whether an evaluation
//! report had been seen for it since. Then, the same day, the ceiling
//! itself was dropped entirely in favour of asking daily, per project
//! actually worked in, once the session had put in at least `serve::
//! usefulness::EVAL_MIN_SESSION_MINUTES` there - measured against a rolling
//! 24 hours since the later of `tracking_since`/`last_evaluation_seen`.
//! Then, still the same day, no evaluation was ever asked for a checkout
//! that resolves to no project at all, since no Report can be filed there
//! to silence it (`model::gate`'s ground 21, `NO_SCOPE_PROBLEM`).
//!
//! THE FOURTH REWRITE (owner's decision, 2026-09-16: he does not want to
//! ever have to run an evaluation himself) drops the 24-hour rolling window
//! and `tracking_since` entirely, in favour of a UTC CALENDAR DAY: the
//! obligation holds whenever no evaluation report for this project has been
//! first seen on the current UTC day (`serve::usefulness::eval_done_today`,
//! `crate::time::same_utc_day`), regardless of how long ago THOR started
//! tracking the project - so a project with NO report ever seen fires the
//! very first time enough minutes are worked, with no first-day grace
//! period any more. The once-per-SESSION gate (the old `eval-debt-
//! asked.json`) is retired too: the debt now blocks the FIRST STOP OF EVERY
//! TURN for as long as it holds (`blocks_on_the_stop_of_a_new_turn_again_
//! and_again_while_no_report_exists` below), relying entirely on Claude
//! Code's own `stop_hook_active` (`does_not_block_twice_in_the_same_turn`)
//! for the "at most once per turn" safety - never a second copy of that
//! mechanism. Every ask is now counted on the PROJECT's own sidecar entry
//! (`asked_count`/`first_asked_since_report`) instead of a session's, reset
//! the moment a new report is seen (`the_ask_counter_increments_per_turn_
//! and_resets_once_a_report_is_seen`). `seed_eval_state`/`recently_covered`/
//! `no_longer_covered` below drive `last_evaluation_seen` directly, at an
//! exact instant relative to the real wall clock this binary reads;
//! `seed_tracking_since` (still calling the real, unchanged `update_eval_
//! debt_state` write) remains only for the no-project tests, which care
//! about whether the sidecar is touched AT ALL, never about which
//! particular field it carries.
//!
//! A SEVENTH REWRITE (2026-09-17, `serve::usefulness`'s own "evaluation
//! debt" section) drops the UTC CALENDAR DAY the fourth rewrite chose, in
//! favour of a rolling `usefulness::EVAL_REPORT_COVERS_HOURS`-hour window
//! from `last_evaluation_seen` (`usefulness::eval_report_covers`, replacing
//! `eval_done_today`/`crate::time::same_utc_day`) - measured (acme-shop
//! eval 2): a report filed about 01:30 local time (23:30 UTC) left `doctor`
//! saying "today's evaluation is not done" 39 minutes later, at 00:09 UTC,
//! naming that very report as the newest thing in the store. `recently_
//! covered`/`no_longer_covered` replace the retired `start_of_today_utc`;
//! `seed_session_work_with_risk`'s own doc comment covers the accrual-side
//! half of this same rewrite.
//!
//! EVERY PAYLOAD BELOW NAMES AN EXPLICIT `cwd`, unlike this file's own
//! earlier shape - the sidecar is keyed by the EXACT project a Stop
//! resolves to (`project::resolve_project`), not by `project::applies_to`'s
//! looser "or global" rule the rest of the debt machinery uses, so a test
//! that seeds the sidecar for `None` (global) must make sure the subprocess
//! actually resolves to `None` too, rather than inheriting whatever
//! directory `cargo test` itself happened to be invoked from (which sits
//! inside this very repository, and so resolves to a REAL project).
//! `Sandbox::global_cwd`/`Sandbox::project_dir` give every test a `cwd` with
//! a known, deliberate resolution instead of an ambient one.
//!
//! Every test below runs the hook with `USERPROFILE`/`HOME` pointed at a
//! throwaway sandbox directory (mirrors `ops/tests/install_eval_command.rs`'s
//! own sandbox), so the message this debt prints - which names a real file
//! path when one exists - never depends on, and never reads, anything under
//! the real user's own `~/.claude`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use model::item::{Binding, Item, Kind, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

/// A comfortably-sized backlog for the message's own "N item(s) currently
/// owe a verdict here" context line - no longer tied to any threshold the
/// evaluation debt itself gates on (see this file's own module doc comment
/// for why the ceiling is gone).
const OWED_CONTEXT_COUNT: usize = 12;
const AFTER: usize = serve::usefulness::JUDGEMENT_DEBT_AFTER;

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
    }

    fn eval_command_path(&self) -> PathBuf {
        self.home.path().join(".claude").join("commands").join("thor-eval.md")
    }

    /// Write a (fake, non-empty) eval command file at the exact path
    /// `usefulness::default_eval_command_path` would resolve to under this
    /// sandbox's own HOME - so the debt's message can name a file that is
    /// really there.
    fn seed_eval_command(&self) {
        let path = self.eval_command_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "fixture eval routine").unwrap();
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path());
        cmd.env("USERPROFILE", self.home.path());
    }

    /// A real, empty directory with no marker and no `.git` anywhere above
    /// it (it lives under the OS temp directory, never inside this
    /// repository), so `project::resolve_project` reliably resolves it to
    /// `None` - the deliberate stand-in for "global", used instead of
    /// simply omitting `cwd` from a payload, which would instead inherit
    /// wherever `cargo test` itself was invoked from (inside this very
    /// repository, and so a REAL project). This is also the fixture for "no
    /// project at all", which is a checkout the evaluation debt must never
    /// speak in - see this file's own module doc comment.
    fn global_cwd(&self) -> PathBuf {
        let dir = self.home.path().join("no-project-cwd");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real, empty directory carrying a `.thor-project` marker naming
    /// `project` - so a payload's own `cwd` resolves to a REAL, named
    /// project (`project::resolve_project`). Needed by every test that
    /// expects the evaluation debt to actually fire: `model::gate`'s
    /// ground 21 refuses a Report with no project at all except one exempt
    /// id, so "global" is not an option for those, and the debt itself
    /// never speaks for a checkout with no project regardless of whether a
    /// report is ever filed.
    fn project_dir(&self, project: &str) -> PathBuf {
        let dir = self.home.path().join("checkouts").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
        dir
    }
}

fn run_hook(db: &Path, payload: &str, sandbox: &Sandbox) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    sandbox.apply(&mut cmd);
    let mut child = cmd.spawn().expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

/// A Stop payload with an EMPTY last assistant message (the Response Guard
/// has nothing to say), `stop_hook_active: false` (the FIRST Stop of a new
/// turn), and an explicit `cwd` - see this file's own module doc comment
/// for why every payload here names one deliberately.
fn stop_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

/// The identical shape as `stop_payload`, except `stop_hook_active: true` -
/// Claude Code's own signal that a Stop hook already held this turn once
/// and this is a retry within the SAME turn. Needed by `does_not_block_
/// twice_in_the_same_turn` below: this debt relies entirely on `hook_once`'s
/// own top-of-`Stop`-arm handling of this flag (`bin/serve.rs`'s
/// `already_fired` branch) for its "at most once per turn" safety, adding
/// no second copy of the mechanism itself, so this is the one payload shape
/// that actually exercises it.
fn retry_stop_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": true,
        "last_assistant_message": "",
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

/// The same shape as `stop_payload`, plus `agent_id` - Claude Code's own
/// documented signal (per `payload_is_from_a_subagent`'s doc comment in
/// `serve.rs`) that a Stop payload arrived from inside a Task-tool subagent
/// rather than the owner's own main session.
fn subagent_stop_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
        "cwd": cwd.to_string_lossy(),
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
    })
    .to_string()
}

/// A `PreToolUse` payload from inside a subagent - the identical `agent_id`
/// signal as `subagent_stop_payload` above, on the event type the fifth
/// rewrite's own accrual (`usefulness::record_hook_event`) runs on
/// unconditionally, subagent or not (see that function's own doc comment).
fn subagent_pretooluse_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
        "tool_name": "Read",
        "tool_input": { "file_path": "fixture.txt" },
    })
    .to_string()
}

/// Declares `n` never-judged rules, scoped to `project` (`None` for
/// global), and serves each one `AFTER` times under a throwaway fixture
/// session id, so all `n` sit in this checkout's own judgement debt - feeds
/// the evaluation debt's own message, "N item(s) currently owe a verdict
/// here", never a condition for it to speak any more. Returns the ids.
///
/// TARGET-BOUND, EACH ON ITS OWN UNIQUE COMMAND ANCHOR - never `Binding::
/// Always`, since `judgement_debt_counts` (and so this debt's own owed
/// count) excludes pinned items since 2026-09-12: an all-pinned fixture here
/// would silently stop testing the evaluation debt at all. A `Command`
/// target, not `Path`: ground 19 (`model::gate`) refuses a GLOBAL item
/// anchored at a source file, and a unique value per item (rather than one
/// shared anchor) keeps every one of them clear of `model::item::MAX_ITEMS`
/// crowding regardless of how large `n` is.
///
/// NEVER SERVED UNDER A REAL SESSION ID HERE - only under the throwaway
/// "fixture" one, which feeds the OWED COUNT (`judgement_debt_counts`,
/// unscoped by session) but is never anyone's `served_ids_in_session`. A
/// test that also needs THIS session to have accrued work in the project
/// uses `seed_session_work` below instead of serving one of these owed
/// items directly - serving an OWED item under the real session would also
/// satisfy `judgement_debt`'s own per-item "seen" filter and make it a
/// second, unwanted contender for most of the tests below.
fn declare_owed_items(store: &mut EventStore, n: usize, project: Option<&str>) -> Vec<String> {
    let label = project.unwrap_or("global");
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let id = format!("owed-{label}-{i:02}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture evaluation debt item number {i} for {label}"),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-eval-debt-command-{label}-{i:02}") }],
            severity: None,
            project: project.map(str::to_string),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("fixture evaluation debt item number {i} turns out to be wrong")),
            check: None,
        };
        model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
        for _ in 0..AFTER {
            serve::deliver::record_delivery(store, "fixture", "fixture", "t", "2026-09-08T00:00:00Z", &[id.clone()]);
        }
        ids.push(id);
    }
    ids
}

/// Seed this (project, session) pair's own accrued-work sidecar entry
/// directly, with `minutes` worth of work already accrued - the fifth
/// rewrite's own fixture lever (2026-09-17, `serve::usefulness`'s own
/// "evaluation debt" section), replacing the retired `serve_marker_in_
/// session`/`serve_marker_in_session_minutes_ago` (which drove the OLD
/// mechanism, a wall-clock timestamp on an `item_served` event) now that
/// "how long has this session worked here" comes from hook-event accrual
/// (`serve::usefulness::SessionWorkState`/`record_hook_event`) instead.
///
/// THE ANCHOR IS BUILT FROM THE LITERAL "now" this process reads right now,
/// `minutes` in the past - safe since 2026-09-17 (seventh rewrite,
/// `serve::usefulness`'s own "evaluation debt" section) in a way it was not
/// before: `accrue_session_work` (unlike a plain `last_evaluation_seen`
/// comparison) re-derives its own answer from a FRESH wall-clock read
/// inside the real hook subprocess, so a seed built from literal "now"
/// minus `minutes` used to be able to land on a DIFFERENT UTC calendar day
/// than the subprocess's own "now" whenever a test happened to run within
/// `minutes` of real UTC midnight - measured directly: this test suite
/// failed exactly this way when run at 00:25 UTC, seeding "90 minutes ago"
/// one calendar day before the subprocess's own clock read moments later,
/// which `session_work_reset_needed` used to read as a new-day reset,
/// silently zeroing the very accrual the test meant to prove. That rule is
/// gone: `session_work_reset_needed` no longer reads a calendar day at all,
/// only whether there is an anchor yet and whether a newer report was seen,
/// so there is no boundary left here to dodge and the elaborate "pin at
/// noon UTC" workaround this comment used to describe is retired along with
/// it.
///
/// A seed `last_event_unix` that lands slightly in the FUTURE relative to
/// the real subprocess's own clock (a few milliseconds of test overhead) is
/// harmless: `accrue_session_work` only ever ADDS a gap when it is positive
/// and under `EVAL_PAUSE_MINUTES`, so a negative gap just leaves the seeded
/// total untouched, which is exactly what a fixture asking for an EXACT
/// accrued total needs anyway.
///
/// Reads the sidecar first and merges in, rather than overwriting the whole
/// file, so a fixture that already seeded a report (`seed_eval_state`) or
/// another session's own accrual for the same project is never clobbered.
fn seed_session_work(db: &Path, project: Option<&str>, session_id: &str, minutes: i64) {
    seed_session_work_with_risk(db, project, session_id, minutes, 0, false);
}

/// The identical fixture as `seed_session_work` above, plus the two risk
/// counters the sixth rewrite (2026-09-17) added to `SessionWorkState` - see
/// `serve::usefulness`'s own "the repeat's own risk" doc comment. Needed by
/// every test proving a REPEAT ask actually fires: accrued time alone is no
/// longer enough, so `seed_session_work`'s own all-default risk (0, false)
/// would silently stay silent for any of those.
fn seed_session_work_with_risk(db: &Path, project: Option<&str>, session_id: &str, minutes: i64, edits_since_test: u32, compacted_since_anchor: bool) {
    let mut all = serve::usefulness::read_eval_debt_state(db);
    let key = project.unwrap_or("").to_string();
    let mut entry = all.get(&key).cloned().unwrap_or_default();
    let now = serve::time::now_unix();
    entry.sessions.insert(
        session_id.to_string(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - minutes * 60),
            accrued_secs: minutes * 60,
            last_event_unix: Some(now),
            edits_since_test,
            compacted_since_anchor,
        },
    );
    all.insert(key, entry);
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
}

/// Seed the sidecar as though THOR started tracking this project
/// `hours_ago` hours before the real "now" - drives the sidecar-backed
/// `tracking_since` field (retired from the predicate itself, still
/// written - see `usefulness`'s own "evaluation debt" section, fourth
/// rewrite) through the exact same production write `bin/serve.rs`'s Stop
/// arm itself uses (`usefulness::update_eval_debt_state`). Kept only for
/// the "no project at all" tests below, which care about whether the
/// sidecar is touched AT ALL by a no-project Stop, never about which
/// particular field a pre-existing entry happens to carry.
fn seed_tracking_since(store: &EventStore, db: &Path, project: Option<&str>, hours_ago: i64) {
    let since = serve::time::now_unix() - hours_ago * 3600;
    serve::usefulness::update_eval_debt_state(store, db, project, since);
}

/// Write the evaluation-debt sidecar directly, the same JSON shape
/// `usefulness::update_eval_debt_state` itself writes - the only way to
/// seed `last_evaluation_seen` (or `asked_count`/`first_asked_since_
/// report`) at an exact, controllable instant, since the real hook binary
/// always reads the real wall clock, never a fixture instant.
fn seed_eval_state(db: &Path, project: Option<&str>, state: serve::usefulness::ProjectEvalState) {
    let mut all: serve::usefulness::EvalDebtState = Default::default();
    all.insert(project.unwrap_or("").to_string(), state);
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
}

/// An instant `usefulness::eval_report_covers` will always agree still
/// covers, relative to the real "now" - regardless of what wall-clock hour
/// the test suite happens to run at. Replaces the retired
/// `start_of_today_utc` (seventh rewrite, 2026-09-17: the UTC calendar day
/// it measured against is gone).
///
/// FOUR HOURS, NOT ONE - MEASURED WHY. Several tests in this file seed this
/// alongside `seed_session_work_with_risk(..., serve::usefulness::
/// EVAL_REPEAT_WORK_MINUTES + 1, ...)`, whose own anchor lands
/// `EVAL_REPEAT_WORK_MINUTES + 1` (181) minutes - about three hours - before
/// "now". `session_work_reset_needed` resets the accrual the instant a
/// report is seen AFTER that anchor (`last_evaluation_seen` strictly later
/// than `anchor_unix`), so an offset shorter than the accrual it is paired
/// with silently zeroes the very accrual the test means to prove: measured
/// directly, one hour here against a three-hour accrual reset it to zero
/// and turned a "fires" test into a false silence with no visible error
/// beyond the eventual assertion failure. Four hours clears the longest
/// accrual this file seeds with room to spare, while staying comfortably
/// inside the 16-hour window.
fn recently_covered() -> i64 {
    serve::time::now_unix() - 4 * 3600
}

/// The mirror of `recently_covered` above: an instant safely OUTSIDE
/// `usefulness::EVAL_REPORT_COVERS_HOURS`, so a fixture can prove the "does
/// not cover any more" branch regardless of wall-clock time.
fn no_longer_covered() -> i64 {
    serve::time::now_unix() - (serve::usefulness::EVAL_REPORT_COVERS_HOURS + 1) * 3600
}

/// A live evaluation-report Report, correctly tagged and scoped - the exact
/// shape `eval-command.example.md`'s own final step now writes with
/// `remember`. `model::gate`'s ground 21 refuses a Report with no project
/// at all (except one exempt id), so this always needs a real project.
fn declare_report(store: &mut EventStore, id: &str, project: &str) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Report,
        text: "fixture evaluation report".to_string(),
        bindings: vec![],
        severity: None,
        project: Some(project.to_string()),
        tags: vec!["evaluation-report".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
}

/// The exact shape `ops::install::working_contract` seeds the real setup
/// note as - written out by hand (`serve/tests` has no dependency on `ops`) -
/// copied from `setup_debt_stop_hook.rs`'s own `declare_setup_note` for the
/// regression test below, which needs setup_debt to still fire first.
fn declare_setup_note(store: &mut EventStore) {
    let item = Item {
        id: model::store::SETUP_NOTE_ID.to_string(),
        kind: Kind::Rule,
        text: "walk the owner through setup once, then retract this note".to_string(),
        bindings: vec![Binding::Always],
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

/// A rule naming an obvious literal (a `--flag`), declared by bypassing the
/// write gate directly - the same escape hatch `bin/serve.rs`'s own
/// `legacy_unanswered` test helper uses, needed because the real gate
/// refuses a NEW rule shaped this way with no check and no `no-literal` tag.
fn declare_teeth_eligible_item(store: &mut EventStore, id: &str) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("Never run the deploy script with --force-{id} on this repo"),
        bindings: vec![Binding::Always],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("this turns out to be safe after all".to_string()),
        check: None,
    };
    let body = serde_json::to_string(&item).unwrap();
    store.append_event("legacy", id, "migration", EventKind::FactCreated, id, None, &body).unwrap();
}

// ------------------------------------------------------------- fires
//
// Every test below is scoped to a REAL project (`Sandbox::project_dir`),
// never `Sandbox::global_cwd`: a checkout with no project can never file
// the report that would silence this debt, so it must never be asked in
// the first place - see the "silences: no project at all" section below.

#[test]
fn fires_for_a_main_session_and_names_the_real_eval_path_when_it_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    sandbox.seed_eval_command();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("This project has had no evaluation in the last 16 hours"), "{reason}");
    assert!(reason.contains("This session has worked here for"), "{reason}");
    assert!(reason.contains(&format!("{OWED_CONTEXT_COUNT} item(s) currently owe a verdict here")), "{reason}");
    assert!(reason.contains("It has been asked 1 time(s)"), "{reason}");
    let expected_path = sandbox.eval_command_path();
    assert!(reason.contains(&expected_path.display().to_string()), "must name the real eval file path: {reason}");
    assert!(reason.contains("/thor-eval"), "must tell the owner how to run it: {reason}");
    assert!(reason.contains("A turn cannot end until the evaluation report for this project is filed"), "{reason}");
    assert!(reason.contains("After that it is quiet for 16 hours"), "{reason}");
}

#[test]
fn falls_back_to_the_generic_note_when_no_eval_file_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    // Deliberately no `seed_eval_command()` call: the sandbox HOME exists,
    // but nothing lives at `.claude/commands/thor-eval.md` under it.
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(
        !reason.contains(&sandbox.home.path().display().to_string()),
        "must never claim a path that does not exist: {reason}"
    );
    assert!(reason.contains("no evaluation routine is installed"), "{reason}");
    assert!(reason.contains("install"), "must say install writes one: {reason}");
    assert!(reason.contains("/thor-eval"), "{reason}");
}

/// Case named in the build brief: a store where BOTH the evaluation and the
/// judgement debt are due shows the evaluation first - since an evaluation
/// settles the judgement debt anyway. Every owed item is served under the
/// real session here, on purpose - unlike every other test in this file -
/// so the judgement debt's own `seen` filter (`bin/serve.rs`'s `judgement_
/// debt`) would genuinely also fire were it ever reached.
#[test]
fn both_debts_due_shows_the_evaluation_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    let ids = declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    for id in &ids {
        serve::deliver::record_delivery(&mut store, "s1", "fixture", "t", "2026-09-08T00:00:00Z", &[id.clone()]);
    }
    drop(store);
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("Run the THOR evaluation"), "the evaluation debt must speak first: {reason}");
    assert!(!reason.contains("Judge ALL"), "the judgement debt's own per-item text must not also appear: {reason}");
}

/// Case named in the build brief: "blocks on the Stop of a new turn again
/// and again while no report exists" - three separate, fresh turns (each
/// its own `stop_hook_active: false` payload) with no report ever filed
/// must ALL block, each naming one more ask than the last - the
/// once-per-session wall this debt used to hit is gone.
#[test]
fn blocks_on_the_stop_of_a_new_turn_again_and_again_while_no_report_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    for turn in 1..=3 {
        let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("turn {turn}: expected a decision JSON: {e}: {out}"));
        assert_eq!(v["decision"], "block", "turn {turn} must still block: {out}");
        let reason = v["reason"].as_str().unwrap();
        assert!(reason.contains(&format!("It has been asked {turn} time(s)")), "turn {turn}: {reason}");
    }
}

/// Case named in the build brief: "does not block twice in the same turn
/// (stop_hook_active)". The first Stop of a turn blocks; a RETRY of that
/// same turn (`stop_hook_active: true`, `retry_stop_payload`) must not -
/// this debt adds no loop-safety mechanism of its own, it relies entirely
/// on `hook_once`'s own top-of-`Stop`-arm handling of the flag, which never
/// even reaches this debt's own code on a retry carrying an empty message.
#[test]
fn does_not_block_twice_in_the_same_turn() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let first = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&first).expect("the first Stop of the turn must fire");
    assert_eq!(v["decision"], "block", "fixture sanity: {first}");

    let retry = run_hook(&db, &retry_stop_payload("s1", &project_dir), &sandbox);
    assert!(retry.trim().is_empty(), "a retry within the same turn must never block again: {retry}");
}

/// Case named in the build brief: "the ask counter increments per blocked
/// turn and resets when a report is seen" - proven end to end: three fresh
/// turns each name the next count on the sidecar itself, then a filed
/// report resets `asked_count`/`first_asked_since_report` back to
/// zero/`None` (doctor reads the identical fields - see `ops::health`'s own
/// `judgement_debt_line` tests for the doctor-facing half of this).
#[test]
fn the_ask_counter_increments_per_turn_and_resets_once_a_report_is_seen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    for turn in 1..=3 {
        run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
        let state = serve::usefulness::project_eval_state(&db, Some("thor-fixture"));
        assert_eq!(state.asked_count, turn, "turn {turn}: the ask count must match exactly");
        assert!(state.first_asked_since_report.is_some(), "turn {turn}: the first-asked clock must be set");
    }

    let mut store = EventStore::open_existing(&db).unwrap();
    declare_report(&mut store, "eval-thor-fixture-2026-09-17", "thor-fixture");
    seed_session_work(&db, Some("thor-fixture"), "s2", 90);
    drop(store);
    let after_report = run_hook(&db, &stop_payload("s2", &project_dir), &sandbox);
    assert!(after_report.trim().is_empty(), "a freshly filed report must silence this turn: {after_report}");

    let state = serve::usefulness::project_eval_state(&db, Some("thor-fixture"));
    assert_eq!(state.asked_count, 0, "a newly filed report must reset the ask counter");
    assert_eq!(state.first_asked_since_report, None, "and clear the first-asked clock");
}

// ------------------------------------------------------------- silences

#[test]
fn is_silent_for_a_subagent_payload_on_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &subagent_stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "a subagent's Stop must never be held for the evaluation debt: {out}");
}

/// Case named in the build brief: subagent `PreToolUse` events accrue work
/// for their own session (`usefulness::record_hook_event` runs
/// unconditionally, subagent or not - see that function's own doc comment),
/// but a subagent `Stop` still never blocks, however much that accrual has
/// grown - proven by reading the sidecar directly after the `PreToolUse`
/// call, then sending a `Stop` for the SAME session and confirming silence.
#[test]
fn subagent_pretooluse_events_accrue_work_but_a_subagent_stop_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_session_work(&db, Some("thor-fixture"), "sub1", 90);

    let out = run_hook(&db, &subagent_pretooluse_payload("sub1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "PreToolUse never blocks for this debt, subagent or not: {out}");
    let accrued_after_pretooluse = serve::usefulness::project_eval_state(&db, Some("thor-fixture"))
        .sessions
        .get("sub1")
        .expect("a subagent's own PreToolUse must still record its own session's accrual")
        .accrued_secs;
    assert!(
        accrued_after_pretooluse >= 90 * 60,
        "the seeded 90 minutes must still be there, not reset by being a subagent event: {accrued_after_pretooluse}"
    );

    let out = run_hook(&db, &subagent_stop_payload("sub1", &project_dir), &sandbox);
    assert!(
        out.trim().is_empty(),
        "a subagent's Stop must never be held for the evaluation debt, however much it has accrued: {out}"
    );
}

/// Case named in the build brief: zero items owed still fires once enough
/// time has been worked - the item count is message context now, never a
/// condition. Deliberately no `declare_owed_items` call at all.
#[test]
fn fires_with_zero_items_owed_once_worked_long_enough() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let store = EventStore::new(&db).unwrap();
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "a zero-item backlog must still owe an evaluation: {out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("0 item(s) currently owe a verdict here"), "{reason}");
}

/// Case named in the build brief: the session's first serving here was less
/// than an hour ago -> silent, however long this project has gone without a
/// report.
#[test]
fn is_silent_when_the_session_has_worked_here_less_than_an_hour() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 30);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "less than an hour worked here must never speak: {out}");
}

/// Case named in the build brief: "59 minutes silent" - one minute under
/// the floor, however long this project has gone without a report.
#[test]
fn is_silent_at_fifty_nine_minutes_worked() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 59);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "59 minutes must not yet be enough: {out}");
}

/// Case named in the build brief: a session with no accrued work yet is
/// silent - the gate against hijacking a session that has done nothing here
/// at all. Since the fifth rewrite (2026-09-17), this is also exactly what a
/// session's very FIRST hook event in a project looks like: `record_hook_
/// event` resets a never-before-seen (project, session) pair's accrual to
/// zero rather than crashing or guessing (`usefulness::accrue_session_
/// work`'s own "the very first event is a reset to zero" behaviour).
#[test]
fn is_silent_for_a_session_with_no_accrued_work_yet() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    // Deliberately no `seed_session_work` call: this is "s1"'s very first
    // hook event ever seen in this project.
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "a session with no accrued work yet must not be hijacked into the evaluation: {out}");
}

/// Case named in the build brief: a report that still covers silences it
/// for as long as it covers - proven across TWO separate fresh turns, not
/// just the one Stop that immediately follows the report.
#[test]
fn a_recently_seen_report_silences_it_for_as_long_as_it_covers() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    // `seed_eval_state` overwrites the WHOLE sidecar file (it has no
    // existing state to merge with here), so it must run BEFORE
    // `seed_session_work`, which reads-and-merges - the other order would
    // silently wipe the session accrual `seed_session_work` just wrote.
    seed_eval_state(
        &db,
        Some("thor-fixture"),
        serve::usefulness::ProjectEvalState { last_evaluation_seen: Some(recently_covered()), ..Default::default() },
    );
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let first = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(first.trim().is_empty(), "a report that still covers must silence the first turn: {first}");
    let second = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(second.trim().is_empty(), "and every turn after it, for as long as it keeps covering: {second}");
}

/// Case named in the build brief: a report seen outside the coverage window
/// does not silence the obligation.
#[test]
fn a_report_outside_the_coverage_window_does_not_silence_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    // `seed_eval_state` overwrites the WHOLE sidecar file, so it must run
    // BEFORE `seed_session_work` (which reads-and-merges) - see the
    // identical note on the test above.
    seed_eval_state(
        &db,
        Some("thor-fixture"),
        serve::usefulness::ProjectEvalState { last_evaluation_seen: Some(no_longer_covered()), ..Default::default() },
    );
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("a report outside the coverage window must not buy silence: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
}

/// Case named in the build brief: "the next day it asks again only after 60
/// minutes of work" - the day after a report (seeded as yesterday here), 59
/// minutes worked is still silent, 61 fires. Two stores, since each needs
/// its own fresh sidecar and its own session.
#[test]
fn the_day_after_a_report_59_minutes_worked_is_silent_61_fires() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    // `seed_eval_state` overwrites the WHOLE sidecar file, so it must run
    // BEFORE `seed_session_work` (which reads-and-merges) - see the
    // identical note on the earlier tests in this file.
    seed_eval_state(
        &db,
        Some("thor-fixture"),
        serve::usefulness::ProjectEvalState { last_evaluation_seen: Some(no_longer_covered()), ..Default::default() },
    );
    seed_session_work(&db, Some("thor-fixture"), "s1", 59);
    let silent = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(silent.trim().is_empty(), "59 minutes, the day after a report, must still be silent: {silent}");

    let dir2 = tempfile::tempdir().unwrap();
    let db2 = dir2.path().join("thor.db");
    let mut store2 = EventStore::new(&db2).unwrap();
    declare_owed_items(&mut store2, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store2);
    seed_eval_state(
        &db2,
        Some("thor-fixture"),
        serve::usefulness::ProjectEvalState { last_evaluation_seen: Some(no_longer_covered()), ..Default::default() },
    );
    seed_session_work(&db2, Some("thor-fixture"), "s2", 61);
    let out = run_hook(&db2, &stop_payload("s2", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("61 minutes must fire: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
}

// ------------------------------------------------------------ repeat: fires
//
// Added 2026-09-17 (fifth rewrite, `serve::usefulness`'s own "evaluation
// debt" section): once a report already exists for today, `EVAL_FIRST_WORK_
// MINUTES` no longer applies at all - only `EVAL_REPEAT_WORK_MINUTES`,
// measured from whenever that report reset the accrual, does.

/// A report seen today, with accrued work since it well past `EVAL_FIRST_
/// WORK_MINUTES` but nowhere near `EVAL_REPEAT_WORK_MINUTES`, must stay
/// silent - the first threshold simply does not apply any more once today's
/// report already exists.
#[test]
fn a_report_seen_today_with_90_minutes_accrued_since_stays_silent() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_eval_state(
        &db,
        Some("thor-fixture"),
        serve::usefulness::ProjectEvalState { last_evaluation_seen: Some(recently_covered()), ..Default::default() },
    );
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "well past the first threshold but nowhere near the repeat one must stay silent: {out}");
}

/// Case named in the build brief: a session with 181 minutes of accrued
/// work after a report seen today is blocked with the repeat message - the
/// end-to-end proof of the owner's own decision ("after every THREE HOURS OF
/// WORK... a NEW evaluation is due").
#[test]
fn a_report_seen_today_with_three_hours_accrued_since_fires_the_repeat_message() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_eval_state(
        &db,
        Some("thor-fixture"),
        serve::usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            last_evaluation_report_id: Some("eval-thor-fixture-2026-09-17".to_string()),
            ..Default::default()
        },
    );
    seed_session_work_with_risk(
        &db,
        Some("thor-fixture"),
        "s1",
        serve::usefulness::EVAL_REPEAT_WORK_MINUTES + 1,
        serve::usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS,
        false,
    );

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("three hours plus a risk since the last report must fire a repeat ask: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.starts_with("[THOR]"), "{reason}");
    assert!(reason.contains("This session has worked here for 3.0 hour(s) since the last evaluation report"), "{reason}");
    assert!(reason.contains("eval-thor-fixture-2026-09-17"), "must name the report this accrual is measured since: {reason}");
    assert!(
        reason.contains(&format!("{} code change(s) since the last test or build run", serve::usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS)),
        "must name the risk that triggered it: {reason}"
    );
    assert!(
        reason.contains("A new evaluation is due, covering only what happened since then plus the state of the work"),
        "{reason}"
    );
    assert!(reason.contains(&format!("{OWED_CONTEXT_COUNT} item(s) currently owe a verdict here")), "{reason}");
    assert!(reason.contains("A turn cannot end until the evaluation report for this project is filed"), "{reason}");
    assert!(!reason.contains("It has been asked"), "the repeat message names hours and a report id, never an ask count: {reason}");
}

/// THE CENTRAL CASE THE SIXTH REWRITE EXISTS FOR (owner's decision,
/// 2026-09-17): the identical fixture as the test just above, MINUS any
/// risk, must now stay silent - end to end, through the real hook binary.
#[test]
fn three_hours_accrued_with_no_risk_at_all_stays_silent_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_eval_state(
        &db,
        Some("thor-fixture"),
        serve::usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            last_evaluation_report_id: Some("eval-thor-fixture-2026-09-17".to_string()),
            ..Default::default()
        },
    );
    seed_session_work(&db, Some("thor-fixture"), "s1", serve::usefulness::EVAL_REPEAT_WORK_MINUTES + 1);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "three hours accrued with no code change and no summary must stay silent: {out}");
}

/// Case named in the build brief: subagent edits count toward the risk
/// counter, but a subagent Stop never blocks - proven with the SAME
/// three-hours-plus-a-report fixture the "fires" test above uses, so a
/// version of the code that forgot the risk gate entirely would be caught
/// here too. `agent_id` marks every payload below as coming from a
/// Task-tool subagent, the identical signal `subagent_pretooluse_payload`/
/// `subagent_stop_payload` already use.
#[test]
fn subagent_untested_edits_count_toward_the_risk_counter_but_a_subagent_stop_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_eval_state(
        &db,
        Some("thor-fixture"),
        serve::usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            last_evaluation_report_id: Some("eval-thor-fixture-2026-09-17".to_string()),
            ..Default::default()
        },
    );
    seed_session_work(&db, Some("thor-fixture"), "sub1", serve::usefulness::EVAL_REPEAT_WORK_MINUTES + 1);

    for _ in 0..serve::usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sub1",
            "cwd": project_dir.to_string_lossy(),
            "agent_id": "a1dca2c0feb7f44fb",
            "agent_type": "general-purpose",
            "tool_name": "Edit",
            "tool_input": { "file_path": "fixture.rs" },
        })
        .to_string();
        let out = run_hook(&db, &payload, &sandbox);
        assert!(out.trim().is_empty(), "PreToolUse never blocks for this debt, subagent or not: {out}");
    }

    let edits = serve::usefulness::project_eval_state(&db, Some("thor-fixture"))
        .sessions
        .get("sub1")
        .expect("a subagent's own PreToolUse must still record its own session's accrual")
        .edits_since_test;
    assert_eq!(
        edits, serve::usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS,
        "a subagent's own untested edits must still count toward the risk counter"
    );

    let out = run_hook(&db, &subagent_stop_payload("sub1", &project_dir), &sandbox);
    assert!(
        out.trim().is_empty(),
        "a subagent's Stop must never be held for the evaluation debt, however large its risk counter has grown: {out}"
    );
}

/// Case named in the build brief: a new evaluation-report Report for the
/// project silences it - the whole point of this debt: filing the report is
/// what tells THOR the evaluation happened. Uses the REAL production
/// detection path (a report declared in the store, read back through a real
/// Stop) rather than a hand-seeded sidecar.
#[test]
fn a_new_evaluation_report_for_the_project_silences_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let first = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&first).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {first}"));
    assert_eq!(v["decision"], "block", "fixture sanity, before the report is filed: {first}");

    // File the evaluation report through the store directly - the same
    // shape `eval-command.example.md`'s own final step files with
    // `remember`: kind Report, this project, tagged `evaluation-report`.
    let mut store = EventStore::open_existing(&db).unwrap();
    declare_report(&mut store, "eval-thor-fixture-2026-09-17", "thor-fixture");
    // A fresh session, so a stale minutes-worked timestamp is not what is
    // silencing the second call - it must clear the "served this project"
    // gate on its own too.
    seed_session_work(&db, Some("thor-fixture"), "s2", 90);
    drop(store);

    let second = run_hook(&db, &stop_payload("s2", &project_dir), &sandbox);
    assert!(second.trim().is_empty(), "a newly filed evaluation report must silence the obligation: {second}");
}

/// THE DEFECT THE FIRST 2026-09-16 REWRITE EXISTED FOR: a clock fed by ANY
/// verdict that applied to a checkout - global items included - reset
/// itself from unrelated activity elsewhere. `last_evaluation_seen`
/// structurally cannot regress the same way: `update_eval_debt_state` only
/// ever stamps it from a live Report tagged `evaluation-report`, never from
/// a plain `mark`. Marking a handful of totally unrelated global facts must
/// not silence this project's own obligation.
#[test]
fn regression_verdicts_on_unrelated_global_items_never_silence_this() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    // Verdicts on OTHER global items - none of these apply to the backlog
    // under test, and none carry the `evaluation-report` tag; they exist
    // only to prove a plain `mark` never masquerades as a filed report.
    for i in 0..6 {
        let id = format!("unrelated-global-{i}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("an unrelated global fixture fact number {i}"),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this stops being true".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "fixture", "fixture", "fixture", &item).unwrap();
        serve::mark::record_useful(&mut store, "s", "s", "t", "2026-09-08T00:00:00Z", &id).unwrap();
    }
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("unrelated global verdicts must never silence this: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("Run the THOR evaluation"), "{reason}");
}

// --------------------------------------------- silences: no project at all
//
// A checkout that resolves to NO project at all can never file the Report
// that silences this debt (`model::gate`'s ground 21, `NO_SCOPE_PROBLEM`),
// so it must never be asked, and the sidecar update must never even run,
// regardless of how long the session has worked or what a pre-existing
// sidecar entry happens to carry.

/// Case named in the build brief: "no project silent" - proven even with a
/// pre-existing sidecar entry (seeded through the real, unchanged
/// `update_eval_debt_state` write) and 61 minutes of serving in that
/// session: stays silent, and leaves the sidecar file byte-for-byte exactly
/// as `seed_tracking_since` left it - not reset, not bumped, not touched at
/// all. This is also the shape a sidecar written before the third rewrite
/// (2026-09-16) could still carry on a real machine - proving that legacy
/// entry is inert, never acted on again.
#[test]
fn is_silent_for_a_checkout_with_no_project_even_with_a_pre_existing_sidecar_entry_and_enough_time_worked() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, None);
    // Deliberately no `seed_session_work` call: a no-project checkout can
    // never accrue work that matters here either - `record_hook_event`
    // itself refuses to touch the sidecar at all for `project: None` (see
    // its own doc comment), so there is nothing honest a "session has
    // worked here" fixture could even represent for this case.
    seed_tracking_since(&store, &db, None, 25);
    drop(store);

    let sidecar = serve::usefulness::eval_debt_state_path(&db);
    let before = std::fs::read_to_string(&sidecar).expect("fixture sanity: seed_tracking_since must have written the sidecar");

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    assert!(out.trim().is_empty(), "a checkout with no project must never be asked for the evaluation: {out}");

    let after = std::fs::read_to_string(&sidecar).unwrap();
    assert_eq!(before, after, "a no-project Stop must never touch the sidecar, not even a pre-existing entry already there");
}

/// A companion to the test above, proving the gate structurally rather than
/// only by outcome. The test above seeds the sidecar before the Stop runs,
/// so - even if the `stop_project.is_some()` half of the gate were missing
/// entirely - `usefulness::update_eval_debt_state` would still be a no-op
/// the second time (its own `get_or_insert` never overwrites an existing
/// value, and no new report exists to stamp), so a byte-for-byte comparison
/// alone cannot tell "the write was skipped" apart from "the write ran and
/// happened to change nothing". This test closes that gap: with NO sidecar
/// seeded at all - the very first Stop this checkout would ever see - a
/// version of the code missing the gate would create the file right here.
/// This is the one assertion in this file that actually goes red if the
/// gate in `bin/serve.rs`'s `hook_once` (`if !is_subagent && stop_project.
/// is_some()`) is weakened back to `if !is_subagent`.
#[test]
fn a_no_project_stop_never_creates_the_sidecar_file_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, None);
    // Deliberately no `seed_tracking_since` or `seed_session_work` call:
    // nothing has written the sidecar yet, the same as the very first Stop
    // this checkout would ever see - and a no-project checkout could never
    // honestly seed accrued work anyway (`record_hook_event` itself refuses
    // to touch the sidecar at all for `project: None`).
    drop(store);

    let sidecar = serve::usefulness::eval_debt_state_path(&db);
    assert!(!sidecar.exists(), "fixture sanity: nothing has written the sidecar yet");

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    assert!(out.trim().is_empty(), "a checkout with no project must never be asked for the evaluation: {out}");
    assert!(
        !sidecar.exists(),
        "a no-project Stop must never create the sidecar file at all - the write is skipped entirely, not merely a no-op"
    );
}

/// THE CONTRAST both tests above need: a checkout that resolves to a real
/// project, 61 minutes worked, no report ever seen - and this one must
/// still fire, proving the silence above is really about the missing
/// project and nothing else accidentally different between the fixtures.
#[test]
fn the_identical_setup_with_a_real_project_still_fires() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 61);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("the identical setup, scoped to a real project, must still fire: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("Run the THOR evaluation"), "{reason}");
}

// --------------------------------------------------- regression: the others

/// THE DEFECT THIS GUARDS AGAINST: this project has measured twice what one
/// shared early return across several Stop-time gates does to the ones after
/// it (see `serve/src/bin/serve.rs`'s own `is_subagent` doc comment). Adding
/// a sixth gate between `judgement_debt` and the backlog burn must not
/// swallow an EARLIER debt - `setup_debt` runs first and must still win
/// outright, even on a store that ALSO satisfies the evaluation debt's own
/// condition.
#[test]
fn setup_debt_still_fires_with_an_eval_eligible_backlog_also_present() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("First session with a new owner"), "setup_debt must still win first: {reason}");
    assert!(!reason.contains("Run the THOR evaluation"), "the evaluation debt must not also speak this turn: {reason}");
}

/// THE OTHER HALF: a debt AFTER the new insertion point (the backlog burn,
/// `teeth_debt`) must still fire on its own store, unaffected, when the
/// evaluation debt itself has nothing to say (no sidecar was ever seeded
/// here, and the session served nothing applying to this project either).
/// Left on `global_cwd` deliberately: this test was never about the
/// evaluation debt firing or staying silent in the first place (nothing
/// here would make it fire either way), only about the debt after it still
/// running.
#[test]
fn teeth_debt_still_fires_after_the_evaluation_debt_check() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_teeth_eligible_item(&mut store, "names-a-flag");
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("names-a-flag"), "teeth_debt must still name the rule: {reason}");
    assert!(reason.contains("--force-names-a-flag"), "and quote what it spotted: {reason}");
}
