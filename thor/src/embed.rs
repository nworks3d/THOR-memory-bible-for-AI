//! Semantic-recall embedding layer (feature `semantic`, client-only).
//!
//! Wraps fastembed's ONNX runtime to turn text into a unit-norm dense vector with
//! the EXACT model the recall eval was tuned on (multilingual MiniLM, mean
//! pooling). Query and document vectors MUST come from this one path so their
//! cosines are comparable; `MODEL_ID` is stamped into the sidecar and a mismatch
//! forces a full rebuild.
//!
//! The model files live under our OWN directory (`%LOCALAPPDATA%\thor\model\`),
//! not a shared Python/fastembed cache, so THOR stays 100% independent.

use anyhow::{Context, Result};
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use std::path::{Path, PathBuf};

/// Identity of the embedding model + pooling. Stored as the sidecar's `model_id`;
/// any change here (or a differently-embedded sidecar) is a mismatch that forces
/// `thor vectors build` to rebuild from scratch. Bump this string if the model,
/// pooling, or preprocessing ever changes.
pub const MODEL_ID: &str = "paraphrase-multilingual-MiniLM-L12-v2-onnx-Q@mean-v1";

/// Output dimensionality of the model. Used to sanity-check stored vectors.
pub const DIM: usize = 384;

/// Bodies are truncated to this many chars before embedding, matching the eval
/// (which embedded `body[:1000]`). Keeps long imported chunks from diluting the
/// vector and bounds tokenization cost.
const MAX_EMBED_CHARS: usize = 1000;

/// The five files a user-defined fastembed model needs. Kept here so `thor
/// vectors build` and any installer agree on the exact names.
pub const MODEL_FILES: [&str; 5] = [
    "model_optimized.onnx",
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

/// A loaded ONNX embedder. Loading is expensive (~1.25s cold: onnxruntime session
/// init dominates), so callers keep ONE instance warm - the MCP server holds its
/// own, and the per-prompt courier reaches a resident daemon rather than loading
/// per hook.
pub struct Embedder {
    inner: TextEmbedding,
}

impl Embedder {
    /// Load the model from a directory holding the five `MODEL_FILES`. Fails with
    /// a clear message naming the missing file, so a caller can degrade to bm25.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let read = |name: &str| -> Result<Vec<u8>> {
            let p = model_dir.join(name);
            std::fs::read(&p).with_context(|| format!("reading model file {}", p.display()))
        };
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };
        let model = UserDefinedEmbeddingModel::new(read("model_optimized.onnx")?, tokenizer_files)
            .with_pooling(Pooling::Mean);
        let inner =
            TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())
                .context("initializing the ONNX embedder")?;
        Ok(Self { inner })
    }

    /// Load from the per-user model dir (`model/` inside THOR's data home).
    pub fn load_default() -> Result<Self> {
        let dir = default_model_dir().context(
            "no per-user data directory for the model: LOCALAPPDATA, XDG_DATA_HOME and HOME are all unset",
        )?;
        Self::load(&dir)
    }

    /// Embed one text into a unit-norm `DIM` vector.
    pub fn embed_one(&mut self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_many(&[text.to_string()])?;
        out.pop().context("embedder returned no vector")
    }

    /// Embed a batch of texts into unit-norm vectors. fastembed already
    /// L2-normalizes; we re-normalize defensively so the invariant holds
    /// regardless of fastembed internals (re-normalizing a unit vector is a
    /// no-op). Each text is truncated to `MAX_EMBED_CHARS` first (eval parity).
    pub fn embed_many(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prepped: Vec<String> = texts.iter().map(|t| truncate_chars(t, MAX_EMBED_CHARS)).collect();
        let raw = self.inner.embed(prepped, None).context("embedding failed")?;
        Ok(raw
            .into_iter()
            .map(|mut v| {
                normalize(&mut v);
                v
            })
            .collect())
    }
}

/// Char-safe truncation (never splits a multi-byte codepoint).
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// In-place L2 normalization. Leaves an all-zero vector untouched.
pub fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// The default per-user model directory. THOR's OWN copy of the model files, so
/// the semantic layer never depends on a Python/fastembed cache being present.
/// `model/` inside the same per-user THOR home the store resolves to, so the
/// model sits next to the data it belongs with and every surface agrees on
/// where it is. This used to key on LOCALAPPDATA alone and fall back to the
/// CWD-relative "thor-model", which meant that off Windows the answer depended
/// on which directory the process happened to start in: the courier, the daemon
/// and `thor doctor` could each look somewhere else, and doctor could report the
/// model present while recall found nothing. None when no home resolves at all
/// (the same contract ledger::data_dir has for the store); callers treat that as
/// "no model" and stay on bm25 rather than guessing a relative path.
pub fn default_model_dir() -> Option<PathBuf> {
    crate::ledger::data_dir().map(|d| d.join("model"))
}

/// True iff every required model file is present in `dir` (so a caller can decide
/// to load the semantic path or degrade to bm25 without catching a load error).
pub fn model_present(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|f| dir.join(f).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_unit_and_zero() {
        let mut v = vec![3.0f32, 4.0];
        normalize(&mut v);
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-6, "normalized to unit length");
        let mut z = vec![0.0f32, 0.0];
        normalize(&mut z);
        assert_eq!(z, vec![0.0, 0.0], "all-zero vector is left untouched");
    }

    #[test]
    fn test_truncate_is_char_safe() {
        // multi-byte chars must not be split mid-codepoint
        let s = "eeeeé".repeat(500);
        let t = truncate_chars(&s, MAX_EMBED_CHARS);
        assert!(t.chars().count() <= MAX_EMBED_CHARS);
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn test_model_present_false_on_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!model_present(dir.path()));
    }

    /// The model has to sit in the same per-user home as the store, or the
    /// surfaces that read it (courier, resident embedder, doctor) can each end
    /// up somewhere else. The old resolver keyed on LOCALAPPDATA alone and fell
    /// back to a CWD-relative folder, so off Windows this assertion fails on it:
    /// the store resolves through HOME while the model resolves to a relative
    /// path. On Windows both resolvers agree either way, so this guard bites on
    /// the Linux and macOS CI jobs rather than locally.
    #[test]
    fn model_dir_lives_in_the_same_home_as_the_store() {
        match (crate::ledger::data_dir(), default_model_dir()) {
            (Some(home), Some(model)) => assert_eq!(
                model,
                home.join("model"),
                "the model folder must be `model` inside THOR's data home"
            ),
            (None, None) => {}
            (home, model) => panic!(
                "the store home and the model folder must agree on whether a home exists \
                 (home {home:?}, model {model:?})"
            ),
        }
    }
}
