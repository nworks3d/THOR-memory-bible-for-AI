use rusqlite::{Connection, Result as SqlResult, params};
use sha2::{Sha256, Digest};
use uuid::Uuid;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Event {
    pub seq: i64,
    pub event_uuid: String,
    pub session_id: String,
    pub lineage_id: String,
    pub actor: String,
    pub kind: EventKind,
    pub entity_id: String,
    pub parent_rev: Option<String>,
    pub body: String,
    pub body_ch: String,
    pub prev_hash: String,
    pub this_hash: String,
}

/// One event as shipped between machines (log shipping). Exactly the JSONL
/// backup field set: the fact-content quad + provenance + the authority's seq and
/// this_hash. `event_uuid` and `prev_hash` are deliberately omitted - event_uuid
/// is local identity (regenerated on ingest, never hashed) and prev_hash is
/// implied by the receiver's own chain tail (that IS the continuity check).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ShippedEvent {
    pub seq: i64,
    pub session_id: String,
    pub lineage_id: String,
    pub actor: String,
    pub kind: String,
    pub entity_id: String,
    #[serde(default)]
    pub parent_rev: Option<String>,
    pub body: String,
    pub this_hash: String,
}

impl Event {
    /// Wire form for shipping this event to a replica.
    pub fn to_shipped(&self) -> ShippedEvent {
        ShippedEvent {
            seq: self.seq,
            session_id: self.session_id.clone(),
            lineage_id: self.lineage_id.clone(),
            actor: self.actor.clone(),
            kind: self.kind.as_str().to_string(),
            entity_id: self.entity_id.clone(),
            parent_rev: self.parent_rev.clone(),
            body: self.body.clone(),
            this_hash: self.this_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    FactCreated,
    FactRevised,
    FactSuperseded,
    FactRetracted,
    FactReasserted,
    FactEchoed,
    FactResolved,
    /// Reassigns the fact's PROJECT scope (body = `{"project":"<key>"}` or
    /// `{"project":null}` for global). Never changes the head-set; folded like a
    /// no-op for heads, read only by the project fold (see cas::compute_projects).
    FactReprojected,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::FactCreated => "fact_created",
            EventKind::FactRevised => "fact_revised",
            EventKind::FactSuperseded => "fact_superseded",
            EventKind::FactRetracted => "fact_retracted",
            EventKind::FactReasserted => "fact_reasserted",
            EventKind::FactEchoed => "fact_echoed",
            EventKind::FactResolved => "fact_resolved",
            EventKind::FactReprojected => "fact_reprojected",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "fact_created" => Some(EventKind::FactCreated),
            "fact_revised" => Some(EventKind::FactRevised),
            "fact_superseded" => Some(EventKind::FactSuperseded),
            "fact_retracted" => Some(EventKind::FactRetracted),
            "fact_reasserted" => Some(EventKind::FactReasserted),
            "fact_echoed" => Some(EventKind::FactEchoed),
            "fact_resolved" => Some(EventKind::FactResolved),
            "fact_reprojected" => Some(EventKind::FactReprojected),
            _ => None,
        }
    }
}

pub fn canonicalize_body(body: &str) -> String {
    // Normalize CRLF and lone CR to LF so the same logical content always
    // canonicalizes (and therefore hashes) identically across platforms
    // and editors, then strip trailing whitespace.
    body.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end()
        .to_string()
}

pub fn hash_event(prev_hash: &str, canonical_repr: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical_repr.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Frame one field as `<byte-len>:<bytes>`. Concatenating framed fields is an
/// INJECTIVE encoding: a decoder reads the decimal length, then exactly that
/// many bytes, so nothing a field contains ('|', ':', digits, newlines) can
/// shift a field boundary. A bare "a|b|c" join is NOT injective when the fields
/// are free text: (entity_id="x", body_ch="y||z") and (entity_id="x||y",
/// body_ch="z") both flatten to "...|x||y||z", so a stored row could be
/// rewritten to a different (entity_id, body) that hashes identically and slips
/// past fsck (and even past replica tip-comparison, since this_hash is
/// unchanged). Length-framing closes that: any change to a HASHED field (the
/// four below) alters the canonical and therefore the recomputed hash.
fn frame(field: &str) -> String {
    format!("{}:{}", field.len(), field)
}

/// The single canonical representation of an event that is hashed into
/// this_hash. The writer (append) and the auditor (fsck) MUST both go through
/// this function so their notions of "the hashed content" can never drift.
///
/// This is `canonical(event)` from the spec formula
/// `this_hash = sha256(prev_hash || canonical(event))` (design section 4):
/// `canonical(event) = frame(kind) . frame(entity_id) . frame(parent_rev) .
/// frame(body_ch)`. It covers exactly the FACT-CONTENT QUAD, no more. Two kinds
/// of fields are deliberately OUTSIDE the hash:
///
/// - the storage `seq`: a hub-assigned counter, not part of the event's
///   meaning; its integrity is enforced structurally by fsck (seq must be
///   contiguous from 1, and the prev_hash chain binds every event to its exact
///   position - rewriting a position forces rewriting the whole suffix, true
///   with or without seq in the hash, since M0 carries no external signature);
/// - the provenance fields `actor`, `session_id`, `lineage_id`: per the schema
///   these are display/provenance-only and never decision input, so M0 does
///   NOT make them tamper-evident. A rewrite of actor/session/lineage on a row
///   passes fsck; that is an accepted M0 stance, not a covered field. (If
///   provenance ever needs tamper-evidence, add it to the canonical then.)
///
/// An empty-string parent_rev is normalized to "no parent" before hashing (see
/// insert_event), so `None` and `Some("")` can never denote distinct events.
pub fn canonical_repr_parts(
    kind: EventKind,
    entity_id: &str,
    parent_rev: Option<&str>,
    body_ch: &str,
) -> String {
    format!(
        "{}{}{}{}",
        frame(kind.as_str()),
        frame(entity_id),
        frame(parent_rev.unwrap_or("")),
        frame(body_ch)
    )
}

pub fn canonical_repr(event: &Event) -> String {
    canonical_repr_parts(
        event.kind,
        &event.entity_id,
        event.parent_rev.as_deref(),
        &event.body_ch,
    )
}

/// A resolve was rejected because the head-set it cited does not match the
/// current head-set (multi-head CAS failure). Carries the fresh head-set so
/// the caller can retry citing exactly what is current.
#[derive(Debug, thiserror::Error)]
#[error("resolve rejected for entity {entity_id}: {reason}; current head-set: {current_heads:?}")]
pub struct ResolveConflict {
    pub entity_id: String,
    pub reason: String,
    pub current_heads: Vec<String>,
}

/// A checked revise/retract was rejected: the entity is unknown, diverged with
/// no parent cited, or the cited parent_rev is not a current head. Carries the
/// fresh head-set so the caller can retry citing exactly what is current -
/// the MCP write path returns this instead of ever minting a silent branch.
#[derive(Debug, thiserror::Error)]
#[error("mutate rejected for entity {entity_id}: {reason}; current head-set: {current_heads:?}")]
pub struct MutateConflict {
    pub entity_id: String,
    pub reason: String,
    pub current_heads: Vec<String>,
}

/// Result of ingesting a shipped batch into a replica (log shipping receiver).
#[derive(Debug)]
pub enum IngestOutcome {
    /// The batch continued our chain. `applied` new events were appended,
    /// `skipped` were already present (idempotent re-ship). `contiguous_seq` /
    /// `tip_hash` are the replica's tip AFTER ingest.
    Applied {
        applied: usize,
        skipped: usize,
        contiguous_seq: i64,
        tip_hash: String,
    },
    /// The batch does NOT continue our chain (a gap, a fork on shared history, or
    /// a tampered payload). NOTHING was written - the whole batch is atomic.
    /// `contiguous_seq` / `tip_hash` are our UNCHANGED tip, so the shipper can
    /// resume from exactly there.
    Rejected {
        reason: String,
        at_seq: i64,
        contiguous_seq: i64,
        tip_hash: String,
    },
}

const EVENT_COLUMNS: &str = "seq, event_uuid, session_id, lineage_id, actor, kind, entity_id, \
                             parent_rev, body, body_ch, prev_hash, this_hash";

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    Ok(Event {
        seq: row.get(0)?,
        event_uuid: row.get(1)?,
        session_id: row.get(2)?,
        lineage_id: row.get(3)?,
        actor: row.get(4)?,
        kind: EventKind::from_str(&row.get::<_, String>(5)?)
            .ok_or(rusqlite::Error::InvalidQuery)?,
        entity_id: row.get(6)?,
        parent_rev: row.get(7)?,
        body: row.get(8)?,
        body_ch: row.get(9)?,
        prev_hash: row.get(10)?,
        this_hash: row.get(11)?,
    })
}

