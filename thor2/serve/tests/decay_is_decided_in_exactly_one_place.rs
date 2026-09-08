//! The property "is this item currently stale" (`decay::DecayContext::is_stale`)
//! must have exactly ONE definition in the whole workspace - mirrors `model/
//! tests/single_can_fire_definition.rs`'s own technique, applied to the
//! second half of "may this item fire right now" (the first half, which
//! KINDS can ever fire, stays that test's alone). Without this, a future
//! injection surface could inline its own `>= NOISE_MARKS_BEFORE_STALE` check
//! instead of calling `decay::retain_live`, and a later change to the
//! threshold or the marked-useful override could update one call site and
//! silently leave the other stale - literally the defect this file's own
//! name warns against.

use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn staleness_is_decided_in_exactly_one_place_in_the_workspace() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("serve/ has a parent directory (the workspace root)")
        .to_path_buf();

    let mut files = Vec::new();
    for crate_src in ["core/src", "intent/src", "model/src", "serve/src", "codeindex/src"] {
        collect_rs_files(&workspace_root.join(crate_src), &mut files);
    }
    assert!(!files.is_empty(), "expected to find source files under {}", workspace_root.display());

    // The exact literal the staleness comparison is spelled with, wherever it
    // is made. `DecayContext::is_stale`'s own body in serve/src/decay.rs is
    // the one permitted hit.
    let needle = ">= NOISE_MARKS_BEFORE_STALE";
    let mut total = 0usize;
    let mut where_found = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        let count = text.matches(needle).count();
        if count > 0 {
            total += count;
            where_found.push(format!("{} ({count})", file.display()));
        }
    }

    assert_eq!(
        total, 1,
        "expected exactly one definition of the staleness comparison across the whole workspace \
         (core/src, intent/src, model/src, serve/src, codeindex/src) - found {total} in: {where_found:?}"
    );
}

#[test]
fn every_injection_surface_caller_applies_retain_live() {
    // The complementary half: every real caller of an injection surface
    // (lib.rs's serve()/serve_prompt(), and the CLI's hook/session-start/
    // prompt commands) must route through decay::retain_live rather than
    // rendering rank::select's or session_start::select's raw output
    // directly. Named after the defect it prevents: a new injection call
    // site added without decay would silently keep serving a stale item
    // forever.
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("serve/ has a parent directory (the workspace root)")
        .to_path_buf();

    let lib_rs = std::fs::read_to_string(workspace_root.join("serve/src/lib.rs")).unwrap();
    let bin_serve_rs = std::fs::read_to_string(workspace_root.join("serve/src/bin/serve.rs")).unwrap();

    assert_eq!(lib_rs.matches("decay::retain_live").count(), 2, "serve() and serve_prompt() must both apply decay");
    assert_eq!(
        bin_serve_rs.matches("decay::retain_live").count(),
        4,
        "the hook channel's SessionStart and UserPromptSubmit branches, cmd_session_start and \
         cmd_prompt must all apply decay (the PreToolUse branch and cmd_check/cmd_why go through \
         serve::serve, which already does)"
    );
}

/// THE HOLE THIS RULES OUT, raised 2026-09-08 against another memory
/// server's own rule ("an agent cannot write its own success story"): THOR's
/// mark tool is callable by the very agent a blocking rule just refused, and
/// two `mark noise` verdicts since the last useful one is all
/// `decay::is_stale` needs to retire an item from an injection surface. If
/// the write guard (`absent_guard`, wired in `bin/serve.rs`'s
/// `absent_guard_block`/`command_guard_block`/`requires_guard_block`) ever
/// took its own candidate pool AFTER that retirement, two `mark noise` calls
/// plus the write a rule exists to stop would be enough to get it through -
/// an agent judging the very rule that judges it.
///
/// It does not: every one of the guard's pure decision functions
/// (`find_violation`, `first_location_violation`, `first_dir_violation`,
/// `first_check_path_dir_violation`, `find_requires_violation`,
/// `first_command_violation`, `find_missing_required`, `find_forbidden_
/// violation`) lives in `absent_guard.rs`, and every one of them reads its
/// candidates straight off `live::candidates_for`/`live::live_items`/
/// `live::always_candidates` - never through `decay::retain_live` or
/// `DecayContext::is_stale`. This is the static half of that proof: the
/// source names none of decay's own machinery at all, so a future change
/// cannot wire the two together by accident without this test naming the
/// exact line it happened on. See `serve/tests/
/// decay_never_disarms_the_write_guard.rs` for the end-to-end half, through
/// the real compiled binary against a real noise-marked rule.
#[test]
fn the_write_guard_never_applies_decay_to_its_candidate_pool() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("serve/ has a parent directory (the workspace root)")
        .to_path_buf();
    let absent_guard_rs = std::fs::read_to_string(workspace_root.join("serve/src/absent_guard.rs")).unwrap();
    for needle in ["decay::", "DecayContext", "retain_live", "is_stale"] {
        assert!(
            !absent_guard_rs.contains(needle),
            "absent_guard.rs must never reference {needle} - the write guard's own candidate pool \
             must never pass through noise-based retirement, only through live::* readers"
        );
    }
}
