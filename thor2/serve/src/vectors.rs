//! The precomputed dense vector sidecar for surface 4's meaning search
//! (feature `semantic`) - a separate SQLite file next to the main event log,
//! never inside it: DERIVED, not authoritative. Delete this file and
//! `lookup::search_best_effort` silently degrades to plain text match;
//! rebuilding is always `build` run once more, over whatever is live right
//! now. Keeping it out of the append-only log preserves that log's purity
//! (a pure hash-chained fact history) and makes the vectors trivially
//! rebuildable and deletable without ever touching the source of truth.
//!
//! Keyed by the item's own `id` (the same stable entity id every other
//! surface in this workspace already uses - `lookup::LookupHit::id`,
//! `audit::audit_rows`, ...), never by event seq: a `revise` keeps the id but
//! changes the body, so a vector recorded under an id can go stale in place.
//! That is exactly what `content_hash` catches (`report`'s `stale` count) -
//! a revised item's text changed, but its stored vector still describes the
//! OLD text, and using it for scoring without noticing would be exactly the
//! "silently wrong answer" this workspace exists to prevent.

use crate::embed::Embedder;
use crate::live::live_items;
use crate::semantic_paths::{DIM, MODEL_ID};
use anyhow::{bail, Result};
use model::item::Kind;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use thor_core::event_store::EventStore;

/// The dense sidecar. Owns its own SQLite connection.
pub struct VectorStore {
    conn: Connection,
}

impl VectorStore {
    /// Open (creating if absent) the sidecar at `path`, ensuring the schema.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL so a search READ never blocks behind a `vectors build` WRITE.
        // synchronous=NORMAL is enough: this sidecar is derived and always
        // rebuildable, so it does not need the main log's FULL-fsync
        // durability.
        conn.execute_batch("PRAGMA journal_mode = WAL")?;
        conn.execute_batch("PRAGMA synchronous = NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS vec(id TEXT PRIMARY KEY, content_hash TEXT NOT NULL, v BLOB NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    /// The `model_id` that produced the stored vectors, if any build ever ran.
    pub fn model_id(&self) -> Option<String> {
        self.conn.query_row("SELECT v FROM meta WHERE k='model_id'", [], |r| r.get(0)).ok()
    }

    /// Stamp the producing model id (called once at the start of a build).
    pub fn set_model_id(&self, id: &str) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO meta(k,v) VALUES('model_id', ?)", params![id])?;
        Ok(())
    }

