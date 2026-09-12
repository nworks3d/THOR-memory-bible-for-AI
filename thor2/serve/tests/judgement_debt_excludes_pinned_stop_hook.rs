//! THE JUDGEMENT DEBT'S PINNED EXCLUSION (`serve::usefulness::is_pinned`,
//! shared by `owed_items` and `bin/serve.rs`'s own `judgement_debt`), end to
//! end through the real compiled `serve hook` binary - mirrors
//! `evaluation_debt_stop_hook.rs`'s own shape for the same reason: the pure
//! decision logic already has its own unit tests
//! (`usefulness.rs`'s pinned-exclusion tests, `bin/serve.rs`'s own
//! `judgement_debt_tests`); this file proves the WIRING - that a real Stop
//! payload against a real store never asks for a verdict on an Always-bound
//! item, and never counts one toward the evaluation debt's own obligation
//! either, the exact defect measured 2026-09-12: doctor's named judgement-
//! debt list carried more than thirty Always-bound items, two of them
//! pinned by the owner on purpose, and a whole evaluation was spent judging
//! every one of them for nothing.
//!
//! Every test below runs the hook with `USERPROFILE`/`HOME` pointed at a
//! throwaway sandbox directory, the same reason `evaluation_debt_stop_hook.rs`
//! does: nothing here should depend on, or touch, the real user's own
//! `~/.claude`.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use model::item::{Binding, Item, Kind, TargetKind};
use thor_core::event_store::EventStore;

const AFTER: usize = serve::usefulness::JUDGEMENT_DEBT_AFTER;
const CEILING: usize = serve::usefulness::EVAL_DEBT_CEILING;

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
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
/// the Response Guard has nothing to say, and every fixture below declares
/// only GLOBAL items, so the debt under test is proven on its own.
fn stop_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string()
}

/// `n` Always-bound (pinned) GLOBAL rules, each served `AFTER` times under
/// `session_id` - enough on their own to have crossed both the judgement-
/// debt threshold and, if pinned items still counted, the evaluation debt's
/// own ceiling too, so a silent hook proves the exclusion rather than merely
/// a backlog too small to speak.
fn declare_pinned_items(store: &mut EventStore, n: usize, session_id: &str) {
    for i in 0..n {
        let id = format!("pinned-{i:02}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture pinned item number {i}"),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("fixture pinned item number {i} turns out to be wrong")),
            check: None,
        };
        model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
        for _ in 0..AFTER {
            serve::deliver::record_delivery(store, session_id, session_id, "t", "2026-09-08T00:00:00Z", &[id.clone()]);
        }
    }
}

/// `n` trigger-bound rules, each on its own unique `Command` anchor (never
/// crowded, whatever `n` is - see `evaluation_debt_stop_hook.rs`'s own
/// `declare_owed_items` for the identical reasoning), served the same way -
/// the control group that must still be named when pinned items are mixed
/// in.
fn declare_trigger_items(store: &mut EventStore, n: usize, session_id: &str) {
    for i in 0..n {
        let id = format!("trigger-{i:02}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture trigger-bound item number {i}"),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-trigger-command-{i:02}") }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("fixture trigger-bound item number {i} turns out to be wrong")),
            check: None,
        };
        model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
        for _ in 0..AFTER {
            serve::deliver::record_delivery(store, session_id, session_id, "t", "2026-09-08T00:00:00Z", &[id.clone()]);
        }
    }
}

#[test]
fn a_store_of_only_pinned_items_produces_no_judgement_ask_and_no_evaluation_obligation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    // More than the evaluation debt's own ceiling, so a silent hook proves
    // the exclusion actually holds rather than the backlog simply being too
    // small to have spoken either way.
    declare_pinned_items(&mut store, CEILING + 5, "s1");
    drop(store);
    let sandbox = Sandbox::new();

    let out = run_hook(&db, &stop_payload("s1"), &sandbox);
    assert!(
        out.trim().is_empty(),
        "an all-pinned backlog must hold no judgement ask and no evaluation obligation: {out}"
    );
}

#[test]
fn a_mixed_store_names_only_the_trigger_bound_items() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    declare_pinned_items(&mut store, 3, "s1");
    declare_trigger_items(&mut store, 3, "s1");
    drop(store);
    let sandbox = Sandbox::new();

    let out = run_hook(&db, &stop_payload("s1"), &sandbox);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    for i in 0..3 {
        assert!(reason.contains(&format!("trigger-{i:02}")), "must name the trigger-bound item: {reason}");
        assert!(!reason.contains(&format!("pinned-{i:02}")), "must never name a pinned item: {reason}");
    }
}
