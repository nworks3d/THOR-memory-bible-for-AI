//! Opt-in cross-encoder rerank for the DELIBERATE recall path (feature
//! `semantic`). A cross-encoder scores each (query, document) PAIR through a
//! full transformer pass - far better at paraphrase ordering than the cosine
//! of two independently-embedded vectors, and far too slow for the per-prompt
//! courier (a forward pass per document on CPU). So it never touches the hot
//! path: MCP recall `rerank:true` and `--rerank` flags only, reordering just
//! the small fused top pool.
//!
//! Contract identical to the semantic layer: model absent or ANY failure =
//! the fused order stands, never an error. The model is a separate optional
//! download in its own directory (`reranker/` next to `model/`), same
//! five-file layout as the embedder; nothing here auto-downloads.

use anyhow::{Context, Result};
use fastembed::{
    OnnxSource, RerankInitOptionsUserDefined, TextRerank, TokenizerFiles,
    UserDefinedRerankingModel,
};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The five files the user-defined reranker needs, mirroring
/// `embed::MODEL_FILES` (the onnx file is named plain `model.onnx` here -
/// rename on download if the source calls it `model_quantized.onnx`).
pub const RERANK_MODEL_FILES: [&str; 5] = [
    "model.onnx",
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

/// How many of the fused top hits get rescored. Small on purpose: the
/// cross-encoder costs one full forward pass per document. Callers fetch at
/// least this many candidates so a gold buried just below the requested limit
/// can still be rescued into it.
pub const RERANK_TOP_N: usize = 12;

/// Bodies are truncated to this many chars before scoring (footer stripped
/// first) - parity with the embedder's input discipline, and it bounds the
/// per-document cost.
const MAX_RERANK_CHARS: usize = 1000;

/// A loaded cross-encoder. Loading costs seconds (onnxruntime session init on
/// a few-hundred-MB model), so the MCP server keeps ONE instance warm via
/// [`rerank_hits`]; a one-shot CLI invocation pays the load each time.
pub struct Reranker {
    inner: TextRerank,
}

impl Reranker {
    /// Load the model from a directory holding the five `RERANK_MODEL_FILES`.
    /// The onnx file is streamed from disk by onnxruntime (`OnnxSource::File`),
    /// never buffered whole into memory.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let read = |name: &str| -> Result<Vec<u8>> {
            let p = model_dir.join(name);
            std::fs::read(&p).with_context(|| format!("reading reranker file {}", p.display()))
        };
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };
        let model = UserDefinedRerankingModel::new(
            OnnxSource::File(model_dir.join("model.onnx")),
            tokenizer_files,
        );
        let inner =
            TextRerank::try_new_from_user_defined(model, RerankInitOptionsUserDefined::default())
                .context("initializing the ONNX reranker")?;
        Ok(Self { inner })
    }

    pub fn load_default() -> Result<Self> {
        Self::load(&default_reranker_dir())
    }

    /// Score `docs` against `query`; returns indices into `docs`, best first.
    pub fn rank(&mut self, query: &str, docs: &[String]) -> Result<Vec<usize>> {
        let prepped: Vec<String> = docs
            .iter()
            .map(|d| crate::footer::strip(d).chars().take(MAX_RERANK_CHARS).collect())
            .collect();
        // query and documents share one generic S in fastembed's signature
        let results = self.inner.rerank(query.to_string(), &prepped, false, None)?;
        Ok(results.into_iter().map(|r| r.index).collect())
    }
}

/// The default per-user reranker directory, next to the embedder's `model/`.
pub fn default_reranker_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return Path::new(&local).join("thor").join("reranker");
    }
    PathBuf::from("thor-reranker")
}

/// True iff every required file is present (callers decide to rerank or keep
/// the fused order without catching a load error).
pub fn model_present(dir: &Path) -> bool {
    RERANK_MODEL_FILES.iter().all(|f| dir.join(f).is_file())
}

