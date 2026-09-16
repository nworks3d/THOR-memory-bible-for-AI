//! THE EVALUATION DEBT (`serve::usefulness::eval_debt_owed` for the pure
//! predicate, `serve::usefulness::update_eval_debt_state` for the sidecar
//! write, `bin/serve.rs`'s own `evaluation_debt` for the Stop-hook wiring),
//! end to end through the real compiled `serve hook` binary - mirrors
//! `setup_debt_stop_hook.rs`'s own shape for the same reason: the pure
//! decision logic already has its own unit tests (`usefulness.rs`'s
//! `eval_debt_predicate_tests`/`eval_debt_state_tests`, `bin/serve.rs`'s
//! `evaluation_debt_tests`); this file proves the WIRING - the real event
//! store, the real sidecar file on disk, the real hook JSON shape on
//! stdout, the once-per-session sidecar, the "this session actually served
//! something here" gate, and above all the one property that cannot be
//! proven at the unit level at all, because it lives in `hook_once`'s
//! payload dispatch rather than in `evaluation_debt` itself: this debt must
//! NEVER hold a subagent's own Stop, and it must now speak BEFORE the
//! judgement debt when both are due.
//!
//! THE TRIGGER ITSELF WAS REWRITTEN 2026-09-16: a single global "newest
//! verdict" clock could never hold a per-project debt (a verdict on ANY
//! item that applied to a checkout reset it, global items included), so it
//! never once fired against a copy of the owner's own store across four
//! days measured. The replacement is a per-project sidecar
//! (`eval-debt-state.json`, `serve::usefulness::ProjectEvalState`) tracking
//! how long this project's own backlog has sat at or over the ceiling, and
//! whether an evaluation report has been seen for it since - see
//! `usefulness.rs`'s own "evaluation debt" section for the full story.
//! `seed_over_ceiling_since` below drives that clock directly (through the
//! exact same production write the Stop hook itself uses), since these
//! tests cannot make real wall-clock time pass.
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

const CEILING: usize = serve::usefulness::EVAL_DEBT_CEILING;
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
    /// repository, and so a REAL project).
    fn global_cwd(&self) -> PathBuf {
        let dir = self.home.path().join("no-project-cwd");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real, empty directory carrying a `.thor-project` marker naming
    /// `project` - so a payload's own `cwd` resolves to a REAL, named
    /// project (`project::resolve_project`). Needed by the tests that file
    /// a scoped evaluation Report: `model::gate`'s ground 21 refuses a
    /// Report with no project at all except one exempt id, so "global" is
    /// not an option for those.
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
/// has nothing to say) and an explicit `cwd` - see this file's own module
/// doc comment for why every payload here names one deliberately.
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

/// The same shape, plus `agent_id` - Claude Code's own documented signal
/// (per `payload_is_from_a_subagent`'s doc comment in `serve.rs`) that a
/// Stop payload arrived from inside a Task-tool subagent rather than the
/// owner's own main session.
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

/// Declares `n` never-judged rules, scoped to `project` (`None` for
/// global), and serves each one `AFTER` times under a throwaway fixture
/// session id, so all `n` sit in this checkout's own judgement debt - the
/// evaluation debt's first condition needs a real backlog, not a single
/// item. Returns the ids.
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
/// test that also needs THIS session to have served something in the
/// project uses `serve_marker_in_session` below instead of serving one of
/// these owed items directly - serving an OWED item under the real session
/// would also satisfy `judgement_debt`'s own per-item "seen" filter and
/// make it a second, unwanted contender for most of the tests below.
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

/// Declare a fresh, harmless item scoped to `project` and serve it EXACTLY
/// ONCE, under the real `session_id` - satisfies `session_served_in_project`
/// (`bin/serve.rs`) without ever approaching `JUDGEMENT_DEBT_AFTER`, so it
/// never becomes owed itself and never gives `judgement_debt` anything to
/// ask about. The id embeds `session_id` so two calls in the same test (a
/// first session, then a second) never collide or trip the write gate's
/// near-duplicate check.
fn serve_marker_in_session(store: &mut EventStore, session_id: &str, project: Option<&str>) -> String {
    let id = format!("marker-{session_id}");
    let item = Item {
        id: id.clone(),
        kind: Kind::Rule,
        text: format!("fixture marker item for session {session_id}"),
        bindings: vec![Binding::Always],
        severity: None,
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("this marker fixture turns out to be wrong".to_string()),
        check: None,
    };
    model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
    serve::deliver::record_delivery(store, session_id, "fixture", "t", "2026-09-08T00:00:00Z", &[id.clone()]);
    id
}

/// Seed the sidecar as though this project's own backlog first crossed the
/// ceiling `hours_ago` hours before the real "now" - the only way to drive
/// the sidecar-backed staleness clock from a test, since these tests cannot
/// make real wall-clock time pass. Calls the exact same production write
/// `bin/serve.rs`'s Stop arm itself uses
/// (`usefulness::update_eval_debt_state`), so the sidecar this writes is
/// byte-identical in shape to the real thing.
fn seed_over_ceiling_since(store: &EventStore, db: &Path, project: Option<&str>, hours_ago: i64) {
    let since = serve::time::now_unix() - hours_ago * 3600;
    serve::usefulness::update_eval_debt_state(store, db, project, since);
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

#[test]
fn fires_for_a_main_session_and_names_the_real_eval_path_when_it_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    sandbox.seed_eval_command();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING, None);
    serve_marker_in_session(&mut store, "s1", None);
    seed_over_ceiling_since(&store, &db, None, 25);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains(&format!("{CEILING} item")), "must name the count: {reason}");
    assert!(reason.contains("no evaluation report was filed"), "{reason}");
    assert!(reason.contains("once per session"), "{reason}");
    let expected_path = sandbox.eval_command_path();
    assert!(
        reason.contains(&expected_path.display().to_string()),
        "must name the real eval file path: {reason}"
    );
    assert!(reason.contains("/thor-eval"), "must tell the owner how to run it: {reason}");
}

