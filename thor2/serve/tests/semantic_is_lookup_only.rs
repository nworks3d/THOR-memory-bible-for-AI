//! Meaning search (feature `semantic`) belongs to surface 4 (OPZOEKEN)
//! ONLY - never session_start (surface 1), the moment/prompt selection path
//! (`serve()`/`serve_prompt()` in `lib.rs`, `rank.rs`, `prompt.rs`), or the
//! `hook` channel (`bin/serve.rs`'s `hook_once`): CONTRACT's own words, "daar
//! mag nooit een gelijkenisscore binnenkomen, want dat is precies de
//! gerangschikte pool die 2.0 afschaft."
//!
//! This greps every `.rs` file under `serve/src` (library and binary alike)
//! for the names that only mean anything if a similarity score is involved -
//! the `fastembed` crate itself, `Embedder`, `VectorStore`, and `cosine` -
//! and fails if any of them turns up anywhere but the three files that are
//! allowed to know meaning search exists at all: `lookup.rs` (the surface
//! itself), `embed.rs` and `vectors.rs` (its own machinery). Mirrors the grep
//! style `codeindex_is_lookup_only.rs` already uses for the code index's own
//! "reachable only via lookup" guarantee, and runs unconditionally (plain
//! text search over source files), so it holds in a build with or without
//! the `semantic` feature compiled in.

use std::path::{Path, PathBuf};

const ALLOWED_FILES: [&str; 3] = ["lookup.rs", "embed.rs", "vectors.rs"];
const NEEDLES: [&str; 4] = ["fastembed", "Embedder", "VectorStore", "cosine"];

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
fn a_similarity_score_never_reaches_an_injection_surface() {
    let serve_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&serve_src, &mut files);
    assert!(!files.is_empty(), "expected to find serve's own source files under {}", serve_src.display());

    let mut offenders = Vec::new();
    for file in &files {
        if file.file_name().is_some_and(|n| ALLOWED_FILES.contains(&n.to_str().unwrap_or(""))) {
            continue; // the three files allowed to know meaning search exists
        }
        let text = std::fs::read_to_string(file).unwrap();
        for needle in NEEDLES {
            let count = text.matches(needle).count();
            if count > 0 {
                offenders.push(format!("{} names '{needle}' ({count} hit(s))", file.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a similarity score's own machinery must be reachable only through lookup.rs/embed.rs/vectors.rs - \
         found it named outside them in: {offenders:?}"
    );
}
