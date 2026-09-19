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
use crate::live::{live_items, LiveItem};
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
            // `part` is which piece of the item's text this vector covers, 0
            // for the first. One row per part instead of one per item: see
            // `get_many_best` for the defect that change closes. An older
            // sidecar has no `part` column at all, so it is simply rebuilt -
            // this file is derived and always rebuildable, which is exactly
            // why it may carry a schema that moves.
            "CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS vec(id TEXT NOT NULL, part INTEGER NOT NULL DEFAULT 0, content_hash TEXT NOT NULL, v BLOB NOT NULL, PRIMARY KEY (id, part));",
        )?;

        // A sidecar written before parts existed still has the one-row-per-id
        // table, and `CREATE TABLE IF NOT EXISTS` leaves it exactly as it is -
        // so the first write would fail with "no column named part". Drop and
        // recreate rather than ALTER: this file is derived and rebuilt from the
        // log in one command, so throwing it away costs a rebuild and nothing
        // else, while a half-migrated sidecar would keep answering with vectors
        // whose provenance nobody can state.
        let has_part: bool = conn
            .prepare("PRAGMA table_info(vec)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .flatten()
            .any(|name| name == "part");
        if !has_part {
            conn.execute_batch(
                "DROP TABLE vec;
                 CREATE TABLE vec(id TEXT NOT NULL, part INTEGER NOT NULL DEFAULT 0, content_hash TEXT NOT NULL, v BLOB NOT NULL, PRIMARY KEY (id, part));
                 DELETE FROM meta WHERE k='model_id';",
            )?;
        }
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
        let parts: Vec<(String, usize, String, Vec<f32>)> =
            rows.iter().map(|(id, h, v)| (id.clone(), 0usize, h.clone(), v.clone())).collect();
        self.upsert_parts(&parts)
    }

    /// Insert/replace vectors that may cover SEVERAL parts of one item. Every
    /// part of an id must be written in the same call: the id's old rows are
    /// cleared first, so a rebuild can never leave a stale part behind that a
    /// later query would still score against.
    pub fn upsert_parts(&mut self, rows: &[(String, usize, String, Vec<f32>)]) -> Result<()> {
        for (id, _, _, v) in rows {
            if v.len() != DIM {
                bail!("refusing to store id '{id}' with dim {} (expected {DIM})", v.len());
            }
        }
        let tx = self.conn.transaction()?;
        {
            let mut del = tx.prepare("DELETE FROM vec WHERE id = ?")?;
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for (id, _, _, _) in rows {
                if seen.insert(id.as_str()) {
                    del.execute(params![id])?;
                }
            }
            let mut st = tx.prepare("INSERT OR REPLACE INTO vec(id, part, content_hash, v) VALUES(?,?,?,?)")?;
            for (id, part, hash, v) in rows {
                st.execute(params![id, *part as i64, hash, f32_to_blob(v)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete every stored row (every part) for each id in `ids` - how
    /// `refresh` drops a vector whose item is no longer live. An id with no
    /// stored row at all is simply a no-op, never an error.
    pub fn delete_ids(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            self.conn.execute("DELETE FROM vec WHERE id = ?", params![id])?;
        }
        Ok(())
    }

    /// Every stored `(id, content_hash)` pair - used by `report` to compare
    /// against live content without paying to decode every vector blob.
    /// One row per ITEM, never per part: every part of an item shares the same
    /// content hash, so counting parts here would report an item as several
    /// and make `report`'s stale/missing arithmetic nonsense.
    pub fn all_ids_and_hashes(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare("SELECT id, content_hash FROM vec WHERE part = 0")?;
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
    /// For each id, the vector of the part that best matches `query_vec`.
    ///
    /// THE DEFECT THIS CLOSES, measured 2026-08-16 on the owner's own store.
    /// One vector per item averages a whole document into a point that is close
    /// to nothing in particular, and the longer the item the worse it gets. A
    /// stored pizza recipe scored 0.490 against a question about its own
    /// hydration - just under the 0.50 floor, so the answer came back empty.
    /// Its best paragraph scored 0.628. A research report scored 0.287 whole
    /// and 0.509 by its best paragraph. Short items do not move at all, which
    /// is what makes this safe: it can only raise a score, never lower one.
    ///
    /// Returning the best PART's vector rather than a score keeps every caller
    /// downstream unchanged - the ranking core still works on one vector per
    /// EVERY stored part of each id, in part order - the whole vector set an
    /// item carries, not one chosen chunk.
    ///
    /// WHY THIS EXISTS (2026-09-19). `get_many_best` above needs the query
    /// vector to pick a chunk, so a caller that scores MANY queries against
    /// the same store either re-reads the sidecar once per query or scores
    /// the wrong chunk. The recall harness took the second road by calling
    /// `get_many` (part 0 only) once for the whole battery, which silently
    /// measured first-chunk similarity while live measures best-chunk - a
    /// divergence at the data layer, in a file whose own header warns about
    /// exactly that class of divergence in the ranking layer. With this, a
    /// batch caller reads the parts once and applies live's own max-over-
    /// chunks rule itself, per query, with no second trip to the database.
    pub fn get_many_parts(&self, ids: &[String]) -> Result<HashMap<String, Vec<Vec<f32>>>> {
        let mut out = HashMap::with_capacity(ids.len());
        let mut st = self.conn.prepare("SELECT v FROM vec WHERE id = ? ORDER BY part")?;
        for id in ids {
            let Ok(rows) = st.query_map(params![id], |r| r.get::<_, Vec<u8>>(0)) else { continue };
            let parts: Vec<Vec<f32>> = rows.flatten().filter_map(|blob| blob_to_f32(&blob)).collect();
            if !parts.is_empty() {
                out.insert(id.clone(), parts);
            }
        }
        Ok(out)
    }

    /// id, and never learns that parts exist.
    pub fn get_many_best(&self, ids: &[String], query_vec: &[f32]) -> Result<HashMap<String, Vec<f32>>> {
        let mut out = HashMap::with_capacity(ids.len());
        let mut st = self.conn.prepare("SELECT v FROM vec WHERE id = ? ORDER BY part")?;
        for id in ids {
            let Ok(rows) = st.query_map(params![id], |r| r.get::<_, Vec<u8>>(0)) else { continue };
            let mut best: Option<(f32, Vec<f32>)> = None;
            for blob in rows.flatten() {
                let Some(v) = blob_to_f32(&blob) else { continue };
                if v.len() != query_vec.len() {
                    continue;
                }
                let score: f32 = v.iter().zip(query_vec).map(|(a, b)| a * b).sum();
                if best.as_ref().is_none_or(|(b, _)| score > *b) {
                    best = Some((score, v));
                }
            }
            if let Some((_, v)) = best {
                out.insert(id.clone(), v);
            }
        }
        Ok(out)
    }

    pub fn get_many(&self, ids: &[String]) -> Result<HashMap<String, Vec<f32>>> {
        let mut out = HashMap::with_capacity(ids.len());
        let mut st = self.conn.prepare("SELECT v FROM vec WHERE id = ? AND part = 0")?;
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

/// Turn a batch of live items into the flat `(id, part, hash, vector)` rows
/// `upsert_parts` expects. THE ONE PLACE `build` and `refresh` both chunk and
/// embed an item, so the two can never silently drift apart on how one is
/// turned into vectors - a refreshed item that embedded even slightly
/// differently from a rebuilt one would be exactly the kind of split-brain
/// this file's own top-of-file doc comment warns a STALE vector already is,
/// except self-inflicted by this crate rather than caused by a revise.
///
/// One vector per PART, not per item. Two defects close together here: the
/// embedder truncates at 1000 characters, so everything past that in a long
/// item never reached the model at all; and a single vector for a whole
/// document averages every subject in it into a point near none of them.
/// Measured 2026-08-16 - see `VectorStore::get_many_best` for the numbers.
///
/// A short item yields exactly one part, so its stored vector is bit for bit
/// what it was before parts existed. That is what keeps the ranking that was
/// tuned on short, identifier-shaped items from moving underneath it.
///
/// PART 0 IS ALWAYS THE WHOLE ITEM, and the pieces follow from 1. That is not
/// tidiness: a binary built before parts existed reads this sidecar with a
/// plain "give me this id's vector" and takes what comes back first. If part
/// 0 were the first paragraph, every older binary reading a rebuilt sidecar
/// would silently start scoring questions against opening lines only - a live
/// behaviour change nobody asked for, from a data migration. With the whole
/// item at 0, an older binary behaves exactly as it always did, and only a
/// binary that knows about parts sees the improvement.
fn embed_rows(embedder: &mut Embedder, items: &[&LiveItem]) -> Result<Vec<(String, usize, String, Vec<f32>)>> {
    let mut flat_ids: Vec<(String, usize, String)> = Vec::new();
    let mut flat_texts: Vec<String> = Vec::new();
    for li in items {
        let hash = content_hash(&li.item.text);
        flat_ids.push((li.id.clone(), 0usize, hash.clone()));
        flat_texts.push(li.item.text.clone());

        let pieces = split_for_embedding(&li.item.text);
        if pieces.len() > 1 {
            for (i, part) in pieces.into_iter().enumerate() {
                flat_ids.push((li.id.clone(), i + 1, hash.clone()));
                flat_texts.push(part);
            }
        }
    }
    let vectors = embedder.embed_many(&flat_texts)?;
    Ok(flat_ids.into_iter().zip(vectors).map(|((id, part, hash), v)| (id, part, hash, v)).collect())
}

/// Rebuild the sidecar from scratch: every live item except `Lookup` - the
/// same population `lookup::search` itself covers (any project, archive
/// kinds `Report`/`Chunk` fully included; CONTRACT: "opzoeken is geen
/// injectie"). Always a clean, whole-store re-embedding, never a partial
/// patch, so a build's result never depends on what was there before. See
/// `refresh` below for the incremental complement that keeps a store fresh
/// in between full rebuilds.
pub fn build(store: &EventStore, model_dir: &Path, vectors_path: &Path) -> Result<usize> {
    let candidates: Vec<_> = live_items(store).into_iter().filter(|li| li.item.kind != Kind::Lookup).collect();
    let mut vs = VectorStore::open(vectors_path)?;
    vs.clear()?;
    vs.set_model_id(MODEL_ID)?;
    if candidates.is_empty() {
        return Ok(0);
    }
    let mut embedder = Embedder::load(model_dir)?;
    let refs: Vec<&LiveItem> = candidates.iter().collect();
    let rows = embed_rows(&mut embedder, &refs)?;
    let n = candidates.len();
    vs.upsert_parts(&rows)?;
    Ok(n)
}

/// What one `refresh` call actually did. `build` returns a plain count
/// because it always does the same kind of work (rebuild everything);
/// `refresh` needs to tell "embedded N" apart from "there was nothing to do"
/// apart from "refused outright", so a struct beats a single number here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RefreshOutcome {
    /// Items actually embedded this call: missing or stale, up to `budget`.
    /// Never counts a part - the same "one row per ITEM" unit `report`'s own
    /// `missing`/`stale` counts use.
    pub embedded: usize,
    /// Vectors deleted because their id is no longer a live item. Never
    /// budgeted - see `refresh`'s own doc comment for why that is cheap
    /// enough to always do in full.
    pub deleted: usize,
    /// How many more items still needed embedding when this call's budget
    /// ran out. 0 whenever every missing/stale item got embedded this call.
    pub remaining: usize,
    /// Whether this call had to stamp `model_id` - true exactly when the
    /// sidecar had none yet and this call just wrote its first real vector.
    pub model_id_written: bool,
    /// True when the sidecar's stored `model_id` did not match this binary's
    /// own: refresh refused to touch the store at all, and every other field
    /// above is 0/false. The fix is always the same regardless of how far out
    /// of date the sidecar is: run `vectors-build`.
    pub model_id_mismatch: bool,
}

/// Embed only what changed since the store was last built or refreshed, and
/// drop vectors whose id is no longer live - the incremental complement to
/// `build`'s full from-scratch rebuild, so a fact written an hour ago does
/// not sit unfindable by meaning until a human remembers to run
/// `vectors-build`.
///
/// SAME WRITE PATH AS `build`, ON PURPOSE: both call `embed_rows` (this
/// file's one chunk-and-embed routine) and the same `upsert_parts`, so an
/// item refreshed here and the same item rebuilt from scratch produce BIT
/// IDENTICAL stored rows - proven by this file's own equivalence test, which
/// compares the actual float rows, not just which ids exist.
///
/// REFUSES OUTRIGHT on a model_id mismatch, checked before a single live item
/// is even read - the same "cheap, before the embedder ever loads" guard
/// `lookup::search_best_effort_cached` applies at query time (see its own doc
/// comment). On the WRITE side refusing is the only safe move: `build` fixes
/// a mismatch by clearing everything first, but `refresh` only ever adds or
/// drops individual rows, so writing even one new vector next to old foreign
/// ones would leave the sidecar half in one embedding space and half in
/// another, with nothing left on the row itself to tell them apart
/// afterwards. `report`'s own `model_id_matches` is the read-side version of
/// this same stance.
///
/// `budget` caps how many ITEMS (never a part) this call embeds; 0 means
/// unlimited. Orphan deletion always runs in full, unbudgeted - deleting a
/// row is one cheap statement, not a roughly-a-second model call, so there is
/// nothing there worth spreading across runs.
pub fn refresh(store: &EventStore, model_dir: &Path, vectors_path: &Path, budget: usize) -> Result<RefreshOutcome> {
    let mut vs = VectorStore::open(vectors_path)?;
    let stored_model_id = vs.model_id();
    if let Some(id) = &stored_model_id {
        if id != MODEL_ID {
            return Ok(RefreshOutcome { model_id_mismatch: true, ..Default::default() });
        }
    }

    let live: Vec<_> = live_items(store).into_iter().filter(|li| li.item.kind != Kind::Lookup).collect();
    let live_ids: std::collections::HashSet<&str> = live.iter().map(|li| li.id.as_str()).collect();
    let stored: HashMap<String, String> = vs.all_ids_and_hashes()?.into_iter().collect();

    let orphan_ids: Vec<String> = stored.keys().filter(|id| !live_ids.contains(id.as_str())).cloned().collect();
    vs.delete_ids(&orphan_ids)?;
    let deleted = orphan_ids.len();

    let needs: Vec<&LiveItem> = live
        .iter()
        .filter(|li| match stored.get(&li.id) {
            None => true,
            Some(h) => *h != content_hash(&li.item.text),
        })
        .collect();
    let total_needed = needs.len();
    let take = if budget == 0 { total_needed } else { budget.min(total_needed) };
    let to_embed = &needs[..take];
    let remaining = total_needed - take;

    let mut model_id_written = false;
    if !to_embed.is_empty() {
        if stored_model_id.is_none() {
            vs.set_model_id(MODEL_ID)?;
            model_id_written = true;
        }
        let mut embedder = Embedder::load(model_dir)?;
        let rows = embed_rows(&mut embedder, to_embed)?;
        vs.upsert_parts(&rows)?;
    }

    Ok(RefreshOutcome { embedded: to_embed.len(), deleted, remaining, model_id_written, model_id_mismatch: false })
}

/// The most a single part may carry, comfortably under the embedder's own
/// 1000-character truncation so a part is never cut in half by it.
const PART_CHARS: usize = 700;

/// Split an item's text into pieces small enough to be embedded whole.
///
/// Paragraphs first, because a paragraph is where one subject lives. A
/// paragraph longer than the bound is cut on the last sentence end that fits,
/// and only failing that on the bound itself - a piece that ends mid-sentence
/// still embeds, it just carries less. Short items come back as one piece,
/// unchanged.
pub fn split_for_embedding(text: &str) -> Vec<String> {
    if text.chars().count() <= PART_CHARS {
        return vec![text.to_string()];
    }
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if current.chars().count() + para.chars().count() <= PART_CHARS {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
            continue;
        }
        if !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        // A single paragraph over the bound: cut it into sentence-sized pieces.
        let mut rest: Vec<char> = para.chars().collect();
        while rest.len() > PART_CHARS {
            // Prefer a sentence end, then any word boundary, and only cut
            // blind if the text offers neither. Cutting on the bound alone
            // splits a word in half and feeds the model a fragment that means
            // nothing - caught by `splitting_keeps_every_word`, which counted
            // one word more coming out than went in.
            // A cut of 0 would drain nothing and spin here forever - a text
            // beginning with whitespace is enough to trigger it, and a build
            // that hangs is worse than one that splits a word. Never below 1,
            // and fall back to the bound when the boundary found is useless.
            let cut = rest[..PART_CHARS]
                .windows(2)
                .rposition(|w| w[0] == '.' && w[1].is_whitespace())
                .map(|i| i + 1)
                .or_else(|| rest[..PART_CHARS].iter().rposition(|c| c.is_whitespace()))
                .filter(|c| *c > 0)
                .unwrap_or(PART_CHARS);
            let head: String = rest[..cut].iter().collect();
            parts.push(head.trim().to_string());
            rest.drain(..cut);
        }
        current = rest.iter().collect::<String>().trim().to_string();
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        parts.push(text.to_string());
    }
    parts
}

#[cfg(test)]
mod split_tests {
    use super::*;

    /// The property that keeps this change from moving anything that was
    /// already tuned: a short item is still exactly one vector, byte for byte
    /// what it was before parts existed.
    #[test]
    fn a_short_item_is_still_one_part() {
        let text = "MIN_SIMILARITY is 0.50, raised from 0.45 on 2026-08-05.";
        assert_eq!(split_for_embedding(text), vec![text.to_string()]);
    }

    /// THE DEFECT THIS PREVENTS, measured 2026-08-16: the embedder truncates at
    /// 1000 characters, so everything past that in a long item never reached
    /// the model. Every part must therefore stay under that bound.
    #[test]
    fn no_part_is_long_enough_to_be_truncated_by_the_embedder() {
        let para = "Een alinea over hydratatie en bloem. ".repeat(40);
        let text = format!("{para}\n\n{para}\n\n{para}");
        let parts = split_for_embedding(&text);
        assert!(parts.len() > 1, "a long item must be split at all");
        for p in &parts {
            assert!(p.chars().count() <= PART_CHARS, "a part of {} chars would be cut", p.chars().count());
        }
    }

    /// THE HANG THIS PREVENTS, hit on the real store 2026-08-16: when the only
    /// boundary inside the bound sits at position 0, the cut consumed nothing
    /// and the loop spun forever. A build that never returns is worse than one
    /// that splits a word, so a cut is never allowed to be zero.
    #[test]
    fn a_boundary_at_the_very_start_cannot_stall_the_split() {
        let text = format!(" {}", "x".repeat(PART_CHARS * 3));
        let parts = split_for_embedding(&text);
        assert!(!parts.is_empty());
        for p in &parts {
            assert!(p.chars().count() <= PART_CHARS);
        }
    }

    /// Nothing may be dropped on the floor: every word of the item has to end
    /// up in some part, or the search would go quiet about content that is
    /// really there - the exact failure this whole change exists to fix.
    #[test]
    fn splitting_keeps_every_word() {
        let text = format!("{}\n\n{}\n\n{}", "eerste ".repeat(200), "tweede ".repeat(200), "derde ".repeat(200));
        let parts = split_for_embedding(&text);
        let joined: String = parts.join(" ");
        for woord in ["eerste", "tweede", "derde"] {
            assert!(joined.contains(woord), "{woord} disappeared while splitting");
        }
        let orig = text.split_whitespace().count();
        let after = joined.split_whitespace().count();
        assert_eq!(orig, after, "splitting must not lose or duplicate a word");
    }
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
    use std::path::PathBuf;

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
            project: (!kind.can_fire()).then(|| "test-project".to_string()),
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

    #[test]
    fn delete_ids_removes_only_the_named_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vs.upsert_batch(&[("a".to_string(), "ha".to_string(), v(0.1)), ("b".to_string(), "hb".to_string(), v(0.2))]).unwrap();
        vs.delete_ids(&["a".to_string()]).unwrap();
        assert_eq!(vs.count().unwrap(), 1);
        assert!(vs.get_many(&["a".to_string()]).unwrap().is_empty());
        assert!(!vs.get_many(&["b".to_string()]).unwrap().is_empty());
    }

    #[test]
    fn delete_ids_on_an_absent_id_is_a_no_op_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let vs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        assert!(vs.delete_ids(&["nope".to_string()]).is_ok());
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

    // ------------------------------------------------------------- refresh()

    /// The real per-user semantic model, when this machine actually has one.
    /// Every `refresh` test below that needs to embed something for real
    /// (there is no seam to fake `Embedder::load`) calls this first and skips
    /// itself when it comes back `None` - the same "a missing model is a
    /// normal, supported setup, never a failure" stance this whole feature
    /// takes everywhere else (see `semantic_paths::SearchMode::ModelMissing`).
    fn require_model() -> Option<PathBuf> {
        let dir = crate::semantic_paths::default_model_dir()?;
        crate::semantic_paths::model_present(&dir).then_some(dir)
    }

    /// Every stored part's vector for one id, in part order. `get_many`/
    /// `get_many_best` each deliberately return only ONE vector per id, so a
    /// test that must compare a multi-part item's rows bit for bit reads the
    /// table directly, the same way `corrupt_blob_is_skipped_not_misread`
    /// above already does.
    fn all_parts(vs: &VectorStore, id: &str) -> Vec<Vec<f32>> {
        let mut st = vs.conn.prepare("SELECT v FROM vec WHERE id = ? ORDER BY part").unwrap();
        st.query_map(params![id], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|b| blob_to_f32(&b.unwrap()).expect("a freshly-written blob must decode"))
            .collect()
    }

    /// THE DEFECT THIS PREVENTS: two independent write paths (a full rebuild
    /// and an incremental refresh) that quietly embedded the same text
    /// differently would be a split-brain no `report` count could ever catch
    /// - a search's ranking would silently depend on WHICH path happened to
    /// write an item last.
    #[test]
    fn refresh_embeds_missing_and_stale_but_leaves_unchanged_alone() {
        let Some(model_dir) = require_model() else {
            eprintln!("skipping refresh_embeds_missing_and_stale_but_leaves_unchanged_alone: no per-user semantic model present");
            return;
        };
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("missing", Kind::Report, "the espresso machine needs descaling every month"))
            .unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("changed", Kind::Report, "the bicycle tire needs replacing before winter"))
            .unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("unchanged", Kind::Report, "the garden hose has a slow leak near the nozzle"))
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("v.db");
        {
            let mut vs = VectorStore::open(&vectors_path).unwrap();
            vs.set_model_id(MODEL_ID).unwrap();
            // Stamped under the OLD text - simulates "embedded before a revise".
            vs.upsert_batch(&[("changed".to_string(), content_hash("the bicycle tire needs air before winter"), v(0.111))]).unwrap();
            vs.upsert_batch(&[("unchanged".to_string(), content_hash("the garden hose has a slow leak near the nozzle"), v(0.222))])
                .unwrap();
        }

        let outcome = refresh(&db, &model_dir, &vectors_path, 0).unwrap();
        assert_eq!(outcome.embedded, 2, "missing + changed, never the unchanged one");
        assert_eq!(outcome.deleted, 0);
        assert_eq!(outcome.remaining, 0);
        assert!(!outcome.model_id_mismatch);

        let vs = VectorStore::open(&vectors_path).unwrap();
        let stored: HashMap<String, String> = vs.all_ids_and_hashes().unwrap().into_iter().collect();
        assert_eq!(stored.get("missing").cloned(), Some(content_hash("the espresso machine needs descaling every month")));
        assert_eq!(stored.get("changed").cloned(), Some(content_hash("the bicycle tire needs replacing before winter")));
        assert_eq!(stored.get("unchanged").cloned(), Some(content_hash("the garden hose has a slow leak near the nozzle")));

        // The untouched row must be BYTE IDENTICAL, not merely hash-equal -
        // proof refresh never rewrites something it had no reason to touch.
        assert_eq!(all_parts(&vs, "unchanged"), vec![v(0.222)], "an unchanged item's stored vector must not move at all");
    }

    /// THE DEFECT THIS PREVENTS: an id that was retracted, diverged away, or
    /// otherwise stopped being live must not go on answering searches forever
    /// just because nobody has run a full rebuild since it left.
    #[test]
    fn refresh_deletes_the_vector_of_an_id_that_is_no_longer_live() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("still-here", Kind::Report, "a fact that is still live")).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("v.db");
        {
            let mut vs = VectorStore::open(&vectors_path).unwrap();
            vs.set_model_id(MODEL_ID).unwrap();
            vs.upsert_batch(&[("still-here".to_string(), content_hash("a fact that is still live"), v(0.5))]).unwrap();
            vs.upsert_batch(&[("gone".to_string(), content_hash("whatever it used to say"), v(0.25))]).unwrap();
        }

        // Deliberately bogus: the one live item is already current, so a
        // correct refresh never has a reason to load an embedder at all -
        // keeping this test hermetic and fast on purpose.
        let bogus_model_dir = dir.path().join("no-such-model");
        let outcome = refresh(&db, &bogus_model_dir, &vectors_path, 0).unwrap();

        assert_eq!(outcome.deleted, 1);
        assert_eq!(outcome.embedded, 0, "the only live item was already current - the embedder must never load");

        let vs = VectorStore::open(&vectors_path).unwrap();
        let stored: HashMap<String, String> = vs.all_ids_and_hashes().unwrap().into_iter().collect();
        assert!(!stored.contains_key("gone"), "the orphaned vector must be gone");
        assert!(stored.contains_key("still-here"), "the still-live item's vector must remain");
    }

    /// THE DEFECT THIS PREVENTS: a first catch-up over hundreds of missing
    /// items must never turn one hourly run into an unbounded one - `budget`
    /// is how `ops::sync`'s hourly job stays short, and this pins that a
    /// budget of N really does stop at N, leaving the rest for next time.
    #[test]
    fn refresh_respects_the_budget() {
        let Some(model_dir) = require_model() else {
            eprintln!("skipping refresh_respects_the_budget: no per-user semantic model present");
            return;
        };
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("a1", Kind::Report, "the espresso machine needs descaling every month"))
            .unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("a2", Kind::Report, "the bicycle tire needs replacing before winter"))
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("v.db");

        let outcome = refresh(&db, &model_dir, &vectors_path, 1).unwrap();
        assert_eq!(outcome.embedded, 1, "a budget of 1 must embed exactly one item");
        assert_eq!(outcome.remaining, 1, "the other missing item must be counted remaining");
        assert!(outcome.model_id_written, "the first ever write to a fresh sidecar must stamp its model_id");

        let vs = VectorStore::open(&vectors_path).unwrap();
        assert_eq!(vs.all_ids_and_hashes().unwrap().len(), 1);
    }

    /// THE MIXING THIS REFUSAL PREVENTS: a sidecar stamped by a different
    /// model must never gain even one new vector from this binary - doing so
    /// would leave some rows in one embedding space and some in another with
    /// nothing on the row itself to tell them apart afterwards. The write-side
    /// twin of `report_flags_a_model_id_mismatch_plainly` above.
    #[test]
    fn refresh_refuses_a_mismatched_model_id_without_writing_anything() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("r1", Kind::Report, "a fact that would need embedding")).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let vectors_path = dir.path().join("v.db");
        {
            let vs = VectorStore::open(&vectors_path).unwrap();
            vs.set_model_id("some-other-model@v0").unwrap();
        }

        // Bogus on purpose: a refusal must never even try to load it.
        let bogus_model_dir = dir.path().join("no-such-model");
        let outcome = refresh(&db, &bogus_model_dir, &vectors_path, 0).unwrap();

        assert!(outcome.model_id_mismatch);
        assert_eq!(outcome.embedded, 0);
        assert_eq!(outcome.deleted, 0);
        assert_eq!(outcome.remaining, 0);
        assert!(!outcome.model_id_written);

        let vs = VectorStore::open(&vectors_path).unwrap();
        assert_eq!(vs.model_id().as_deref(), Some("some-other-model@v0"), "refresh must never overwrite a foreign model_id");
        assert_eq!(vs.count().unwrap(), 0, "refresh must not write anything at all when it refuses");
    }

    /// THE EQUIVALENCE PROOF: `build` and `refresh` must write BIT IDENTICAL
    /// rows for the same item, or a search's ranking would silently depend on
    /// which of the two write paths happened to touch an id last - exactly
    /// the split-brain `embed_rows`'s own doc comment names. Covers a short
    /// item (one part) and a long one (several parts), so both of
    /// `embed_rows`'s branches are compared, not just the easy case.
    #[test]
    fn refresh_from_empty_matches_build_from_scratch_bit_for_bit() {
        let Some(model_dir) = require_model() else {
            eprintln!("skipping refresh_from_empty_matches_build_from_scratch_bit_for_bit: no per-user semantic model present");
            return;
        };
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report_item("short", Kind::Report, "a short fact about the bbq recipe")).unwrap();
        let long_text = format!("{}\n\n{}\n\n{}", "first paragraph ".repeat(200), "second paragraph ".repeat(200), "third paragraph ".repeat(200));
        store::declare(&mut db, "s", "l", "a", &report_item("long", Kind::Report, &long_text)).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let built_path = dir.path().join("built.db");
        let refreshed_path = dir.path().join("refreshed.db");

        let n = build(&db, &model_dir, &built_path).unwrap();
        assert_eq!(n, 2);
        let outcome = refresh(&db, &model_dir, &refreshed_path, 0).unwrap();
        assert_eq!(outcome.embedded, 2);

        let built = VectorStore::open(&built_path).unwrap();
        let refreshed = VectorStore::open(&refreshed_path).unwrap();
        let mut built_rows = built.all_ids_and_hashes().unwrap();
        let mut refreshed_rows = refreshed.all_ids_and_hashes().unwrap();
        built_rows.sort();
        refreshed_rows.sort();
        assert_eq!(built_rows, refreshed_rows, "the same ids with the same content hashes must come out of both paths");

        for id in ["short", "long"] {
            let built_vecs = all_parts(&built, id);
            let refreshed_vecs = all_parts(&refreshed, id);
            assert!(!built_vecs.is_empty(), "id {id}: build must have stored at least one part");
            assert_eq!(built_vecs.len(), refreshed_vecs.len(), "id {id}: same number of parts from both paths");
            for (part, (a, b)) in built_vecs.iter().zip(&refreshed_vecs).enumerate() {
                assert_eq!(a, b, "id {id} part {part}: build and refresh must store bit-identical vectors");
            }
        }
    }
}