#[test]
fn falls_back_to_the_generic_note_when_no_eval_file_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    // Deliberately no `seed_eval_command()` call: the sandbox HOME exists,
    // but nothing lives at `.claude/commands/thor-eval.md` under it.
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING, None);
    serve_marker_in_session(&mut store, "s1", None);
    seed_over_ceiling_since(&store, &db, None, 25);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
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
/// judgement debt are due shows the evaluation first - proving the
/// 2026-09-16 reordering (evaluation debt now asked before judgement debt
/// in `hook_once`'s `Stop` arm, since an evaluation settles the judgement
/// debt anyway). Every owed item is served under the real session here, on
/// purpose - unlike every other test in this file - so the judgement
/// debt's own `seen` filter (`bin/serve.rs`'s `judgement_debt`) would
/// genuinely also fire were it ever reached.
#[test]
fn both_debts_due_shows_the_evaluation_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    let ids = declare_owed_items(&mut store, CEILING, None);
    for id in &ids {
        serve::deliver::record_delivery(&mut store, "s1", "fixture", "t", "2026-09-08T00:00:00Z", &[id.clone()]);
    }
    seed_over_ceiling_since(&store, &db, None, 25);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("Run the THOR evaluation"), "the evaluation debt must speak first: {reason}");
    assert!(!reason.contains("Judge ALL"), "the judgement debt's own per-item text must not also appear: {reason}");
}

// ------------------------------------------------------------- silences

#[test]
fn is_silent_for_a_subagent_payload_on_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING, None);
    serve_marker_in_session(&mut store, "s1", None);
    seed_over_ceiling_since(&store, &db, None, 25);
    drop(store);

    let out = run_hook(&db, &subagent_stop_payload("s1", &cwd), &sandbox);
    assert!(out.trim().is_empty(), "a subagent's Stop must never be held for the evaluation debt: {out}");
}

#[test]
fn is_silent_on_the_second_stop_of_the_same_session() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING, None);
    serve_marker_in_session(&mut store, "s1", None);
    seed_over_ceiling_since(&store, &db, None, 25);
    drop(store);

    let first = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&first).expect("the first Stop must fire");
    assert_eq!(v["decision"], "block", "fixture sanity: {first}");

    let second = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    assert!(second.trim().is_empty(), "the same session must not be asked twice: {second}");
}

