//! The CONTENT guard's Dir-anchored half (`serve::absent_guard::
//! find_dir_content_violation` / `stale_in_dir_content`), end to end through
//! the real compiled `serve hook` binary - mirrors `absent_check_guard.rs`'s
//! and `location_guard.rs`'s own shape for exactly the same reason: the pure
//! decision logic already has its own unit tests in `serve::absent_guard`'s
//! own test module; this file proves the WIRING - the real store, the real
//! marker file on disk, and the real hook JSON shape on stdout, for a real
//! Write PreToolUse payload whose target file sits inside a directory a
//! content rule protects.
//!
//! WHY THIS FILE EXISTS. A rule bound to a Dir target used to be silent
//! forever: `rank::select` drops it on a KIND mismatch before it compares a
//! single path, so the content guard's Path-only pool could never surface
//! one. Every content rule anchored to a directory was therefore dead, and
//! nothing told the author. These two tests were written against that gap
//! and failed with empty stdout before `absent_guard_block` was wired to
//! call `find_dir_content_violation` from the Path+Dir-narrowed pool it
//! already fetches for the location guard. They pass because that wiring
//! landed; if either ever goes silent again, the same class of rule has
//! gone dead again.

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
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

fn write_payload(session_id: &str, cwd: &Path, file_path: &Path, content: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": { "file_path": file_path.to_string_lossy(), "content": content },
    })
    .to_string()
}

/// A real store with one live Rule anchored to the DIRECTORY `thor2`
/// (relative to `dir`), carrying a still-current `Check::Absent` forbidding
/// the em dash character - the realistic case from the defect report this
/// change addresses (a rule forbidding a character in everything written
/// under a directory). `thor2` is created on disk right here so the
/// anchor's currency actually holds.
fn fixture_with_dir_anchored_em_dash_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::create_dir(dir.join("thor2")).unwrap();
    let item = model::item::Item {
        id: "no-em-dash-under-thor2".to_string(),
        kind: model::item::Kind::Rule,
        text: "nothing under thor2/ ever carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Dir,
            value: "thor2".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands under thor2/ and nobody notices".to_string()),
        check: Some(model::item::Check::Absent { path: "thor2".to_string(), literal: "\u{2014}".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END - THE DEFECT REPORT'S OWN REPRODUCTION: a
/// rule anchored to a directory forbidding the em dash character, and a real
/// `Write` tool call whose target file sits directly inside that directory,
/// is blocked - through the real compiled binary, not a unit-level call into
/// the guard's own pure functions.
#[test]
fn a_write_introducing_an_em_dash_under_a_protected_directory_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_dir_anchored_em_dash_rule(dir.path());
    let target = dir.path().join("thor2").join("probe-scratch.md");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &target, "a line with an em dash \u{2014} right here"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("no-em-dash-under-thor2"), "must name the rule id: {reason}");
    assert!(reason.contains('\u{2014}'), "the quoted fragment must carry the offending character itself: {reason}");
}

/// The same, at depth - mirrors the pure-function-level
/// `a_dir_anchored_content_rule_blocks_a_file_several_levels_deep` test in
/// `serve::absent_guard`'s own test module.
#[test]
fn a_write_several_levels_under_a_protected_directory_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_dir_anchored_em_dash_rule(dir.path());
    let target = dir.path().join("thor2").join("serve").join("src").join("probe-scratch.md");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &target, "a line with an em dash \u{2014} right here"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("no-em-dash-under-thor2"), "{out}");
}
