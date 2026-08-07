//! "May an item owned by project X be served to a session in project Y" is
//! decided in exactly one place: `serve::project::applies_to`. This test
//! fails if a second copy of that comparison appears anywhere in the crate.
//!
//! THE DEFECT THIS PREVENTS. Until 2026-08-03 there was exactly one copy -
//! written inline in `session_start::select` - and the moment-of-action
//! surface simply did not have it. Not a wrong comparison: a MISSING one, in
//! the surface that fires on every tool call. Measured on the live store, 136
//! moment-bound rules owned by one project competed for four slots inside
//! every other project. Two implementations of one rule may disagree; one
//! implementation and one omission is the same class and harder to see,
//! because the surface that has it looks correct.
//!
//! Same instrument, same reason, as `single_project_resolution.rs`,
//! `single_normalization.rs` and `model/tests/single_can_fire_definition.rs`:
//! the property is "there is no second way of doing this", which no ordinary
//! unit test can observe.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The comparison itself, spelled out by hand, is what must not reappear.
/// Anything of the shape `project.is_none() || project == <something>` is a
/// second implementation of `applies_to`.
#[test]
fn the_project_scope_comparison_is_written_out_exactly_once() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);

    let mut offenders = Vec::new();
    for path in &files {
        let Ok(text) = fs::read_to_string(path) else { continue };
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.starts_with("//") || line.starts_with("///") {
                continue;
            }
            // The hand-written form this rule replaced.
            let hand_written = line.contains("project.is_none()")
                && (line.contains("project.as_deref() ==") || line.contains("project =="));
            if hand_written && !path.ends_with("project.rs") {
                offenders.push(format!("{}:{}", path.display(), n + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the project-scope comparison must live only in project::applies_to, \
         found a second hand-written copy at: {offenders:?}"
    );
}

/// And both injection surfaces must actually reach it. A surface that simply
/// never calls it is the omission this whole test file exists for.
#[test]
fn every_injection_surface_calls_the_one_rule() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for surface in ["src/rank.rs", "src/session_start.rs"] {
        let text = fs::read_to_string(root.join(surface))
            .unwrap_or_else(|e| panic!("cannot read {surface}: {e}"));
        assert!(
            text.contains("project::applies_to"),
            "{surface} is an injection surface and must scope through \
             project::applies_to - a surface without it serves every \
             project's rules in every project"
        );
    }
}
