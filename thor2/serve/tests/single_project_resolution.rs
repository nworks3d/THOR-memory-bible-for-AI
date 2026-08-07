//! "Which project is this session in", resolved from a filesystem location,
//! must have exactly ONE definition in the whole workspace -
//! `serve::project::resolve_project`. Mirrors `model/tests/
//! single_can_fire_definition.rs` and `model/tests/single_normalization.rs`:
//! this greps every crate's source for a second `fn resolve_project` and
//! fails if one appears anywhere but `serve/src/project.rs`.

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
fn resolve_project_is_defined_exactly_once_in_the_workspace() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("serve/ has a parent directory (the workspace root)")
        .to_path_buf();

    let mut files = Vec::new();
    for crate_src in ["core/src", "intent/src", "model/src", "serve/src", "codeindex/src"] {
        collect_rs_files(&workspace_root.join(crate_src), &mut files);
    }
    assert!(!files.is_empty(), "expected to find source files under {}", workspace_root.display());

    let needle = "fn resolve_project";
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
        "expected exactly one definition of the project-resolution function across the whole \
         workspace (core/src, intent/src, model/src, serve/src, codeindex/src) - found {total} in: {where_found:?}"
    );
}