/// Reorder `hits` so the first `order.len()` positions follow the reranker's
/// order; anything past the rescored pool keeps its fused position. Pure, so
/// the reordering is testable without a model. Out-of-range or duplicate
/// indices void the reorder (fused order returned) - a broken permutation
/// must never drop or duplicate a hit.
pub fn reorder<T>(hits: Vec<T>, order: &[usize]) -> Vec<T> {
    let n = order.len().min(hits.len());
    let valid = {
        let mut seen = vec![false; n];
        order.iter().take(n).all(|&i| {
            i < n && !std::mem::replace(&mut seen[i], true)
        }) && order.len() == n
    };
    if !valid {
        return hits;
    }
    let mut slots: Vec<Option<T>> = hits.into_iter().map(Some).collect();
    let mut out: Vec<T> = Vec::with_capacity(slots.len());
    for &i in order {
        out.push(slots[i].take().expect("validated permutation"));
    }
    for slot in slots {
        if let Some(h) = slot {
            out.push(h);
        }
    }
    out
}

/// Rerank the top of a fused hit list with the process-wide warm reranker.
/// Returns (hits, applied): `applied == false` means the model is missing or
/// failed and the fused order was returned untouched. The first load failure
/// pins unavailability for the process lifetime (a missing model does not
/// grow back mid-run; restart after installing it).
pub fn rerank_hits(query: &str, hits: Vec<crate::recall::RecallHit>) -> (Vec<crate::recall::RecallHit>, bool) {
    if hits.len() < 2 {
        return (hits, false); // nothing to reorder - never load the model for it
    }
    static WARM: OnceLock<Mutex<Option<Reranker>>> = OnceLock::new();
    let warm = WARM.get_or_init(|| {
        let dir = default_reranker_dir();
        Mutex::new(if model_present(&dir) { Reranker::load(&dir).ok() } else { None })
    });
    let mut guard = warm.lock().unwrap_or_else(|p| p.into_inner());
    match guard.as_mut() {
        Some(reranker) => rerank_hits_with(reranker, query, hits),
        None => (hits, false),
    }
}

/// Same as [`rerank_hits`] but with an explicit instance (one-shot CLI and the
/// benchmark harness load their own).
pub fn rerank_hits_with(
    reranker: &mut Reranker,
    query: &str,
    hits: Vec<crate::recall::RecallHit>,
) -> (Vec<crate::recall::RecallHit>, bool) {
    let n = hits.len().min(RERANK_TOP_N);
    if n < 2 {
        return (hits, false); // nothing to reorder
    }
    let docs: Vec<String> = hits[..n].iter().map(|h| h.body.clone()).collect();
    match reranker.rank(query, &docs) {
        Ok(order) => (reorder(hits, &order), true),
        Err(_) => (hits, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_respects_permutation_and_keeps_tail() {
        let hits = vec!["a", "b", "c", "d", "e"];
        // rescored pool of 3, reversed; tail d/e untouched
        assert_eq!(reorder(hits, &[2, 1, 0]), vec!["c", "b", "a", "d", "e"]);
    }

    #[test]
    fn reorder_voids_broken_permutations() {
        assert_eq!(reorder(vec!["a", "b", "c"], &[0, 0, 1]), vec!["a", "b", "c"], "duplicate index");
        assert_eq!(reorder(vec!["a", "b", "c"], &[0, 1, 9]), vec!["a", "b", "c"], "out of range");
        assert_eq!(reorder(vec!["a", "b"], &[0]), vec!["a", "b"], "short order");
        let empty: Vec<&str> = vec![];
        assert_eq!(reorder(empty, &[]), Vec::<&str>::new());
    }

    #[test]
    fn missing_model_is_not_present_and_load_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!model_present(dir.path()));
        assert!(Reranker::load(dir.path()).is_err(), "clear error, caller degrades");
    }
}
