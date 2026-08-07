//! The embedding layer for surface 4's meaning search (feature `semantic`,
//! part of the OPZOEKEN surface only - see this crate's own `lib.rs` doc
//! comment and `lookup::search_best_effort`, the only caller). Wraps
//! fastembed's ONNX runtime to turn text into a unit-norm dense vector with
//! the EXACT model + pooling `semantic_paths::MODEL_ID` names; query and
//! stored document vectors MUST come from this one path so their cosines are
//! comparable - a mismatch is caught by `vectors::VectorStore::model_id`
//! before this struct is ever loaded (see `lookup::search_best_effort`).
//!
//! Ported from THOR 1.0's own proven `embed.rs` (same model file, same
//! fastembed version, same recipe) rather than re-derived - see this
//! workspace's CONTRACT.md instruction to reuse the existing local model
//! rather than build a second embedding path.

use crate::semantic_paths::MODEL_FILES;
use anyhow::{Context, Result};
use fastembed::{InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel};
use std::path::Path;

/// Bodies are truncated to this many chars before embedding - carried over
/// from THOR 1.0's own proven recipe (a known-good engineering bound on
/// tokenization cost for this exact model), not a claim about accuracy.
const MAX_EMBED_CHARS: usize = 1000;

/// A loaded ONNX embedder. Loading is expensive (roughly a second: onnxruntime
/// session init dominates), so a caller that will embed more than one text
/// (a `vectors build` run, in particular) keeps one instance and reuses it.
pub struct Embedder {
    inner: TextEmbedding,
}

impl Embedder {
    /// Load the model from a directory holding the five `MODEL_FILES`. Fails
    /// with a clear message naming the missing file; every caller in this
    /// crate only reaches this after `semantic_paths::model_present` already
    /// said yes, so a failure here means the files exist but are not valid,
    /// not that they are simply absent.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let read = |name: &str| -> Result<Vec<u8>> {
            let p = model_dir.join(name);
            std::fs::read(&p).with_context(|| format!("reading model file {}", p.display()))
        };
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read(MODEL_FILES[1])?,
            config_file: read(MODEL_FILES[2])?,
            special_tokens_map_file: read(MODEL_FILES[3])?,
            tokenizer_config_file: read(MODEL_FILES[4])?,
        };
        let model = UserDefinedEmbeddingModel::new(read(MODEL_FILES[0])?, tokenizer_files).with_pooling(Pooling::Mean);
        let inner = TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())
            .context("initializing the ONNX embedder")?;
        Ok(Self { inner })
    }

    /// Embed one text into a unit-norm `semantic_paths::DIM` vector.
    pub fn embed_one(&mut self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_many(std::slice::from_ref(&text.to_string()))?;
        out.pop().context("embedder returned no vector")
    }

    /// Embed a batch of texts into unit-norm vectors. fastembed already
    /// L2-normalizes; we re-normalize defensively so the invariant holds
    /// regardless of fastembed internals (re-normalizing a unit vector is a
    /// no-op). Each text is truncated to `MAX_EMBED_CHARS` first.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_unit_and_zero() {
        let mut v = vec![3.0f32, 4.0];
        normalize(&mut v);
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-6, "normalized to unit length");
        let mut z = vec![0.0f32, 0.0];
        normalize(&mut z);
        assert_eq!(z, vec![0.0, 0.0], "an all-zero vector is left untouched, never divided by zero");
    }

    #[test]
    fn truncate_is_char_safe() {
        // Named after the defect it prevents: truncating by byte count can
        // split a multi-byte codepoint in half, producing invalid UTF-8.
        let s = "eeeeé".repeat(500);
        let t = truncate_chars(&s, MAX_EMBED_CHARS);
        assert!(t.chars().count() <= MAX_EMBED_CHARS);
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn load_over_a_directory_missing_every_file_is_a_named_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        match Embedder::load(dir.path()) {
            Err(e) => assert!(!e.to_string().is_empty(), "a missing model file must be a named error, never a panic"),
            Ok(_) => panic!("loading from an empty directory must fail"),
        }
    }
}
