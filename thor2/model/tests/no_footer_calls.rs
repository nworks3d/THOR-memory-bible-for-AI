//! R6 / F1 enforcement: the model layer may never call into `core::footer`.
//! That module survives in `core` for exactly one purpose (reading 1.0's
//! legacy prose-footer bodies during a later migration); `model` stores
//! every metadata field as real JSON, so it never needs to compose or parse
//! a footer. This test greps `model`'s own source and fails on any hit.
//!
//! Lives in `model/tests/`, not `model/src/`, so scanning `model/src`
//! entirely (this file included in no scan) can never make the test trip
//! over its own text.

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
fn model_never_calls_core_footer() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);
    assert!(!files.is_empty(), "expected to find model's own source files under {}", src_dir.display());

    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            !text.contains("footer::"),
            "{} calls into core::footer - the model layer must never parse or compose the prose \
             footer (see FINDINGS-1.0.md F1): every metadata field belongs in a typed Item field, \
             stored as canonical JSON",
            file.display()
        );
    }
}
