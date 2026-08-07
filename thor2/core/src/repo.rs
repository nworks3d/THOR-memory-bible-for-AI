//! MINIMAL PORT of thor/src/repo.rs (1.0 crate). The 1.0 file is a repo/ingest
//! helper module (git walking, chunking, project-key resolution, ...) that
//! belongs to a layer NOT ported here. Storage needs exactly two things from
//! it, both copied unchanged from the 1.0 source:
//! - `FactType`, the return type of footer::fact_type (used by
//!   event_store.rs's tests via crate::repo::FactType::Gotcha);
//! - `owner_project`, the birth-project derivation folded by
//!   cas::compute_projects and mirrored by event_store::apply_projection_delta.
//! Everything else in the 1.0 repo.rs (tracked_files, chunk_text, project_key,
//! is_chunk_id, project_root, ...) is out of scope for the storage layer and
//! is deliberately left out.

/// The constraint class of a hand-written fact, so the courier / guard / brief
/// can label (and later prioritize) the facts that prevent drift. Only these
/// three classes get a tag; anything else (note, insight, plain text) is None.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactType {
    Gotcha,
    Decision,
    Preference,
}

impl FactType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FactType::Gotcha => "gotcha",
            FactType::Decision => "decision",
            FactType::Preference => "preference",
        }
    }
}

/// The project that owns an entity: the id prefix before the first `:` for repo
/// chunks; `None` for unprefixed (global) facts, which are never scoped out.
pub fn owner_project(entity_id: &str) -> Option<&str> {
    entity_id.split_once(':').map(|(p, _)| p)
}
