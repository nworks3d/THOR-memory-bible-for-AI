//! Code is archive, never injected (CONTRACT gap 2): the code index this
//! workspace builds from git may be reached ONLY through `serve::lookup`
//! (surface 4, OPZOEKEN) - never from `session_start` (surface 1), the
//! moment/prompt selection path (`serve()`/`serve_prompt()` in `lib.rs`,
//! `rank.rs`, `prompt.rs`), or the `hook` channel (`bin/serve.rs`'s
//! `hook_once`), since those are the three injection surfaces this workspace
//! promises never touch code on their own.
//!
//! This greps every `.rs` file under `serve/src` (library and binary alike)
//! for the crate name `codeindex` and fails if it turns up anywhere but
//! `serve/src/lookup.rs` - mirroring the grep style already used by
//! `model/tests/single_can_fire_definition.rs` and `model/tests/
//! single_normalization.rs` for this workspace's other "exactly one place"
//! guarantees. A future change that wires code search into an injection
//! surface would have to import this crate to do it, so this test is a
//! structural lock on the door, not a hope.

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
fn codeindex_is_named_only_in_lookup_rs() {
    let serve_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&serve_src, &mut files);
    assert!(!files.is_empty(), "expected to find serve's own source files under {}", serve_src.display());

    let needle = "codeindex";
    let mut offenders = Vec::new();
    for file in &files {
        if file.file_name().is_some_and(|n| n == "lookup.rs") {
            continue; // the one allowed door
        }
        let text = std::fs::read_to_string(file).unwrap();
        let count = text.matches(needle).count();
        if count > 0 {
            offenders.push(format!("{} ({count} hit(s))", file.display()));
        }
    }

    assert!(
        offenders.is_empty(),
        "the code index must be reachable only through serve::lookup (surface 4) - found the name \
         'codeindex' outside lookup.rs in: {offenders:?}"
    );
}
