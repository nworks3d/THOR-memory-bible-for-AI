//! Precomputed dense sidecar for semantic recall (feature `semantic`).
//!
//! A separate SQLite database `thor-vectors.db` sitting next to `thor.db`. It is
//! DERIVED, not authoritative: delete the file and recall silently degrades to
//! bm25. It holds one unit-norm vector per event seq plus the `model_id` that
//! produced them; a model mismatch means the vectors are stale and must be
//! rebuilt from scratch (`thor vectors build`).
//!
//! Keeping it OUT of the append-only `thor.db` preserves that store's purity (it
//! stays a pure hash-chained log) and makes the vectors trivially rebuildable and
//! deletable without ever touching the source of truth.

use crate::embed::DIM;
use anyhow::{bail, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The dense sidecar. Owns its own SQLite connection.
pub struct VectorStore {
    conn: Connection,
}

impl VectorStore {
    /// Open (creating if absent) the sidecar at `path`, ensuring the schema.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL so a per-prompt courier READ never blocks behind a `thor vectors
        // build` WRITE (rollback-journal mode would stall the reader up to the
        // busy_timeout). synchronous=NORMAL is enough: the sidecar is derived and
        // rebuildable, so we don't need FULL-fsync durability like the main store.
        conn.execute_batch("PRAGMA journal_mode = WAL")?;
        conn.execute_batch("PRAGMA synchronous = NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS vec(seq INTEGER PRIMARY KEY, v BLOB NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    /// The `model_id` that produced the stored vectors, if any were ever written.
    pub fn model_id(&self) -> Option<String> {
        self.conn
            .query_row("SELECT v FROM meta WHERE k='model_id'", [], |r| r.get(0))
            .ok()
    }

    /// Stamp the producing model id (called once at the start of a full build).
    pub fn set_model_id(&self, id: &str) -> Result<()> {
        self.conn
            .execute("INSERT OR REPLACE INTO meta(k,v) VALUES('model_id', ?)", params![id])?;
        Ok(())
    }

    /// The highest seq that already has a vector (0 when empty) - the cursor for
    /// incremental `sync` (only events past this need embedding).
    pub fn max_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(seq),0) FROM vec", [], |r| r.get(0))?)
    }

    /// Number of stored vectors.
    pub fn count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM vec", [], |r| r.get(0))?)
    }

    /// Drop all vectors (used before a full rebuild). The `model_id` is reset by
    /// the caller via `set_model_id`.
    pub fn clear(&self) -> Result<()> {
        self.conn.execute("DELETE FROM vec", [])?;
        Ok(())
    }

    /// Insert/replace a batch of (seq, vector) rows in one transaction. Rejects a
    /// wrong-dimension vector loudly rather than storing a corrupt row.
    pub fn upsert_batch(&mut self, rows: &[(i64, Vec<f32>)]) -> Result<()> {
        for (seq, v) in rows {
            if v.len() != DIM {
                bail!("refusing to store seq {} with dim {} (expected {})", seq, v.len(), DIM);
            }
        }
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare("INSERT OR REPLACE INTO vec(seq,v) VALUES(?,?)")?;
            for (seq, v) in rows {
                st.execute(params![seq, f32_to_blob(v)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Load EVERY stored vector as (seq, vector). Used for true dense retrieval:
    /// the fusion caller brute-forces the query cosine over all of them to reach
    /// paraphrase golds that have no lexical overlap (so are absent from the bm25
    /// pool). At personal-memory scale (thousands to tens of thousands of facts)
    /// a full scan is a few milliseconds; an ANN index is only worth it far beyond
    /// that. Corrupt rows are skipped, never misread.
    pub fn all_vectors(&self) -> Result<Vec<(i64, Vec<f32>)>> {
        let mut st = self.conn.prepare("SELECT seq, v FROM vec")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, blob) = row?;
            if let Some(v) = blob_to_f32(&blob) {
                out.push((seq, v));
            }
        }
        Ok(out)
    }

    /// Fetch the vectors for a candidate set (the bm25 pool). Seqs with no stored
    /// vector are simply absent from the map; the fusion caller then treats them
    /// as cosine 0 (bm25-only), so a partially-built sidecar still works.
    pub fn get_many(&self, seqs: &[i64]) -> Result<HashMap<i64, Vec<f32>>> {
        let mut out = HashMap::with_capacity(seqs.len());
        let mut st = self.conn.prepare("SELECT v FROM vec WHERE seq=?")?;
        for &seq in seqs {
            if let Ok(blob) = st.query_row(params![seq], |r| r.get::<_, Vec<u8>>(0)) {
                if let Some(v) = blob_to_f32(&blob) {
                    out.insert(seq, v);
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

/// Decode little-endian f32 bytes. Returns None on a length that is not a whole
/// number of f32s (a corrupt blob), so a bad row is skipped, never misread.
fn blob_to_f32(b: &[u8]) -> Option<Vec<f32>> {
    if b.is_empty() || !b.len().is_multiple_of(4) {
        return None;
    }
    Some(
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// The sidecar path derived from the main store path: `thor-vectors.db` alongside
/// `thor.db`.
pub fn default_vectors_path(db: &Path) -> PathBuf {
    db.with_file_name("thor-vectors.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(fill: f32) -> Vec<f32> {
        vec![fill; DIM]
    }

    #[test]
    fn test_roundtrip_and_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let mut vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        assert_eq!(vs.max_seq().unwrap(), 0);
        assert_eq!(vs.count().unwrap(), 0);
        vs.set_model_id("m1").unwrap();
        assert_eq!(vs.model_id().as_deref(), Some("m1"));

        vs.upsert_batch(&[(1, v(0.5)), (2, v(-0.25))]).unwrap();
        assert_eq!(vs.max_seq().unwrap(), 2);
        assert_eq!(vs.count().unwrap(), 2);

        let got = vs.get_many(&[1, 2, 999]).unwrap();
        assert_eq!(got.len(), 2, "missing seq is simply absent, not an error");
        assert!((got[&1][0] - 0.5).abs() < 1e-6);
        assert!((got[&2][7] + 0.25).abs() < 1e-6);
    }

    #[test]
    fn test_rejects_wrong_dim() {
        let dir = tempfile::tempdir().unwrap();
        let mut vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        let bad = vs.upsert_batch(&[(1, vec![0.1, 0.2, 0.3])]);
        assert!(bad.is_err(), "a wrong-dimension vector must be rejected, not stored");
    }

    #[test]
    fn test_clear_and_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let mut vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vs.upsert_batch(&[(1, v(1.0))]).unwrap();
        vs.clear().unwrap();
        assert_eq!(vs.count().unwrap(), 0);
        assert_eq!(vs.max_seq().unwrap(), 0, "cursor resets after a clear");
    }

    #[test]
    fn test_corrupt_blob_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        // write a deliberately malformed blob (not a whole number of f32s)
        vs.conn
            .execute("INSERT INTO vec(seq,v) VALUES(1, ?)", params![vec![1u8, 2, 3]])
            .unwrap();
        let got = vs.get_many(&[1]).unwrap();
        assert!(got.is_empty(), "a corrupt blob is skipped, never misread");
    }
}
