//! Model/vector-sidecar identity and presence checks, deliberately kept free
//! of the ONNX embedding crate or any SQL dependency so this whole file
//! compiles and runs in EVERY build, feature or no feature. That is the
//! point: `status`/`doctor` must
//! always be able to say which search mode is active (CONTRACT: "een
//! ontbrekend model is geen fout maar een stille terugval - en het
//! statuscommando en de gezondheidscheck zeggen WELKE modus actief is,
//! zodat het nooit onduidelijk is"), and they must agree with each other
//! and with `lookup::search_best_effort` about what "the model" even means -
//! one definition, read by all three, rather than three call sites each
//! guessing the same path or the same identity string separately.
//!
//! `default_model_dir` mirrors THOR 1.0's own `ledger::data_dir` resolution
//! exactly (`LOCALAPPDATA` / `XDG_DATA_HOME` / `HOME/.local/share`, then
//! `thor/model`) because it points at the SAME per-user model directory 1.0
//! already uses - the model files are a generic local sentence-embedding
//! model, not something owned by either codebase, so both may read the same
//! copy. This workspace only ever reads that directory; it never writes,
//! moves or deletes anything there.

use std::path::{Path, PathBuf};

/// The five files a "bring your own" ONNX embedding model needs, in the
/// exact names the embedder wrapper (feature `semantic`) reads. Kept here,
/// not in `embed.rs`, so presence can be checked without the feature.
pub const MODEL_FILES: [&str; 5] =
    ["model_optimized.onnx", "tokenizer.json", "config.json", "special_tokens_map.json", "tokenizer_config.json"];

/// Identity of the embedding model + pooling recipe this workspace's own
/// vector sidecar was built with. Deliberately a DIFFERENT string from THOR
/// 1.0's own `embed::MODEL_ID` (even though both currently point at the same
/// physical model file) so a thor2 sidecar and a 1.0 sidecar are never
/// mistaken for interchangeable just because they happen to share a model.
pub const MODEL_ID: &str = "paraphrase-multilingual-MiniLM-L12-v2@mean-thor2-v1";

/// Output dimensionality of the model above (`config.json`'s own
/// `hidden_size`, read directly off the installed model file - not assumed).
pub const DIM: usize = 384;

/// The per-user model directory: `LOCALAPPDATA`/`XDG_DATA_HOME`/
/// `HOME/.local/share`, then `thor/model` - the exact resolution order THOR
/// 1.0's `ledger::data_dir` uses, so both land on the same directory. `None`
/// when no per-user home resolves at all; callers treat that as "no model"
/// the same way a present-but-empty directory is treated.
pub fn default_model_dir() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| Path::new(&h).join(".local").join("share")))?;
    Some(base.join("thor").join("model"))
}

/// True iff every file `MODEL_FILES` names is present in `dir` - a cheap
/// existence check, never a load, so a status line never pays the ~1.25s
/// ONNX session cost just to say whether the model is there.
pub fn model_present(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|f| dir.join(f).is_file())
}

/// The vector sidecar's own path, derived from the main store's path -
/// `thor2-vectors.db` next to it. A different filename from THOR 1.0's own
/// `thor-vectors.db` on purpose (see `MODEL_ID`'s doc comment): the two
/// sidecars are never meant to be confused, even if they end up in the same
/// directory.
pub fn default_vectors_path(db: &Path) -> PathBuf {
    db.with_file_name("thor2-vectors.db")
}

/// Which mode `lookup::search_best_effort` is running in right now, in three
/// states - never fewer, because "compiled without the feature" and
/// "compiled in but the model is missing" both degrade to plain text match,
/// but for a different reason, and a person reading `status`/`doctor` should
/// never have to guess which one applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// The `semantic` feature was not compiled into this binary at all.
    CompiledOut,
    /// Compiled in, but no usable model was found at the resolved directory.
    ModelMissing,
    /// Compiled in and the model files are present (says nothing about
    /// whether any vectors have actually been built yet - see `vectors`'s
    /// own report for that).
    ModelPresent,
}

/// Resolve the active mode: `model_dir` overrides `default_model_dir` when
/// given (mirrors `serve vectors build --model-dir`'s own override).
pub fn search_mode(model_dir: Option<&Path>) -> SearchMode {
    if !cfg!(feature = "semantic") {
        return SearchMode::CompiledOut;
    }
    match model_dir.map(Path::to_path_buf).or_else(default_model_dir) {
        Some(dir) if model_present(&dir) => SearchMode::ModelPresent,
        _ => SearchMode::ModelMissing,
    }
}

/// One plain-language line stating the active mode - the exact text
/// `serve status` and `ops::health::semantic_line` (doctor) both print, so
/// the two can never phrase this differently.
pub fn mode_line(model_dir: Option<&Path>) -> String {
    match search_mode(model_dir) {
        SearchMode::CompiledOut => {
            "semantic search: not compiled into this binary (build with --features semantic to add meaning search)"
                .to_string()
        }
        SearchMode::ModelMissing => {
            let resolved = model_dir.map(Path::to_path_buf).or_else(default_model_dir);
            match resolved {
                Some(dir) => format!(
                    "semantic search: compiled in, but no model found at {} - falling back to plain text match",
                    dir.display()
                ),
                None => "semantic search: compiled in, but no per-user data directory could be resolved - falling back to plain text match".to_string(),
            }
        }
        SearchMode::ModelPresent => {
            let dir = model_dir.map(Path::to_path_buf).or_else(default_model_dir).expect("ModelPresent implies a resolved directory");
            format!("semantic search: model present at {} (see 'vectors-status' for how many are built)", dir.display())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_present_is_false_on_an_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!model_present(dir.path()), "an empty directory must never read as a present model");
    }

    #[test]
    fn model_present_is_true_only_when_every_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        for (i, f) in MODEL_FILES.iter().enumerate() {
            assert!(!model_present(dir.path()), "must stay false until every file is present");
            std::fs::write(dir.path().join(f), format!("fixture {i}")).unwrap();
        }
        assert!(model_present(dir.path()), "must be true once all five files are present");
    }

    #[test]
    fn default_vectors_path_sits_next_to_the_store_under_its_own_name() {
        let db = Path::new("/some/where/thor2.db");
        let vectors = default_vectors_path(db);
        assert_eq!(vectors, Path::new("/some/where/thor2-vectors.db"));
    }

    // ------------------------------------------------------------ mode_line

    #[cfg(not(feature = "semantic"))]
    #[test]
    fn mode_line_says_not_compiled_when_the_feature_is_off() {
        // Named after the defect it prevents: a default build must still
        // answer "which mode" plainly, never silently omit the question.
        let line = mode_line(None);
        assert!(line.contains("not compiled"), "{line}");
        assert_eq!(search_mode(None), SearchMode::CompiledOut);
    }

    #[cfg(feature = "semantic")]
    #[test]
    fn mode_line_says_missing_when_the_directory_has_no_model_files() {
        let dir = tempfile::tempdir().unwrap();
        let line = mode_line(Some(dir.path()));
        assert!(line.contains("no model found"), "{line}");
        assert!(line.contains("falling back"), "{line}");
        assert_eq!(search_mode(Some(dir.path())), SearchMode::ModelMissing);
    }

    #[cfg(feature = "semantic")]
    #[test]
    fn mode_line_says_present_when_every_model_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        for f in MODEL_FILES {
            std::fs::write(dir.path().join(f), "fixture").unwrap();
        }
        let line = mode_line(Some(dir.path()));
        assert!(line.contains("model present"), "{line}");
        assert_eq!(search_mode(Some(dir.path())), SearchMode::ModelPresent);
    }
}
