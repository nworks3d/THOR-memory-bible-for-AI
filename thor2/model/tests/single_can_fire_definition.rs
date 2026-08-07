//! The property "which kinds can fire" (`model::item::Kind::can_fire`) must
//! have exactly ONE definition in the whole workspace. This is not a
//! hypothetical concern: before this test existed, `serve::rank::eligible`
//! and `serve::audit::audit_rows` each re-decided the same boolean inline
//! (`matches!(kind, Kind::Rule | Kind::Orientation)`) instead of calling the
//! canonical method, so a future change to which kinds can fire could update
//! one call site and silently leave the other stale. This test greps every
//! crate's source (including `serve`, where the duplicate actually lived) for
//! that literal decision and fails if it appears anywhere but `can_fire`'s
//! own body in `model/src/item.rs`.

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
fn can_fire_is_decided_in_exactly_one_place_in_the_workspace() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("model/ has a parent directory (the workspace root)")
        .to_path_buf();

    let mut files = Vec::new();
    for crate_src in ["core/src", "intent/src", "model/src", "serve/src"] {
        collect_rs_files(&workspace_root.join(crate_src), &mut files);
    }
    assert!(!files.is_empty(), "expected to find source files under {}", workspace_root.display());

    // The exact literal this decision is spelled with wherever it is made.
    // `can_fire`'s own body in model/src/item.rs is the one permitted hit.
    let needle = "Kind::Rule | Kind::Orientation";
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
        "expected exactly one definition of \"which kinds can fire\" across the whole \
         workspace (core/src, intent/src, model/src, serve/src) - found {total} in: {where_found:?}"
    );
}