fn next_seq(conn: &Connection) -> SqlResult<i64> {
    conn.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM event", [], |row| row.get(0))
}

fn last_hash(conn: &Connection) -> SqlResult<String> {
    // "" is the genesis prev_hash and is returned ONLY for a genuinely empty
    // log. Every other error (BUSY, IO error, corruption) propagates: mapping
    // it to "" would durably commit a forged chain break.
    let result = conn.query_row(
        "SELECT this_hash FROM event ORDER BY seq DESC LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(hash) => Ok(hash),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// The hole-proof "contiguous tip": (contiguous_seq, tip_hash) where
/// contiguous_seq is the highest N such that EVERY seq 1..=N is present (never
/// MAX(seq) - a gap above a hole must not be reported as reached), and tip_hash
/// is the this_hash at that seq. (0, "") for an empty log or a gap from the very
/// start. The predicate `e.seq == COUNT(x.seq <= e.seq)` holds exactly when the
/// prefix 1..=e.seq is dense; MAX over those rows is the contiguous frontier.
fn contiguous_tip_conn(conn: &Connection) -> SqlResult<(i64, String)> {
    // contiguous_seq = the highest N such that every seq 1..=N is present.
    // Robust against an externally-injected non-positive seq: a seq <= 0 must
    // NEITHER mask a real hole above it NOR sink a clean 1..K prefix to 0, so the
    // whole computation is scoped to seq >= 1. Fast on the healthy path: when the
    // positive seqs are already dense from 1 (MAX == COUNT over seq >= 1) the
    // frontier is just MAX (index-only); only a genuine hole falls through to the
    // first-gap scan, which MIN short-circuits at the lowest gap (so it is cheap
    // exactly when a low hole would make a COUNT(x.seq <= e.seq) form quadratic).
    let cseq: i64 = conn.query_row(
        "SELECT CASE \
           WHEN NOT EXISTS (SELECT 1 FROM event WHERE seq = 1) THEN 0 \
           WHEN (SELECT MAX(seq) FROM event WHERE seq >= 1) \
              = (SELECT COUNT(*) FROM event WHERE seq >= 1) \
             THEN (SELECT MAX(seq) FROM event WHERE seq >= 1) \
           ELSE (SELECT MIN(e.seq) FROM event e WHERE e.seq >= 1 \
                 AND NOT EXISTS (SELECT 1 FROM event n WHERE n.seq = e.seq + 1)) \
         END",
        [],
        |r| r.get(0),
    )?;
    if cseq == 0 {
        return Ok((0, String::new()));
    }
    let hash: String =
        conn.query_row("SELECT this_hash FROM event WHERE seq = ?", [cseq], |r| r.get(0))?;
    Ok((cseq, hash))
}

fn this_hash_at(conn: &Connection, seq: i64) -> SqlResult<Option<String>> {
    match conn.query_row("SELECT this_hash FROM event WHERE seq = ?", [seq], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(h) => Ok(Some(h)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

fn query_all_events(conn: &Connection) -> SqlResult<Vec<Event>> {
    let mut stmt = conn.prepare(&format!("SELECT {} FROM event ORDER BY seq", EVENT_COLUMNS))?;
    let events = stmt.query_map([], row_to_event)?;
    events.collect()
}

fn query_events_by_entity(conn: &Connection, entity_id: &str) -> SqlResult<Vec<Event>> {
    let mut stmt = conn
        .prepare(&format!("SELECT {} FROM event WHERE entity_id = ? ORDER BY seq", EVENT_COLUMNS))?;
    let events = stmt.query_map([entity_id], row_to_event)?;
    events.collect()
}

/// Insert one event on a handle that ALREADY holds the immediate (RESERVED)
/// write lock. seq and prev_hash are read on that same handle, inside the
/// transaction: two writers can therefore never observe the same
/// MAX(seq)/last hash and race each other. The caller commits.
fn insert_event(
    conn: &Connection,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    kind: EventKind,
    entity_id: &str,
    parent_rev: Option<&str>,
    body: &str,
) -> anyhow::Result<Event> {
    let event_uuid = Uuid::new_v4().to_string();
    let body_ch = canonicalize_body(body);

    // Normalize an empty parent_rev to "no parent": an empty string is not a
    // valid rev (revs are 64-hex sha256 hashes), and leaving Some("") around
    // would alias with None in the canonical (both frame to "0:"). Coercing it
    // here keeps the empty-parent state unrepresentable end to end (stored NULL,
    // hashed as no-parent), so None and Some("") can never denote two events.
    let parent_rev = parent_rev.filter(|p| !p.is_empty());

    let seq = next_seq(conn)?;
    let prev_hash = last_hash(conn)?;

    let canonical = canonical_repr_parts(kind, entity_id, parent_rev, &body_ch);
    let this_hash = hash_event(&prev_hash, &canonical);

    conn.execute(
        "INSERT INTO event (seq, event_uuid, session_id, lineage_id, actor, kind, entity_id,
         parent_rev, body, body_ch, prev_hash, this_hash) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            seq,
            &event_uuid,
            session_id,
            lineage_id,
            actor,
            kind.as_str(),
            entity_id,
            parent_rev,
            body,
            &body_ch,
            &prev_hash,
            &this_hash
        ],
    )?;

    // Recall projection, SAME transaction as the append: the FTS index can
    // never be more or less current than the log it indexes.
    conn.execute(
        "INSERT INTO event_fts(rowid, body_ch) VALUES (?, ?)",
        params![seq, &body_ch],
    )?;

    Ok(Event {
        seq,
        event_uuid,
        session_id: session_id.to_string(),
        lineage_id: lineage_id.to_string(),
        actor: actor.to_string(),
        kind,
        entity_id: entity_id.to_string(),
        parent_rev: parent_rev.map(|s| s.to_string()),
        body: body.to_string(),
        body_ch,
        prev_hash,
        this_hash,
    })
}

pub struct EventStore {
    conn: Connection,
}

impl EventStore {
    pub fn new(path: &Path) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        // Serialize competing writers instead of erroring with SQLITE_BUSY.
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode = WAL")?;
        conn.execute_batch("PRAGMA synchronous = FULL")?;
        Self::init_schema(&conn)?;
        Self::sync_fts(&conn)?;
        Ok(EventStore { conn })
    }

    pub fn in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode = WAL")?;
        conn.execute_batch("PRAGMA synchronous = FULL")?;
        Self::init_schema(&conn)?;
        Self::sync_fts(&conn)?;
        Ok(EventStore { conn })
    }

    fn init_schema(conn: &Connection) -> SqlResult<()> {
        // M0 stance: head-sets are a pure projection recomputed from the event
        // log on every read (see cas::compute_head_sets). There are
        // deliberately NO materialized projection tables yet. When they arrive
        // (M1+), they must be written in the SAME transaction as the append,
        // and fsck must assert stored projection == from-scratch fold.
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS event (
                seq INTEGER PRIMARY KEY,
                event_uuid TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                lineage_id TEXT NOT NULL,
                actor TEXT NOT NULL,
                kind TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                parent_rev TEXT,
                body TEXT NOT NULL,
                body_ch TEXT NOT NULL,
                prev_hash TEXT NOT NULL,
                this_hash TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_event_entity ON event(entity_id);
            -- Recall projection (M1): a contentless FTS5 index over body_ch,
            -- keyed by the event seq (rowid). Written in the SAME transaction as
            -- the event append (see insert_event), so the index can never lag the
            -- log. `thor fsck` (verify_fts_projection) asserts the index ROW SET
            -- equals the log; sync_fts heals a cold-open row-count mismatch (a
            -- pre-M1 store or aborted backfill). Per-row text is not separately
            -- audited (contentless index), but it cannot drift from the code path.
            CREATE VIRTUAL TABLE IF NOT EXISTS event_fts USING fts5(body_ch, content='');
            ",
        )?;
        Ok(())
    }

    /// Rebuild the FTS projection from the log if the two ever disagree in row
    /// count (e.g. a store written by a pre-M1 binary that had no FTS table, or
    /// an aborted backfill). Cheap no-op when they already match. Append keeps
    /// them in lockstep in-transaction; this only heals a cold-open mismatch.
    fn sync_fts(conn: &Connection) -> SqlResult<()> {
        let events: i64 = conn.query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
        let indexed: i64 = conn.query_row("SELECT COUNT(*) FROM event_fts", [], |r| r.get(0))?;
        if events == indexed {
            return Ok(());
        }
        conn.execute("INSERT INTO event_fts(event_fts) VALUES('delete-all')", [])?;
        let mut stmt = conn.prepare("SELECT seq, body_ch FROM event ORDER BY seq")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (seq, body_ch) = row?;
            conn.execute(
                "INSERT INTO event_fts(rowid, body_ch) VALUES (?, ?)",
                params![seq, &body_ch],
            )?;
        }
        Ok(())
    }

    pub fn get_next_seq(&self) -> SqlResult<i64> {
        next_seq(&self.conn)
    }

    pub fn get_prev_hash(&self) -> SqlResult<String> {
        last_hash(&self.conn)
    }

    pub fn append_event(
        &mut self,
        session_id: &str,
        lineage_id: &str,
        actor: &str,
        kind: EventKind,
        entity_id: &str,
        parent_rev: Option<&str>,
        body: &str,
    ) -> anyhow::Result<Event> {
        // BEGIN IMMEDIATE takes the write lock FIRST; seq and prev_hash are
        // then read inside the transaction (see insert_event), so the read-
        // decide-write sequence is atomic against other writers.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let event = insert_event(&tx, session_id, lineage_id, actor, kind, entity_id, parent_rev, body)?;
        tx.commit()?;
        Ok(event)
    }

    /// Append a fact_resolved event as a real multi-head CAS: the caller must
    /// cite the exact, full current head-set ({keep_rev} union discarded[]).
    /// The head-set is recomputed under the immediate write lock, so no head
    /// can appear between the check and the append. On mismatch nothing is
    /// written and a ResolveConflict carrying the fresh head-set is returned.
    pub fn append_resolve(
        &mut self,
        session_id: &str,
        lineage_id: &str,
        actor: &str,
        entity_id: &str,
        keep_rev: &str,
        discarded: &[String],
    ) -> anyhow::Result<Event> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // Per-entity load (idx_event_entity): the head fold only ever needs this
        // entity's events, so the whole log is never materialized under the lock.
        let events = query_events_by_entity(&tx, entity_id)?;
        let current: HashSet<String> = crate::cas::compute_head_sets(&events)
            .get(entity_id)
            .map(|head_set| head_set.heads.clone())
            .unwrap_or_default();

        let mut current_sorted: Vec<String> = current.iter().cloned().collect();
        current_sorted.sort();
        let conflict = |reason: &str| ResolveConflict {
            entity_id: entity_id.to_string(),
            reason: reason.to_string(),
            current_heads: current_sorted.clone(),
        };

        if discarded.iter().any(|d| d == keep_rev) {
            return Err(conflict("keep_rev is also listed in discarded").into());
        }
        if !current.contains(keep_rev) {
            return Err(conflict("keep_rev is not a current head").into());
        }
        let mut cited: HashSet<String> = discarded.iter().cloned().collect();
        cited.insert(keep_rev.to_string());
        if cited != current {
            return Err(conflict("cited head-set does not match the current head-set").into());
        }

        let body = serde_json::json!({ "keep_rev": keep_rev, "discarded": discarded }).to_string();
        let event = insert_event(
            &tx,
            session_id,
            lineage_id,
            actor,
            EventKind::FactResolved,
            entity_id,
            None,
            &body,
        )?;
        tx.commit()?;
        Ok(event)
    }

    /// Append a revise/retract as a CHECKED mutation: the head-set is recomputed
    /// under the immediate write lock, and the event is only written when its
    /// parent is a current head - so a concurrent writer turns the call into a
    /// typed MutateConflict (carrying the fresh heads) instead of a silent
    /// DIVERGED branch. `parent_rev = None` auto-fills the single head; a
    /// diverged entity then rejects with "resolve first". This is the safe write
    /// path for agents (MCP): plain `append_event` stays the raw, branch-capable
    /// primitive.
    pub fn append_mutate_checked(
        &mut self,
        session_id: &str,
        lineage_id: &str,
        actor: &str,
        kind: EventKind,
        entity_id: &str,
        parent_rev: Option<&str>,
        body: &str,
    ) -> anyhow::Result<Event> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // Head-set fold is per-entity, so only this entity's events are loaded
        // (idx_event_entity) - never the whole log while holding the write lock.
        let events = query_events_by_entity(&tx, entity_id)?;
        let current: HashSet<String> = crate::cas::compute_head_sets(&events)
            .get(entity_id)
            .map(|head_set| head_set.heads.clone())
            .unwrap_or_default();
        let mut current_sorted: Vec<String> = current.iter().cloned().collect();
        current_sorted.sort();
        let conflict = |reason: &str| MutateConflict {
            entity_id: entity_id.to_string(),
            reason: reason.to_string(),
            current_heads: current_sorted.clone(),
        };

        if current.is_empty() {
            return Err(conflict("unknown entity (no live head); store a new fact instead").into());
        }
        let parent = match parent_rev.filter(|p| !p.is_empty()) {
            Some(p) => {
                if !current.contains(p) {
                    return Err(conflict(
                        "parent_rev is not a current head (the fact changed since you read it)",
                    )
                    .into());
                }
                p.to_string()
            }
            None => {
                if current.len() > 1 {
                    return Err(conflict(
                        "entity is DIVERGED: resolve it first, or cite the exact parent_rev to mutate",
                    )
                    .into());
                }
                current_sorted[0].clone()
            }
        };

        let event = insert_event(&tx, session_id, lineage_id, actor, kind, entity_id, Some(&parent), body)?;
        tx.commit()?;
        Ok(event)
    }

    /// Append a fact_created with its uniqueness checks under the SAME immediate
    /// write lock, so a concurrent writer process (another MCP server, a running
    /// `thor import`) cannot slip an equal fact between check and append:
    /// - the entity must not exist yet: a second parentless root would silently
    ///   ADD a contested head (create is never an upsert);
    /// - no live (non-retracted) head accepted by `is_dup` may exist; the caller
    ///   supplies the near-duplicate predicate over
    ///   (entity_id, effective_project, head_body).
    /// On conflict nothing is written and a typed MutateConflict names the
    /// blocking entity (reason "already exists" or "near-duplicate").
    pub fn append_created_unique<F>(
        &mut self,
        session_id: &str,
        lineage_id: &str,
        actor: &str,
        entity_id: &str,
        body: &str,
        is_dup: F,
    ) -> anyhow::Result<Event>
    where
        F: Fn(&str, Option<&str>, &str) -> bool,
    {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // The dup scan needs every entity's live heads, so this one loads the
        // full log under the lock - creates are rare (a handful per session),
        // unlike the per-entity mutate path above.
        let events = query_all_events(&tx)?;
        let head_sets = crate::cas::compute_head_sets(&events);
        if let Some(head_set) = head_sets.get(entity_id) {
            let mut heads: Vec<String> = head_set.heads.iter().cloned().collect();
            heads.sort();
            return Err(MutateConflict {
                entity_id: entity_id.to_string(),
                reason: "entity already exists (create is never an upsert): revise it, or omit \
                         entity_id to mint a new one"
                    .to_string(),
                current_heads: heads,
            }
            .into());
        }
        let projects = crate::cas::compute_projects(&events);
        let by_hash: std::collections::HashMap<&str, &Event> =
            events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
        for (id, head_set) in &head_sets {
            let project = projects.get(id).and_then(|p| p.as_deref());
            for rev in &head_set.heads {
                let Some(head_ev) = by_hash.get(rev.as_str()) else { continue };
                if matches!(head_ev.kind, EventKind::FactRetracted) {
                    continue; // a retracted head is not a live duplicate
                }
                if is_dup(id, project, &head_ev.body) {
                    return Err(MutateConflict {
                        entity_id: id.clone(),
                        reason: "near-duplicate of an existing live fact".to_string(),
                        current_heads: vec![rev.clone()],
                    }
                    .into());
                }
            }
        }

        let event =
            insert_event(&tx, session_id, lineage_id, actor, EventKind::FactCreated, entity_id, None, body)?;
        tx.commit()?;
        Ok(event)
    }

    pub fn get_event_by_uuid(&self, event_uuid: &str) -> SqlResult<Option<Event>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {} FROM event WHERE event_uuid = ?", EVENT_COLUMNS))?;
        match stmt.query_row([event_uuid], row_to_event) {
            Ok(event) => Ok(Some(event)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn get_all_events(&self) -> SqlResult<Vec<Event>> {
        query_all_events(&self.conn)
    }

    pub fn get_events_by_entity(&self, entity_id: &str) -> SqlResult<Vec<Event>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM event WHERE entity_id = ? ORDER BY seq",
            EVENT_COLUMNS
        ))?;
        let events = stmt.query_map([entity_id], row_to_event)?;
        events.collect()
    }

    /// The hole-proof contiguous tip (contiguous_seq, tip_hash) - the resume
    /// cursor and lag basis for log shipping.
    pub fn contiguous_tip(&self) -> SqlResult<(i64, String)> {
        contiguous_tip_conn(&self.conn)
    }

    /// A cheap key that changes whenever the log changes: (row count, max seq).
    ///
    /// EXACT for this table by construction: every statement that touches
    /// `event` is a plain `INSERT` (`append_event`, `ingest_batch`, restore) -
    /// there is no UPDATE, no DELETE and no INSERT OR REPLACE anywhere, so a row
    /// can never change under a fixed count. Any append moves the count; `max
    /// seq` rides along as a second witness and costs nothing.
    ///
    /// Deliberately NOT `contiguous_tip()`: that one is hole-proof (it scans for
    /// the first gap) and measures ~30ms on a 16k-event log - a third of the
    /// 81ms fold it is supposed to guard, which would eat the entire point of a
    /// resident cache. It is not needed here anyway: `ingest_batch` only accepts
    /// tip+1, so a replica cannot punch a hole below `MAX(seq)`. This pair costs
    /// ~0.02ms (both index-only), i.e. ~4000x cheaper than the fold - and that
    /// asymmetry is exactly what makes a resident cache worth holding.
    /// Used by `recall::ResidentCache` to decide reuse-or-rebuild.
    pub fn cache_fingerprint(&self) -> SqlResult<(i64, i64)> {
        self.conn.query_row(
            "SELECT COUNT(*), COALESCE(MAX(seq), 0) FROM event",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    /// The shipper's read side: every event with seq > `after_seq`, in chain
    /// order. Map with `Event::to_shipped` to ship the backlog to a replica.
    pub fn events_since(&self, after_seq: i64) -> SqlResult<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {} FROM event WHERE seq > ? ORDER BY seq", EVENT_COLUMNS))?;
        let events = stmt.query_map([after_seq], row_to_event)?;
        events.collect()
    }

    /// Log-shipping receiver: ingest a shipped batch that must CONTINUE this
    /// store's chain. Events already present (seq <= our tip) are verified then
    /// skipped (idempotent re-ship); the next event must be exactly our tip+1 and,
    /// replayed onto our tail, reconstruct the shipped this_hash. That single
    /// equality proves prev_hash continuity AND fact-content integrity (the
    /// canonical quad: kind, entity_id, parent_rev, canonicalized body), because
    /// `this_hash = sha256(our_tail || canonical(event))` - a fork (our tail is
    /// not the authority's prefix) or an edit to any HASHED field diverges. NOT
    /// covered (see canonical_repr_parts - an accepted M0 stance): provenance
    /// fields (actor/session/lineage) and whitespace-only body differences sit
    /// outside the hash, and ORIGIN authenticity comes from the transport's
    /// bearer token, not this check. A gap, a fork on shared history, or a tampered payload rejects
    /// the WHOLE batch atomically (nothing written - the tx rolls back) and
    /// returns our unchanged contiguous tip so the shipper resumes from there.
    /// Batch order is normalized by seq; the append reuses the exact same
    /// insert_event path (so the FTS projection stays in lockstep).
    pub fn ingest_batch(&mut self, recs: &[ShippedEvent]) -> anyhow::Result<IngestOutcome> {
        let mut ordered: Vec<&ShippedEvent> = recs.iter().collect();
        ordered.sort_by_key(|r| r.seq);

        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Tip BEFORE the batch. Every rejection reports THIS (the batch is atomic,
        // so on reject our tip is unchanged), letting the shipper resume exactly.
        let (pre_cseq, pre_tip) = contiguous_tip_conn(&tx)?;
        let reject = |reason: String, at_seq: i64| IngestOutcome::Rejected {
            reason,
            at_seq,
            contiguous_seq: pre_cseq,
            tip_hash: pre_tip.clone(),
        };

        let mut expected = next_seq(&tx)?; // our tip + 1
        let mut applied = 0usize;
        let mut skipped = 0usize;

        for r in &ordered {
            if r.seq < expected {
                // Already have this seq: idempotent re-ship, but verify it is the
                // SAME event (a fork on shared history must not be swallowed).
                match this_hash_at(&tx, r.seq)? {
                    Some(h) if h == r.this_hash => {
                        skipped += 1;
                        continue;
                    }
                    Some(_) => {
                        return Ok(reject(
                            format!("fork: shipped seq {} differs from the event already stored there", r.seq),
                            r.seq,
                        ))
                    }
                    None => {
                        return Ok(reject(
                            format!(
                                "seq {} is below our next-seq but missing from the log: the replica has a hole (external corruption) - run fsck and rebuild; shipping cannot heal it",
                                r.seq
                            ),
                            r.seq,
                        ))
                    }
                }
            }
            if r.seq > expected {
                return Ok(reject(format!("gap: expected seq {expected}, got {}", r.seq), r.seq));
            }
            // r.seq == expected: replay onto our tail and verify the hash.
            // An unknown kind is adversarial / version-skew input, NOT a store
            // error: classify it as a Rejected (carrying the resume cursor) like
            // every other bad-batch branch, never a bare Err - a bare Err has no
            // cursor and is indistinguishable from a transient DB failure.
            let kind = match EventKind::from_str(&r.kind) {
                Some(k) => k,
                None => {
                    return Ok(reject(
                        format!(
                            "unknown event kind '{}' at seq {} (receiver too old, or a corrupt/forged payload)",
                            r.kind, r.seq
                        ),
                        r.seq,
                    ))
                }
            };
            let ev = insert_event(
                &tx,
                &r.session_id,
                &r.lineage_id,
                &r.actor,
                kind,
                &r.entity_id,
                r.parent_rev.as_deref(),
                &r.body,
            )?;
            if ev.this_hash != r.this_hash {
                // Reconstruction diverged: fork or tampered payload. Return before
                // commit; the tx drops here, rolling back this and every earlier
                // insert in the batch (atomic all-or-nothing).
                return Ok(reject(
                    format!(
                        "continuity/integrity fail at seq {}: replayed hash does not match the shipped one",
                        r.seq
                    ),
                    r.seq,
                ));
            }
            applied += 1;
            expected += 1;
        }
        tx.commit()?;
        let (cseq, tip) = self.contiguous_tip()?;
        Ok(IngestOutcome::Applied { applied, skipped, contiguous_seq: cseq, tip_hash: tip })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// Assert the FTS recall projection matches the log: same row count AND the same
/// rowid set as event.seq. This is the fsck-side half of the "projection written
/// in the append tx" contract. A contentless FTS5 index stores no column text to
/// read back, so per-row CONTENT is not independently compared here; content
/// cannot drift from the code path anyway, because each FTS row is written in the
/// same transaction as its event and body_ch is append-only (any body_ch edit is
/// already caught by the hash chain in verify_chain_integrity). This row-set
/// check catches a missing, extra, or orphan index entry (a pre-M1 store, an
/// aborted backfill, or external tampering of event_fts).
pub fn verify_fts_projection(conn: &Connection) -> Result<(), String> {
    let db_err = |e: rusqlite::Error| format!("FTS projection check could not run: {}", e);
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))
        .map_err(db_err)?;
    let indexed: i64 = conn
        .query_row("SELECT COUNT(*) FROM event_fts", [], |r| r.get(0))
        .map_err(db_err)?;
    if events != indexed {
        return Err(format!(
            "FTS index has {} rows but the log has {} (projection drift)",
            indexed, events
        ));
    }
    let missing: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event WHERE seq NOT IN (SELECT rowid FROM event_fts)",
            [],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    if missing > 0 {
        return Err(format!("{} event rows are missing from the FTS index", missing));
    }
    let orphan: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event_fts WHERE rowid NOT IN (SELECT seq FROM event)",
            [],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    if orphan > 0 {
        return Err(format!(
            "{} FTS entries point at a non-existent event seq",
            orphan
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize_body() {
        assert_eq!(canonicalize_body("hello\r\nworld\r\n"), "hello\nworld");
        assert_eq!(canonicalize_body("hello   \n"), "hello");
        assert_eq!(canonicalize_body("test"), "test");
        // lone CR (old-Mac endings) and mixed endings normalize like LF
        assert_eq!(canonicalize_body("a\rb\r"), "a\nb");
        assert_eq!(canonicalize_body("a\r\nb\rc\r\n"), "a\nb\nc");
        assert_eq!(canonicalize_body("a\rb"), canonicalize_body("a\nb"));
        assert_eq!(canonicalize_body("a\r\nb\rc"), canonicalize_body("a\nb\nc"));
    }

    #[test]
    fn test_hash_consistency() {
        let hash1 = hash_event("prev", "canonical1");
        let hash2 = hash_event("prev", "canonical1");
        assert_eq!(hash1, hash2);

        let hash3 = hash_event("prev", "canonical2");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_event_store_creation() {
        let store = EventStore::in_memory().unwrap();
        let seq = store.get_next_seq().unwrap();
        assert_eq!(seq, 1);
    }

    #[test]
    fn test_prev_hash_genesis_on_empty_log() {
        // the genuine no-rows case yields the genesis prev_hash ""
        let store = EventStore::in_memory().unwrap();
        assert_eq!(store.get_prev_hash().unwrap(), "");
    }

    #[test]
    fn test_this_hash_follows_seqless_spec_formula() {
        // Lock the wire format of the hash: this_hash = sha256(prev_hash ||
        // canonical(event)) where canonical(event) is the length-framed SEMANTIC
        // event and deliberately EXCLUDES the storage seq (design section 4).
        // The framed format is spelled out here (NOT via frame()/canonical_repr)
        // so a helper change cannot silently hide a wire-format regression; if
        // someone reintroduces seq or drops framing, this test fails loudly.
        let mut store = EventStore::in_memory().unwrap();
        let ev = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "the body")
            .unwrap();

        // frame(s) = "<byte-len>:<s>"; canonical = frame(kind).frame(entity).frame(parent).frame(body)
        let canonical = format!(
            "{}:{}{}:{}{}:{}{}:{}",
            "fact_created".len(), "fact_created",
            "e1".len(), "e1",
            "".len(), "",
            "the body".len(), "the body",
        );
        let expected = hash_event(&ev.prev_hash, &canonical);
        assert_eq!(
            ev.this_hash, expected,
            "this_hash must be sha256(prev_hash || framed(kind,entity,parent,body_ch)), no seq"
        );

        // A second event with a parent_rev: parent is in the canonical, seq is not.
        let ev2 = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(&ev.this_hash), "next body",
            )
            .unwrap();
        let canonical2 = format!(
            "{}:{}{}:{}{}:{}{}:{}",
            "fact_revised".len(), "fact_revised",
            "e1".len(), "e1",
            ev.this_hash.len(), &ev.this_hash,
            "next body".len(), "next body",
        );
        assert_eq!(ev2.this_hash, hash_event(&ev2.prev_hash, &canonical2));
    }

    #[test]
    fn test_seq_integrity_is_structural_not_hashed() {
        // Two independent claims, each with its own assertion (the earlier
        // version asserted only is_err(), which passes whether or not seq is
        // hashed - so it proved neither half):
        //   (a) seq is NOT in the hash: the stored this_hash still equals a
        //       recompute over the semantic fields after seq is changed;
        //   (b) the tamper is nonetheless caught, specifically by the
        //       contiguity check (not incidentally by a hash mismatch).
        use crate::auditor::verify_chain_integrity;
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "b1")
            .unwrap();
        store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e2", None, "b2")
            .unwrap();

        let mut events = store.get_all_events().unwrap();
        assert!(verify_chain_integrity(&events).is_ok(), "clean log passes");

        events[1].seq = 99;

        // (a) if seq WERE hashed, this recompute would diverge from the stored hash
        assert_eq!(
            events[1].this_hash,
            hash_event(&events[1].prev_hash, &canonical_repr(&events[1])),
            "this_hash must be independent of seq (seq is not part of the canonical)"
        );

        // (b) caught specifically by the contiguity branch
        let err = verify_chain_integrity(&events).unwrap_err();
        assert!(
            err.contains("Non-contiguous seq"),
            "a tampered seq must be caught by the contiguity check, got: {}",
            err
        );
    }

    #[test]
    fn test_seq_swap_is_caught_by_prev_hash_chain() {
        // A subtler seq tamper than a renumber: swap two events' seq values so
        // the set {1,2} stays contiguous. Contiguity alone cannot see this; only
        // the prev_hash chain does - the exact guarantee the canonical_repr doc
        // comment leans on to justify dropping seq from the hash.
        use crate::auditor::verify_chain_integrity;
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "b1")
            .unwrap();
        store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e2", None, "b2")
            .unwrap();

        let mut events = store.get_all_events().unwrap();
        let (s0, s1) = (events[0].seq, events[1].seq);
        events[0].seq = s1;
        events[1].seq = s0;
        events.sort_by_key(|e| e.seq); // mimic get_all_events' ORDER BY seq

        let err = verify_chain_integrity(&events).unwrap_err();
        assert!(
            err.contains("Hash chain broken"),
            "a seq swap must be caught by the prev_hash chain, got: {}",
            err
        );
    }

    #[test]
    fn test_empty_parent_rev_normalizes_to_none() {
        // Some("") and None are the same event: an empty parent is "no parent".
        // Normalizing at the write boundary keeps that state unrepresentable, so
        // the None-vs-Some("") canonical alias can never become a latent hazard.
        let mut store_none = EventStore::in_memory().unwrap();
        let a = store_none
            .append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "body")
            .unwrap();
        let mut store_empty = EventStore::in_memory().unwrap();
        let b = store_empty
            .append_event("s", "l", "act", EventKind::FactCreated, "e1", Some(""), "body")
            .unwrap();

        assert_eq!(a.parent_rev, None);
        assert_eq!(b.parent_rev, None, "Some(\"\") must be normalized to None");
        assert_eq!(
            a.this_hash, b.this_hash,
            "an empty parent_rev must hash identically to no parent"
        );
    }

    #[test]
    fn test_canonical_is_injective_across_field_boundaries() {
        // Length-framing must keep field boundaries unambiguous so two
        // semantically DIFFERENT events never share a canonical (a bare
        // '|'-join collides these, letting a stored row be rewritten to a
        // different (entity_id, body) that hashes identically and passes fsck).
        let a = canonical_repr_parts(EventKind::FactCreated, "x", None, "y||z");
        let b = canonical_repr_parts(EventKind::FactCreated, "x||y", None, "z");
        assert_ne!(a, b, "(entity_id, body_ch) boundary must be unambiguous");

        let c = canonical_repr_parts(EventKind::FactCreated, "a|", None, "b");
        let d = canonical_repr_parts(EventKind::FactCreated, "a", None, "|b");
        assert_ne!(c, d, "a '|' at a field edge must not shift the boundary");
    }

    #[test]
    fn test_verify_fts_projection_detects_drift() {
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "alpha body")
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "beta body")
            .unwrap();
        assert!(
            verify_fts_projection(store.conn()).is_ok(),
            "a store whose FTS was written in-tx must pass"
        );

        // inject an orphan FTS row: count + rowid-set now drift from the log
        store
            .conn()
            .execute("INSERT INTO event_fts(rowid, body_ch) VALUES (999, 'orphan')", [])
            .unwrap();
        assert!(
            verify_fts_projection(store.conn()).is_err(),
            "fsck must catch an FTS index that drifted from the log"
        );
    }

    #[test]
    fn test_append_event() {
        let mut store = EventStore::in_memory().unwrap();
        let event = store
            .append_event(
                "session1",
                "lineage1",
                "actor1",
                EventKind::FactCreated,
                "entity1",
                None,
                "test body",
            )
            .unwrap();

        assert_eq!(event.seq, 1);
        assert_eq!(event.entity_id, "entity1");
        assert_eq!(event.kind, EventKind::FactCreated);

        let retrieved = store.get_event_by_uuid(&event.event_uuid).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().seq, 1);
    }

    #[test]
    fn test_append_mutate_checked_cas_semantics() {
        let mut store = EventStore::in_memory().unwrap();
        let v1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "v1")
            .unwrap();

        // single head + no parent cited -> auto-fills the head (a safe FF revise)
        let v2 = store
            .append_mutate_checked("s", "l", "a", EventKind::FactRevised, "e1", None, "v2")
            .unwrap();
        assert_eq!(v2.parent_rev.as_deref(), Some(v1.this_hash.as_str()));

        // a STALE parent is rejected with the fresh head-set, and nothing is written
        let n_before = store.get_all_events().unwrap().len();
        let err = store
            .append_mutate_checked("s", "l", "a", EventKind::FactRevised, "e1", Some(&v1.this_hash), "v3")
            .unwrap_err();
        let conflict = err.downcast_ref::<MutateConflict>().expect("typed conflict");
        assert!(conflict.reason.contains("not a current head"));
        assert_eq!(conflict.current_heads, vec![v2.this_hash.clone()]);
        assert_eq!(store.get_all_events().unwrap().len(), n_before, "rejected mutate writes nothing");

        // unknown entity -> typed conflict, not a silent create
        assert!(store
            .append_mutate_checked("s", "l", "a", EventKind::FactRevised, "nope", None, "x")
            .unwrap_err()
            .downcast_ref::<MutateConflict>()
            .is_some());

        // force a divergence via the raw primitive; a parentless mutate then rejects
        store
            .append_event("s", "l", "a", EventKind::FactRevised, "e1", Some("stale-parent"), "branch")
            .unwrap();
        let err = store
            .append_mutate_checked("s", "l", "a", EventKind::FactRetracted, "e1", None, "")
            .unwrap_err();
        let conflict = err.downcast_ref::<MutateConflict>().expect("typed conflict");
        assert!(conflict.reason.contains("DIVERGED"));
        assert_eq!(conflict.current_heads.len(), 2, "the fresh head-set is returned for the retry");

        // ...but citing one exact contested head is allowed (an explicit choice)
        let keep = conflict.current_heads[0].clone();
        store
            .append_mutate_checked("s", "l", "a", EventKind::FactRevised, "e1", Some(&keep), "explicit")
            .unwrap();
    }

    // ---- log shipping (sync receiver) ----

    /// An authority store with a small chain (2 revs of e1 + one e2).
    fn authority() -> EventStore {
        let mut a = EventStore::in_memory().unwrap();
        let e1 = a.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "first").unwrap();
        a.append_event("s", "l", "act", EventKind::FactRevised, "e1", Some(&e1.this_hash), "second").unwrap();
        a.append_event("s", "l", "act", EventKind::FactCreated, "e2", None, "third").unwrap();
        a
    }

    fn shipped_of(store: &EventStore) -> Vec<ShippedEvent> {
        store.get_all_events().unwrap().iter().map(|e| e.to_shipped()).collect()
    }

    #[test]
    fn test_ingest_extends_replica_and_is_hash_identical() {
        let a = authority();
        let batch = shipped_of(&a);
        let mut r = EventStore::in_memory().unwrap();

        match r.ingest_batch(&batch).unwrap() {
            IngestOutcome::Applied { applied, skipped, contiguous_seq, tip_hash } => {
                assert_eq!(applied, 3);
                assert_eq!(skipped, 0);
                assert_eq!(contiguous_seq, 3);
                assert_eq!(tip_hash, a.contiguous_tip().unwrap().1, "replica tip must equal authority tip");
            }
            other => panic!("expected Applied, got {other:?}"),
        }
        // byte-for-byte the same chain
        let (ra, aa) = (r.get_all_events().unwrap(), a.get_all_events().unwrap());
        assert_eq!(ra.len(), aa.len());
        for (x, y) in ra.iter().zip(aa.iter()) {
            assert_eq!(x.this_hash, y.this_hash, "hash must match");
            assert_eq!(x.body, y.body);
            assert_eq!(x.entity_id, y.entity_id);
        }
    }

    #[test]
    fn test_ingest_incremental_prefix_then_tail() {
        let a = authority();
        let batch = shipped_of(&a);
        let mut r = EventStore::in_memory().unwrap();
        // seed the replica with just the first event...
        assert!(matches!(
            r.ingest_batch(&batch[..1]).unwrap(),
            IngestOutcome::Applied { applied: 1, .. }
        ));
        // ...then ship the tail; it must continue the chain.
        match r.ingest_batch(&batch[1..]).unwrap() {
            IngestOutcome::Applied { applied, contiguous_seq, .. } => {
                assert_eq!(applied, 2);
                assert_eq!(contiguous_seq, 3);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn test_ingest_idempotent_reship() {
        let a = authority();
        let batch = shipped_of(&a);
        let mut r = EventStore::in_memory().unwrap();
        r.ingest_batch(&batch).unwrap();
        // shipping the exact same batch again applies nothing and skips all
        match r.ingest_batch(&batch).unwrap() {
            IngestOutcome::Applied { applied, skipped, contiguous_seq, .. } => {
                assert_eq!(applied, 0);
                assert_eq!(skipped, 3);
                assert_eq!(contiguous_seq, 3);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn test_ingest_rejects_gap() {
        let a = authority();
        let batch = shipped_of(&a);
        let mut r = EventStore::in_memory().unwrap();
        // ship seq 2,3 into an EMPTY replica (expects seq 1 first) -> a gap
        match r.ingest_batch(&batch[1..]).unwrap() {
            IngestOutcome::Rejected { contiguous_seq, reason, .. } => {
                assert_eq!(contiguous_seq, 0);
                assert!(reason.contains("gap"), "reason: {reason}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(r.get_all_events().unwrap().is_empty(), "a rejected gap writes nothing");
    }

    #[test]
    fn test_ingest_rejects_tampered_payload() {
        let a = authority();
        let mut batch = shipped_of(&a);
        // corrupt a body but keep the recorded this_hash -> replay must diverge
        batch[0].body = "TAMPERED".to_string();
        let mut r = EventStore::in_memory().unwrap();
        match r.ingest_batch(&batch).unwrap() {
            IngestOutcome::Rejected { reason, .. } => {
                assert!(reason.contains("continuity/integrity"), "reason: {reason}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(r.get_all_events().unwrap().is_empty());
    }

    #[test]
    fn test_ingest_is_atomic_on_midbatch_reject() {
        let a = authority();
        let mut batch = shipped_of(&a);
        // first event is valid, second is tampered: the WHOLE batch must roll back
        batch[1].body = "TAMPERED".to_string();
        let mut r = EventStore::in_memory().unwrap();
        match r.ingest_batch(&batch).unwrap() {
            IngestOutcome::Rejected { at_seq, .. } => assert_eq!(at_seq, 2),
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(
            r.get_all_events().unwrap().is_empty(),
            "the valid first event must not survive a later rejection (atomic batch)"
        );
    }

    #[test]
    fn test_ingest_rejects_fork_on_shared_history() {
        // replica has its OWN seq 1; the authority ships a DIFFERENT seq 1.
        let mut r = EventStore::in_memory().unwrap();
        r.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "local version").unwrap();
        let mut a = EventStore::in_memory().unwrap();
        a.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "authoritative version").unwrap();

        match r.ingest_batch(&shipped_of(&a)).unwrap() {
            IngestOutcome::Rejected { reason, at_seq, .. } => {
                assert_eq!(at_seq, 1);
                assert!(reason.contains("fork"), "reason: {reason}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        // the replica's own event is untouched
        assert_eq!(r.get_all_events().unwrap()[0].body, "local version");
    }

    #[test]
    fn test_contiguous_tip_stops_at_a_hole() {
        let s = authority(); // seq 1,2,3
        assert_eq!(s.contiguous_tip().unwrap().0, 3);
        // simulate external tampering that leaves a hole: a raw row at seq 5
        s.conn()
            .execute(
                "INSERT INTO event (seq,event_uuid,session_id,lineage_id,actor,kind,entity_id,parent_rev,body,body_ch,prev_hash,this_hash) \
                 VALUES (5,'u5','s','l','a','fact_created','e9',NULL,'b','b','ph','th')",
                [],
            )
            .unwrap();
        assert_eq!(
            s.contiguous_tip().unwrap().0,
            3,
            "contiguous tip must stop at the hole (3), never jump to MAX(seq)=5"
        );
    }

    #[test]
    fn test_ingest_rejects_unknown_kind_without_erroring() {
        // An unknown kind is adversarial / version-skew input: it must come back
        // as a structured Rejected (carrying a resume cursor), never a bare Err
        // or a panic - the same uniform channel as gap/fork/tamper.
        let a = authority();
        let mut batch = shipped_of(&a);
        batch[0].kind = "fact_teleported".to_string();
        let mut r = EventStore::in_memory().unwrap();
        match r.ingest_batch(&batch).unwrap() {
            IngestOutcome::Rejected { reason, at_seq, contiguous_seq, .. } => {
                assert_eq!(at_seq, 1);
                assert_eq!(contiguous_seq, 0);
                assert!(reason.contains("unknown event kind"), "reason: {reason}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(r.get_all_events().unwrap().is_empty(), "a rejected batch writes nothing");
    }

    #[test]
    fn test_contiguous_tip_is_robust_to_injected_nonpositive_seq() {
        // Regression: a COUNT(x.seq <= e.seq) frontier assumed seqs start at 1, so
        // one injected seq <= 0 could mask a hole or sink a clean prefix. The
        // computation is now scoped to seq >= 1.
        let s = authority(); // dense 1,2,3
        // (a) a stray seq-0 row must NOT drop the clean 1..3 prefix to 0
        s.conn()
            .execute(
                "INSERT INTO event (seq,event_uuid,session_id,lineage_id,actor,kind,entity_id,parent_rev,body,body_ch,prev_hash,this_hash) \
                 VALUES (0,'u0','s','l','a','fact_created','e0',NULL,'b','b','ph','th0')",
                [],
            )
            .unwrap();
        assert_eq!(s.contiguous_tip().unwrap().0, 3, "a seq-0 row must not sink a dense 1..3 prefix");
        // (b) with the seq-0 present, a hole above must STILL cap the frontier
        s.conn()
            .execute(
                "INSERT INTO event (seq,event_uuid,session_id,lineage_id,actor,kind,entity_id,parent_rev,body,body_ch,prev_hash,this_hash) \
                 VALUES (5,'u5','s','l','a','fact_created','e9',NULL,'b','b','ph','th5')",
                [],
            )
            .unwrap();
        assert_eq!(
            s.contiguous_tip().unwrap().0,
            3,
            "the hole at seq 4 must still cap the frontier at 3, not jump to 5 via the seq-0 offset"
        );
    }
}
