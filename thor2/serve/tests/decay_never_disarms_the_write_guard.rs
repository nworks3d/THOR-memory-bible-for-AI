//! Whether two `mark noise` verdicts against a blocking rule, with no useful
//! mark recorded since, can be used to get a forbidden write through -
//! raised 2026-09-08 as a hypothesis from a comparison with another memory
//! server's own rule, "an agent cannot write its own success story". THOR's
//! `mark` tool is callable by the very agent a block just refused,
//! `serve::mark::record_noise` is that call's write half, and
//! `decay::is_stale` (see `serve/src/decay.rs`) retires an item from every
//! injection surface once it has been called noise
//! `decay::NOISE_MARKS_BEFORE_STALE` times since anyone last called it
//! useful. If the write guard (`serve::absent_guard`, wired in
//! `bin/serve.rs`'s `absent_guard_block`) ever took its own candidate pool
//! AFTER that retirement, two `mark noise` calls plus the write the rule
//! exists to stop would be all it took to get it through.
//!
//! PROVEN NOT SO, end to end, through the real compiled `serve hook` binary -
//! the same process Claude Code itself spawns for a PreToolUse call. The
//! guard reads its candidates straight off `live::candidates_for`/
//! `live::live_items`/`live::always_candidates`, never through
//! `decay::retain_live`, so noise-marking a rule changes nothing about
//! whether it can still block a write
//! (`two_noise_marks_since_the_last_useful_one_do_not_disarm_the_write_guard`
//! below). Decay only ever touches the INFORMATIONAL surfaces - session
//! start, the per-prompt/per-action render, and their `why`/`check` previews
//! - and the second test here confirms that half still works exactly as
//! documented: the very same retired rule really does stop appearing in
//! `serve why`. See `serve/src/decay.rs`'s own `retain_live` doc comment and
//! `serve/src/absent_guard.rs`'s own module doc comment for the two halves
//! of this same invariant stated where the code lives, and `serve/tests/
//! decay_is_decided_in_exactly_one_place.rs`'s own
//! `the_write_guard_never_applies_decay_to_its_candidate_pool` for the
//! static (source-level) half of this same proof, which never depends on
//! running anything.

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

fn edit_payload(session_id: &str, cwd: &Path, file_path: &Path, old_string: &str, new_string: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Edit",
        "tool_input": {
            "file_path": file_path.to_string_lossy(),
            "old_string": old_string,
            "new_string": new_string,
        },
    })
    .to_string()
}

fn run_why(db: &Path, cwd: &Path, file: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("why")
        .arg("--file")
        .arg(file)
        .current_dir(cwd)
        .output()
        .expect("run serve why");
    assert!(out.status.success(), "serve why must exit 0: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// A real store with one live Rule anchored to `notes.md` (relative to
/// `dir`), severity `Irreversible`, carrying a still-current `Check::Absent`
/// forbidding the literal `SECRET` - exactly the fixture the investigation
/// this file proves was asked for. `notes.md` is created on disk right here
/// so the anchor's currency actually holds (see
/// `absent_guard::anchor_is_current`). Returns the store path and the item's
/// own id.
fn fixture_with_secret_rule(dir: &Path) -> (std::path::PathBuf, String) {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("notes.md"), "# notes\n").unwrap();
    let id = "no-secret-in-notes".to_string();
    let item = model::item::Item {
        id: id.clone(),
        kind: model::item::Kind::Rule,
        text: "notes.md never carries the word SECRET".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "notes.md".to_string(),
        }],
        severity: Some(model::item::Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("SECRET lands in notes.md and nobody notices".to_string()),
        check: Some(model::item::Check::Absent { path: "notes.md".to_string(), literal: "SECRET".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    (db, id)
}

/// Two `item_marked_noise` events for `id`, no `item_marked_useful` since -
/// exactly `decay::NOISE_MARKS_BEFORE_STALE`, the threshold
/// `decay::DecayContext::is_stale` retires on. Opens its own connection and
/// drops it before returning, the same one-step-per-open convention
/// `absent_check_guard.rs`'s own fixture already uses, so the compiled
/// binary spawned right after never contends with this process for the
/// sqlite file.
fn mark_noise_twice(db: &Path, id: &str) {
    let mut store = thor_core::event_store::EventStore::new(db).unwrap();
    serve::mark::record_noise(&mut store, "s", "l", "a", "2026-09-08T00:00:00Z", id).unwrap();
    serve::mark::record_noise(&mut store, "s", "l", "a", "2026-09-08T00:01:00Z", id).unwrap();
}

/// THE MAIN PROOF. Two noise marks, no useful mark since, then a real
/// PreToolUse `Edit` introducing the exact forbidden literal - through the
/// real compiled binary, the same one Claude Code's own hook invocation
/// spawns. If the hypothesised hole were real this would come back empty (or
/// a plain, non-blocking context render); it comes back a `block` decision
/// naming the very rule that was twice called noise.
#[test]
fn two_noise_marks_since_the_last_useful_one_do_not_disarm_the_write_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (db, id) = fixture_with_secret_rule(dir.path());
    mark_noise_twice(&db, &id);
    let notes = dir.path().join("notes.md");

    let out = run_hook(&db, &edit_payload("s1", dir.path(), &notes, "# notes\n", "# notes\nSECRET\n"));
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("expected a block decision even though the rule was twice called noise: {e}: {out}"));
    assert_eq!(
        v["decision"], "block",
        "a rule twice called noise must still block the exact write its check exists to stop: {out}"
    );
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains(&id), "the block must still name the retired rule's own id: {reason}");
}

/// THE OTHER HALF, so the first test is never mistaken for "decay simply
/// never fires any more": the SAME two noise marks really do retire this
/// item from the informational surface - `serve why`, run through the same
/// compiled binary, exactly the way a person standing in `dir` would run it.
/// Decay is real; it just never reaches the guard the first test just proved
/// still blocks.
#[test]
fn the_same_retired_rule_no_longer_appears_in_serve_why() {
    let dir = tempfile::tempdir().unwrap();
    let (db, id) = fixture_with_secret_rule(dir.path());

    let before = run_why(&db, dir.path(), "notes.md");
    assert!(before.contains(&id), "fixture sanity: before any noise mark, why must list the rule: {before}");

    mark_noise_twice(&db, &id);

    let after = run_why(&db, dir.path(), "notes.md");
    assert!(
        !after.contains(&id),
        "two noise marks since the last useful one must retire the rule from `serve why`: {after}"
    );
}
