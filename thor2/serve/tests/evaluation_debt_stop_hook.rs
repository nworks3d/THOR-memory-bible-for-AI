//! THE EVALUATION DEBT (`serve::usefulness::eval_debt_owed` for the pure
//! predicate, `bin/serve.rs`'s own `evaluation_debt` for the Stop-hook
//! wiring), end to end through the real compiled `serve hook` binary -
//! mirrors `setup_debt_stop_hook.rs`'s own shape for the same reason: the
//! pure decision logic already has its own unit tests (`usefulness.rs`'s
//! `eval_debt_predicate_tests`, `bin/serve.rs`'s `evaluation_debt_tests`);
//! this file proves the WIRING - the real event store, the real hook JSON
//! shape on stdout, the once-per-session sidecar, and above all the one
//! property that cannot be proven at the unit level at all, because it lives
//! in `hook_once`'s payload dispatch rather than in `evaluation_debt` itself:
//! this debt must NEVER hold a subagent's own Stop.
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

/// A Stop payload with an EMPTY last assistant message and no `cwd` at all -
/// mirrors `setup_debt_stop_hook.rs`'s own `stop_payload`: the Response
/// Guard has nothing to say, and every fixture below declares only GLOBAL
/// items, so the debt under test is proven on its own, with no project
/// resolution and no other guard riding along.
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

/// Declares `n` never-judged global rules and serves each one `AFTER` times,
/// so all `n` sit in this checkout's own judgement debt - the evaluation
/// debt's first condition needs a real backlog, not a single item, and a
/// GLOBAL one so no `cwd`/project resolution is needed for it to apply here.
///
/// TARGET-BOUND, EACH ON ITS OWN UNIQUE COMMAND ANCHOR - never `Binding::
/// Always`, since `judgement_debt_counts` (and so this debt's own owed
/// count) excludes pinned items since 2026-09-12: an all-pinned fixture here
/// would silently stop testing the evaluation debt at all. A `Command`
/// target, not `Path`: ground 19 (`model::gate`) refuses a GLOBAL item
/// anchored at a source file, and a unique value per item (rather than one
/// shared anchor) keeps every one of them clear of `model::item::MAX_ITEMS`
/// crowding regardless of how large `n` is.
fn declare_owed_items(store: &mut EventStore, n: usize) {
    for i in 0..n {
        let id = format!("owed-{i:02}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture evaluation debt item number {i}"),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-eval-debt-command-{i:02}") }],
            severity: None,
            project: None,
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
    }
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
    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING);
    drop(store);

    let sandbox = Sandbox::new();
    sandbox.seed_eval_command();

    let out = run_hook(&db, &stop_payload("s1"), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains(&format!("{CEILING} item")), "must name the count: {reason}");
    assert!(reason.contains("ever been judged"), "{reason}");
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
    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING);
    drop(store);

    // Deliberately no `seed_eval_command()` call: the sandbox HOME exists,
    // but nothing lives at `.claude/commands/thor-eval.md` under it.
    let sandbox = Sandbox::new();

    let out = run_hook(&db, &stop_payload("s1"), &sandbox);
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

// ------------------------------------------------------------- silences

#[test]
fn is_silent_for_a_subagent_payload_on_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING);
    drop(store);
    let sandbox = Sandbox::new();

    let out = run_hook(&db, &subagent_stop_payload("s1"), &sandbox);
    assert!(out.trim().is_empty(), "a subagent's Stop must never be held for the evaluation debt: {out}");
}

#[test]
fn is_silent_on_the_second_stop_of_the_same_session() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING);
    drop(store);
    let sandbox = Sandbox::new();

    let first = run_hook(&db, &stop_payload("s1"), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&first).expect("the first Stop must fire");
    assert_eq!(v["decision"], "block", "fixture sanity: {first}");

    let second = run_hook(&db, &stop_payload("s1"), &sandbox);
    assert!(second.trim().is_empty(), "the same session must not be asked twice: {second}");
}

#[test]
fn is_silent_when_the_backlog_has_a_fresh_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    // One extra item beyond the ceiling: marking it useful "now" drops it
    // out of the owed set on its own, so the remaining count still sits
    // exactly at the ceiling - proving the fresh verdict, not a falling
    // count, is what silences this.
    declare_owed_items(&mut store, CEILING + 1);
    serve::mark::record_useful(&mut store, "fixture", "fixture", "fixture", &serve::time::now_iso8601(), "owed-00")
        .unwrap();
    assert_eq!(
        serve::usefulness::judgement_debt_counts(&store, None).1,
        CEILING,
        "fixture sanity: still exactly at the ceiling after one item is judged"
    );
    drop(store);
    let sandbox = Sandbox::new();

    let out = run_hook(&db, &stop_payload("s1"), &sandbox);
    assert!(out.trim().is_empty(), "a fresh verdict in this project's scope must silence the debt: {out}");
}

#[test]
fn is_silent_below_the_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, CEILING - 1);
    drop(store);
    let sandbox = Sandbox::new();

    let out = run_hook(&db, &stop_payload("s1"), &sandbox);
    assert!(out.trim().is_empty(), "one below the ceiling must never speak: {out}");
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
    let mut store = EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    declare_owed_items(&mut store, CEILING);
    drop(store);
    let sandbox = Sandbox::new();

    let out = run_hook(&db, &stop_payload("s1"), &sandbox);
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
    let mut store = EventStore::new(&db).unwrap();
    declare_teeth_eligible_item(&mut store, "names-a-flag");
    drop(store);
    let sandbox = Sandbox::new();

    let out = run_hook(&db, &stop_payload("s1"), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("names-a-flag"), "teeth_debt must still name the rule: {reason}");
    assert!(reason.contains("--force-names-a-flag"), "and quote what it spotted: {reason}");
}