#[test]
fn is_silent_below_the_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING - 1, None);
    serve_marker_in_session(&mut store, "s1", None);
    seed_over_ceiling_since(&store, &db, None, 100);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    assert!(out.trim().is_empty(), "one below the ceiling must never speak, however stale: {out}");
}

/// Case named in the build brief: a session that served nothing in the
/// project is silent - the 2026-09-16 gate against hijacking a session that
/// did no THOR-relevant work here at all.
#[test]
fn is_silent_for_a_session_that_served_nothing_in_this_project() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING, None);
    // Deliberately no `serve_marker_in_session` call: "s1" never had
    // anything served to it at all in this store.
    seed_over_ceiling_since(&store, &db, None, 25);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    assert!(out.trim().is_empty(), "a session that served nothing here must not be hijacked into the evaluation: {out}");
}

/// Case named in the build brief: a new evaluation-report Report for the
/// project silences it - the whole point of the 2026-09-16 rewrite: filing
/// the report is what tells THOR the evaluation happened.
#[test]
fn a_new_evaluation_report_for_the_project_silences_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING, Some("thor-fixture"));
    serve_marker_in_session(&mut store, "s1", Some("thor-fixture"));
    seed_over_ceiling_since(&store, &db, Some("thor-fixture"), 25);
    drop(store);

    let first = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&first).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {first}"));
    assert_eq!(v["decision"], "block", "fixture sanity, before the report is filed: {first}");

    // File the evaluation report through the store directly - the same
    // shape `eval-command.example.md`'s own final step files with
    // `remember`: kind Report, this project, tagged `evaluation-report`.
    let mut store = EventStore::open_existing(&db).unwrap();
    declare_report(&mut store, "eval-thor-fixture-2026-09-16", "thor-fixture");
    // A fresh session, so the once-per-session sidecar for "s1" is not what
    // is silencing the second call - it must clear the "served this
    // project" gate on its own too.
    serve_marker_in_session(&mut store, "s2", Some("thor-fixture"));
    drop(store);

    let second = run_hook(&db, &stop_payload("s2", &project_dir), &sandbox);
    assert!(second.trim().is_empty(), "a newly filed evaluation report must silence the obligation: {second}");
}

/// THE REGRESSION THIS WHOLE REWRITE EXISTS FOR, measured on a copy of the
/// owner's own store 2026-09-12 to 2026-09-16: a clock fed by ANY verdict
/// that applied to a checkout - global items included - reset itself every
/// few hours from unrelated activity elsewhere, so "nothing judged for 24
/// hours" was never once true in four days even while this project's own
/// backlog sat over the ceiling 56% to 100% of the time. Marking a handful
/// of totally unrelated global facts every few hours, right up to the edge
/// of the window, must no longer stop the obligation from firing once this
/// project's own crossing has genuinely stayed stale for a day.
#[test]
fn regression_verdicts_on_unrelated_global_items_never_reset_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING, None);
    serve_marker_in_session(&mut store, "s1", None);
    seed_over_ceiling_since(&store, &db, None, 25);

    // Verdicts on OTHER global items, every few hours over the last day -
    // exactly the pattern that silenced the old clock forever. None of
    // these apply to the backlog under test; they exist only to feed the
    // OLD clock's own "newest verdict anywhere" reading, were it still
    // there to be fed.
    for (i, hours_ago) in [2, 6, 10, 14, 18, 22].into_iter().enumerate() {
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
        let stamp = serve::time::iso8601_from_unix(serve::time::now_unix() - hours_ago * 3600);
        serve::mark::record_useful(&mut store, "s", "s", "t", &stamp, &id).unwrap();
    }
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("recent verdicts on unrelated global items must never silence this: {e}: {out}"));
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
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    declare_owed_items(&mut store, CEILING, None);
    serve_marker_in_session(&mut store, "s1", None);
    seed_over_ceiling_since(&store, &db, None, 25);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("First session with a new owner"), "setup_debt must still win first: {reason}");
    assert!(!reason.contains("Run the THOR evaluation"), "the evaluation debt must not also speak this turn: {reason}");
}

/// THE OTHER HALF: a debt AFTER the new insertion point (the backlog burn,
/// `teeth_debt`) must still fire on its own store, unaffected, when the
/// evaluation debt itself has nothing to say (the backlog here never
/// reaches `CEILING`).
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