    /// Number of stored vectors.
    pub fn count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM vec", [], |r| r.get(0))?)
    }

    /// Drop every stored vector (used before a full rebuild). The caller
    /// re-stamps `model_id` via `set_model_id` right after.
    pub fn clear(&self) -> Result<()> {
        self.conn.execute("DELETE FROM vec", [])?;
        Ok(())
    }

    /// Insert/replace a batch of `(id, content_hash, vector)` rows in one
    /// transaction. Rejects a wrong-dimension vector loudly rather than
    /// storing a corrupt row - a bad row here would silently poison every
    /// future cosine comparison it takes part in.
    pub fn upsert_batch(&mut self, rows: &[(String, String, Vec<f32>)]) -> Result<()> {
        for (id, _, v) in rows {
            if v.len() != DIM {
                bail!("refusing to store id '{id}' with dim {} (expected {DIM})", v.len());
            }
        }
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare("INSERT OR REPLACE INTO vec(id, content_hash, v) VALUES(?,?,?)")?;
            for (id, hash, v) in rows {
                st.execute(params![id, hash, f32_to_blob(v)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every stored `(id, content_hash)` pair - used by `report` to compare
    /// against live content without paying to decode every vector blob.
    pub fn all_ids_and_hashes(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare("SELECT id, content_hash FROM vec")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Fetch the vectors for a candidate set of ids. An id with no stored
    /// vector, or a corrupt blob, is simply absent from the map, never an
    /// error - the caller (`lookup::search_best_effort`) treats an absent id
    /// as "no semantic opinion" and falls back to the text-only hits.
    pub fn get_many(&self, ids: &[String]) -> Result<HashMap<String, Vec<f32>>> {
        let mut out = HashMap::with_capacity(ids.len());
        let mut st = self.conn.prepare("SELECT v FROM vec WHERE id = ?")?;
        for id in ids {
            if let Ok(blob) = st.query_row(params![id], |r| r.get::<_, Vec<u8>>(0)) {
                if let Some(v) = blob_to_f32(&blob) {
                    out.insert(id.clone(), v);
                }
            }
        }
        Ok(out)
    }
}

/// Encode a vector as little-endian f32 bytes.
fn f32_to_blob(v: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(v.len() * 4);
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
    b
}

/// Decode little-endian f32 bytes. `None` on a length that is not a whole
/// number of f32s (a corrupt blob), so a bad row is skipped, never misread.
fn blob_to_f32(b: &[u8]) -> Option<Vec<f32>> {
    if b.is_empty() || !b.len().is_multiple_of(4) {
        return None;
    }
    Some(b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// A content fingerprint for staleness detection: two different texts almost
/// certainly hash differently, so a stored hash that no longer matches an
/// item's current text means the item was revised after it was embedded.
fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Rebuild the sidecar from scratch: every live item except `Lookup` - the
/// same population `lookup::search` itself covers (any project, archive
/// kinds `Report`/`Chunk` fully included; CONTRACT: "opzoeken is geen
/// injectie"). Always a clean, whole-store re-embedding, never a partial
/// patch, so a build's result never depends on what was there before.
pub fn build(store: &EventStore, model_dir: &Path, vectors_path: &Path) -> Result<usize> {
    let candidates: Vec<_> = live_items(store).into_iter().filter(|li| li.item.kind != Kind::Lookup).collect();
    let mut vs = VectorStore::open(vectors_path)?;
    vs.clear()?;
    vs.set_model_id(MODEL_ID)?;
    if candidates.is_empty() {
        return Ok(0);
    }
    let mut embedder = Embedder::load(model_dir)?;
    let texts: Vec<String> = candidates.iter().map(|li| li.item.text.clone()).collect();
    let vectors = embedder.embed_many(&texts)?;
    let rows: Vec<(String, String, Vec<f32>)> =
        candidates.into_iter().zip(vectors).map(|(li, v)| (li.id, content_hash(&li.item.text), v)).collect();
    let n = rows.len();
    vs.upsert_batch(&rows)?;
    Ok(n)
}

/// "Hoeveel er zijn en of ze bij de huidige inhoud passen" - the count-and-
/// freshness half of the `vectors` commands. Never loads the embedder: every
/// number here comes from comparing the sidecar's own stored ids/hashes
/// against `live_items`, so checking freshness never pays the model-load
/// cost `build`/`search_best_effort` do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorsReport {
    pub model_id_stored: Option<String>,
    pub model_id_expected: String,
    pub stored_count: i64,
    pub live_count: usize,
    /// A live, non-Lookup item with no stored vector at all yet.
    pub missing: usize,
    /// A live item whose stored vector's content hash no longer matches its
    /// current text - it was revised after the last build.
    pub stale: usize,
    /// A stored vector whose id is no longer a live item at all (retracted,
    /// diverged, or simply gone since the last build).
    pub orphaned: usize,
}

impl VectorsReport {
    /// Whether these vectors could be trusted for scoring right now. The
    /// same guard `lookup::search_best_effort` applies at query time: a
    /// model-id mismatch means the stored numbers came from a different
    /// embedding space entirely, so comparing them to a fresh query vector
    /// would silently produce meaningless scores dressed up as real ones.
    pub fn model_id_matches(&self) -> bool {
        self.model_id_stored.as_deref() == Some(self.model_id_expected.as_str())
    }
}

pub fn report(store: &EventStore, vectors_path: &Path) -> Result<VectorsReport> {
    let vs = VectorStore::open(vectors_path)?;
    let live: Vec<_> = live_items(store).into_iter().filter(|li| li.item.kind != Kind::Lookup).collect();
    let stored: HashMap<String, String> = vs.all_ids_and_hashes()?.into_iter().collect();
    let live_ids: std::collections::HashSet<&str> = live.iter().map(|li| li.id.as_str()).collect();

    let mut missing = 0usize;
    let mut stale = 0usize;
    for li in &live {
        match stored.get(&li.id) {
            None => missing += 1,
            Some(h) if *h != content_hash(&li.item.text) => stale += 1,
            _ => {}
        }
    }
    let orphaned = stored.keys().filter(|id| !live_ids.contains(id.as_str())).count();

    Ok(VectorsReport {
        model_id_stored: vs.model_id(),
        model_id_expected: MODEL_ID.to_string(),
        stored_count: vs.count()?,
        live_count: live.len(),
        missing,
        stale,
        orphaned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::Item;
    use model::store;

    fn v(fill: f32) -> Vec<f32> {
        vec![fill; DIM]
    }

    fn report_item(id: &str, kind: Kind, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind,
            text: text.to_string(),
            bindings: vec![],
            severity: None,
            project: None,
            tags: vec![],
            expires: if kind == Kind::Report { Some("2027-01-01".to_string()) } else { None },
            key: if kind == Kind::Lookup { Some(format!("{id}-key")) } else { None },
            falsifier: None,
            check: None,
        }
    }

    // ------------------------------------------------------- VectorStore CRUD

    #[test]
    fn roundtrip_and_count() {
        let dir = tempfile::tempdir().unwrap();
        let mut vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        assert_eq!(vs.count().unwrap(), 0);
        vs.set_model_id("m1").unwrap();
        assert_eq!(vs.model_id().as_deref(), Some("m1"));

        vs.upsert_batch(&[("a".to_string(), "hash-a".to_string(), v(0.5)), ("b".to_string(), "hash-b".to_string(), v(-0.25))])
            .unwrap();
        assert_eq!(vs.count().unwrap(), 2);

        let got = vs.get_many(&["a".to_string(), "b".to_string(), "missing".to_string()]).unwrap();
        assert_eq!(got.len(), 2, "a missing id is simply absent, not an error");
        assert!((got["a"][0] - 0.5).abs() < 1e-6);
        assert!((got["b"][7] + 0.25).abs() < 1e-6);
    }

    #[test]
    fn rejects_wrong_dim() {
        let dir = tempfile::tempdir().unwrap();
        let mut vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        let bad = vs.upsert_batch(&[("a".to_string(), "h".to_string(), vec![0.1, 0.2, 0.3])]);
        assert!(bad.is_err(), "a wrong-dimension vector must be rejected, not stored");
    }

    #[test]
    fn clear_resets_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let mut vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vs.upsert_batch(&[("a".to_string(), "h".to_string(), v(1.0))]).unwrap();
        vs.clear().unwrap();
        assert_eq!(vs.count().unwrap(), 0);
    }

    #[test]
    fn corrupt_blob_is_skipped_not_misread() {
        let dir = tempfile::tempdir().unwrap();
        let vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vs.conn.execute("INSERT INTO vec(id,content_hash,v) VALUES('a','h', ?)", params![vec![1u8, 2, 3]]).unwrap();
        let got = vs.get_many(&["a".to_string()]).unwrap();
        assert!(got.is_empty(), "a corrupt blob is skipped, never misread as a real vector");
    }

    // -------------------------------------------------------------- report()

    #[test]
    fn report_on_a_never_built_sidecar_counts_everything_missing() {
        // Named after the defect it prevents: a status check over a sidecar
        // that was never built must say "0 stored, N missing", never crash
        // or silently claim the vectors are fine.
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("r1", Kind::Report, "the bbq recipe lives here")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("never-built.db");

        let r = report(&db, &vectors_path).unwrap();
        assert_eq!(r.stored_count, 0);
        assert_eq!(r.live_count, 1);
        assert_eq!(r.missing, 1);
        assert_eq!(r.stale, 0);
        assert_eq!(r.orphaned, 0);
        assert_eq!(r.model_id_stored, None);
        assert!(!r.model_id_matches());
    }

    #[test]
    fn report_recognizes_a_stale_vector_after_the_text_changes() {
        // Named after the exact defect CONTRACT.md calls out: "een
        // verouderde vectorenset wordt herkend in plaats van stil verkeerde
        // antwoorden te geven". A vector stored for the OLD text of a live
        // id must be counted `stale` once the id's current text differs,
        // never silently treated as still valid.
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("r1", Kind::Report, "the original wording")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("v.db");
        {
            let mut vs = VectorStore::open(&vectors_path).unwrap();
            vs.set_model_id(MODEL_ID).unwrap();
            // Stamped with a hash of text that no longer matches what is
            // live now - simulating "embedded before the last revise".
            vs.upsert_batch(&[("r1".to_string(), content_hash("the original wording BEFORE a revise"), v(0.1))]).unwrap();
        }

        let r = report(&db, &vectors_path).unwrap();
        assert_eq!(r.stale, 1, "a hash mismatch against the live text must be counted stale");
        assert_eq!(r.missing, 0);
    }

    #[test]
    fn report_counts_an_orphaned_vector_whose_item_is_no_longer_live() {
        let db = EventStore::in_memory().unwrap(); // nothing declared at all
        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("v.db");
        {
            let mut vs = VectorStore::open(&vectors_path).unwrap();
            vs.set_model_id(MODEL_ID).unwrap();
            vs.upsert_batch(&[("gone".to_string(), content_hash("whatever"), v(0.2))]).unwrap();
        }

        let r = report(&db, &vectors_path).unwrap();
        assert_eq!(r.orphaned, 1, "a vector for an id that is not live must be counted orphaned");
        assert_eq!(r.live_count, 0);
    }

    #[test]
    fn report_flags_a_model_id_mismatch_plainly() {
        let db = EventStore::in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("v.db");
        {
            let vs = VectorStore::open(&vectors_path).unwrap();
            vs.set_model_id("some-other-model@v0").unwrap();
        }
        let r = report(&db, &vectors_path).unwrap();
        assert!(!r.model_id_matches(), "a foreign model_id must never read as matching");
        assert_eq!(r.model_id_stored.as_deref(), Some("some-other-model@v0"));
    }

    #[test]
    fn report_never_counts_a_lookup_item_as_live() {
        // Lookup is excluded from the semantic population the same way
        // `lookup::search` excludes it from text search - a Lookup answers
        // only its own key, never a ranked result.
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("l1", Kind::Lookup, "the release checklist lives in RELEASE.md"))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("v.db");
        let r = report(&db, &vectors_path).unwrap();
        assert_eq!(r.live_count, 0, "a Lookup item must never be counted in the semantic population");
    }
}
