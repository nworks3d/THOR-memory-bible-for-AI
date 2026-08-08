//! A guard against the one defect class this project keeps producing: a
//! comment that describes a mechanism the code does not have, believed by the
//! next reader, and used to justify a change.
//!
//! THE CASE THIS WAS BUILT FROM. On 2026-08-08 a comment in
//! `model/src/store.rs` asserted that "every serving surface builds its input
//! with `ServeInput::add_file`, which adds a Path target AND a Dir target for
//! the same touch". It does not - `add_file` pushes one target, a Path. On the
//! strength of that sentence a rival rule was added to the crowding count,
//! which decides a REFUSAL, so the memory could refuse an honest write for
//! rivals it would never meet. Two independent reviews found it the same
//! evening, both by reading `input.rs` rather than the comment. A whole
//! afternoon of measurement had already been taken through the wrong model.
//!
//! Nothing here checks prose in general - that is not mechanisable. It pins
//! the ONE claim that was load-bearing and got it wrong, in both directions:
//! the code must keep not doing it, and no comment may say it does.
//!
//! Mirrors `decay_is_decided_in_exactly_one_place.rs`'s technique (read the
//! workspace's own source, assert a property over it) applied to a claim
//! rather than to a definition.

use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == "target") {
            continue;
        }
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("serve/ has a parent directory (the workspace root)")
        .to_path_buf()
}

/// The code half: `add_file` must keep adding exactly one target kind, Path.
/// If a future change genuinely wants a Dir target per touch, this test is
/// where that decision gets made deliberately - and everything that reasons
/// about pools (`model::store::pool_rivals`, `capacity`) has to be revisited
/// in the same commit.
#[test]
fn add_file_adds_a_path_target_and_no_directory_target() {
    let input_rs = workspace_root().join("serve/src/input.rs");
    let src = std::fs::read_to_string(&input_rs).expect("serve/src/input.rs must be readable");

    let start = src.find("pub fn add_file").expect("add_file must exist");
    let body = &src[start..];
    let end = body.find("\n    }").expect("add_file must have a closing brace");
    let body = &body[..end];

    assert!(body.contains("TargetKind::Path"), "add_file must still add a Path target: {body}");
    assert!(
        !body.contains("TargetKind::Dir"),
        "add_file now adds a Dir target. That is a real design change, not a tidy-up: every \
         crowding decision in model::store assumes it does not, and a Dir-bound item is \
         currently unservable at a file touch because of it. Revisit pool_rivals and capacity \
         in the same change, then update this test on purpose. Body was: {body}"
    );
}

/// The prose half: no source file may claim the mechanism the test above just
/// proved absent. Checked as a phrase rather than a regex over English,
/// because the exact sentence is what was copied from one file into another.
#[test]
fn no_comment_claims_a_touch_adds_a_directory_target() {
    let mut files = Vec::new();
    let root = workspace_root();
    for crate_dir in ["core", "intent", "model", "serve", "codeindex", "mcp", "ops"] {
        collect_rs_files(&root.join(crate_dir), &mut files);
    }
    assert!(!files.is_empty(), "the workspace crates must be readable");

    // Both spellings the false claim actually took, normalised for case.
    let banned = ["a path target and a dir target", "path target and a dir target for the same touch"];
    let mut offenders = Vec::new();
    for file in &files {
        let Ok(src) = std::fs::read_to_string(file) else { continue };
        // This file itself quotes the claim, on purpose, to name it.
        if file.file_name().is_some_and(|n| n == "a_comment_never_claims_what_the_code_does_not_do.rs") {
            continue;
        }
        let lower = src.to_lowercase();
        for phrase in banned {
            if lower.contains(phrase) {
                offenders.push(format!("{} claims: {phrase}", file.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a comment has reintroduced the claim that a file touch adds a directory target. \
         `ServeInput::add_file` adds a Path target only, and normalize::target_matches refuses a \
         kind mismatch, so a Dir-bound item is not in that pool at all: {offenders:#?}"
    );
}
