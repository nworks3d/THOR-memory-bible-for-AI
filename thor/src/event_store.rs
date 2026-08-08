use rusqlite::{Connection, OpenFlags, OptionalExtension, Result as SqlResult, params};
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

    // Heads projection (M2), SAME transaction. Incremental when the
    // projection is exactly one event behind; any gap (fresh table, a store
    // written by a pre-M2 binary, a bulk path that bypassed append) triggers
    // a full in-transaction rebuild - amortized once, and the projection can
    // never advance from a wrong base.
    let tip: i64 = conn
        .query_row("SELECT v FROM projection_meta WHERE k = 'tip_seq'", [], |r| r.get(0))
        .unwrap_or(0);
    if tip == seq - 1 {
        EventStore::apply_projection_delta(
            conn, seq, kind, entity_id, parent_rev, body, &this_hash,
        )?;
        conn.execute(
            "INSERT INTO projection_meta (k, v) VALUES ('tip_seq', ?)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            params![seq],
        )?;
    } else {
        EventStore::rebuild_projection(conn)?;
    }

    // Facets projection (M3), SAME transaction. Unlike the block above this is
    // NOT a fold: one event's facets are a pure function of its own body, so
    // there is no tip to track and no rebuild-from-scratch branch - it simply
    // runs for every content-bearing event, every time, in the same append.
    EventStore::apply_facets_delta(conn, kind, entity_id, body, &this_hash)?;

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

    /// Open an EXISTING store to look at it: no create, no schema work, no FTS
    /// heal. `new` is the right constructor for anything that writes, but the
    /// inspection commands (doctor, fsck, status) went through it too, and it
    /// CREATES the file when it is missing. A typo in `--db` therefore produced a
    /// brand-new empty store that then looked like a healthy but empty THOR -
    /// the worst possible answer to "why is my memory not coming back". Skipping
    /// the FTS heal also lets fsck report a broken projection instead of quietly
    /// repairing it during its own open and then declaring itself OK; a normal
    /// command still heals it, because every writer keeps using `new`.
    /// Not the same as read-only: SQLite touches the -wal and -shm companions on
    /// any open. What this guarantees is narrower and is what matters - it never
    /// creates your store and never changes what is in it.
    pub fn open_existing(path: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            path.exists(),
            "no THOR store at {} - this command never creates one; check the path (--db) \
             or store your first memory to create it",
            path.display()
        );
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
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
        // Projections stance: head-sets WERE a pure fold recomputed from the
        // event log on every read (M0/M1a, see cas::compute_head_sets). M2
        // materializes that fold - as the M0 comment here always prescribed:
        // written in the SAME transaction as the append, and fsck asserts
        // stored projection == from-scratch fold. The log stays the only
        // truth; every projection table is derived, rebuildable, and readers
        // fall back to the fold whenever the projection is not current.
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
            CREATE INDEX IF NOT EXISTS idx_event_hash ON event(this_hash);
            -- Recall projection (M1): a contentless FTS5 index over body_ch,
            -- keyed by the event seq (rowid). Written in the SAME transaction as
            -- the event append (see insert_event), so the index can never lag the
            -- log. `thor fsck` (verify_fts_projection) asserts the index ROW SET
            -- equals the log; sync_fts heals a cold-open row-count mismatch (a
            -- pre-M1 store or aborted backfill). Per-row text is not separately
            -- audited (contentless index), but it cannot drift from the code path.
            CREATE VIRTUAL TABLE IF NOT EXISTS event_fts USING fts5(body_ch, content='');
            -- Heads projection (M2): cas::compute_head_sets and
            -- compute_projects, materialized. head_state holds every CURRENT
            -- head (one row per contested head; >1 rows = DIVERGED);
            -- entity_meta holds the effective project per seen entity (NULL =
            -- global); projection_meta.tip_seq records the last folded seq.
            -- Maintained by apply_projection_delta inside the append
            -- transaction; any writer that bypasses append (restore, a
            -- pre-M2 binary) leaves tip_seq behind MAX(seq), which makes
            -- every reader fall back to the full fold and the next append
            -- rebuild the projection in-transaction. Nothing here is truth.
            CREATE TABLE IF NOT EXISTS head_state (
                entity_id TEXT NOT NULL,
                rev_hash TEXT NOT NULL,
                head_seq INTEGER NOT NULL,
                PRIMARY KEY (entity_id, rev_hash)
            );
            CREATE INDEX IF NOT EXISTS idx_head_entity ON head_state(entity_id);
            CREATE TABLE IF NOT EXISTS entity_meta (
                entity_id TEXT PRIMARY KEY,
                project TEXT
            );
            CREATE TABLE IF NOT EXISTS projection_meta (
                k TEXT PRIMARY KEY,
                v INTEGER NOT NULL
            );
            -- Facets projection (M3): typed metadata per (entity, revision) -
            -- the SCHEMA-backed counterpart of the footer's free text (fact
            -- type, tags, fires-when triggers, anchors, expires, provenance).
            -- Written in the SAME transaction as the append (see
            -- apply_facets_delta), from the SAME body text every footer
            -- parser already reads - so it can never drift from what the
            -- footer says for a GIVEN revision, whose body is immutable once
            -- written. NOT the source of truth: the footer stays that, for
            -- every existing reader. This is a second, authoritative place a
            -- caller may read a typed field from FIRST (EventStore::
            -- read_facets), falling back to the footer for events written
            -- before this table existed (`thor backfill-facets` fills those
            -- in from their footer, on request; a MISSING row is not an
            -- error - see verify_facets_projection). tags/triggers/anchors
            -- are stored as JSON arrays (serde_json, already a dependency) -
            -- SQLite has no native array/list column type.
            -- `project`/`mimir_id` (M4): the two remaining footer fields a
            -- facets row needs to reconstruct a revision's ORIGINAL footer
            -- text byte for byte (see footer::compose_from_facets) - project
            -- is the LITERAL value THIS revision's footer carried (frozen at
            -- write time; the entity's EFFECTIVE project after a later
            -- `reproject` lives in entity_meta instead, and can legitimately
            -- differ), mimir_id the trailing import marker. Both nullable:
            -- absent on the overwhelming majority of footers (no project
            -- field at all is rare but legal; mimir_id only ever appears on
            -- an imported fact).
            CREATE TABLE IF NOT EXISTS fact_facets (
                entity_id TEXT NOT NULL,
                rev_hash TEXT NOT NULL,
                fact_type TEXT NOT NULL,
                tags TEXT NOT NULL,
                triggers TEXT NOT NULL,
                anchors TEXT NOT NULL,
                expires TEXT,
                provenance TEXT,
                project TEXT,
                mimir_id TEXT,
                PRIMARY KEY (entity_id, rev_hash)
            );
            ",
        )?;
        Ok(())
    }

    /// Write (or overwrite) one revision's facets row. Idempotent - the same
    /// (entity_id, rev_hash) always maps to the same event body, so writing
    /// it twice is a no-op in effect. Shared by the live write path
    /// (apply_facets_delta, inside the append transaction) and the CLI
    /// backfill (its own transaction, over already-existing events).
    fn upsert_facets_row(
        conn: &Connection,
        entity_id: &str,
        rev_hash: &str,
        facets: &crate::footer::Facets,
    ) -> SqlResult<()> {
        conn.execute(
            "INSERT INTO fact_facets
                (entity_id, rev_hash, fact_type, tags, triggers, anchors, expires, provenance,
                 project, mimir_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(entity_id, rev_hash) DO UPDATE SET
                fact_type = excluded.fact_type,
                tags = excluded.tags,
                triggers = excluded.triggers,
                anchors = excluded.anchors,
                expires = excluded.expires,
                provenance = excluded.provenance,
                project = excluded.project,
                mimir_id = excluded.mimir_id",
            params![
                entity_id,
                rev_hash,
                facets.fact_type,
                serde_json::to_string(&facets.tags).unwrap_or_default(),
                serde_json::to_string(&facets.triggers).unwrap_or_default(),
                serde_json::to_string(&facets.anchors).unwrap_or_default(),
                facets.expires,
                facets.provenance,
                facets.project,
                facets.mimir_id,
            ],
        )?;
        Ok(())
    }

    /// The facets delta for one event: parse its body's footer and, when it
    /// is a genuine memory footer, upsert the typed row - SAME transaction as
    /// the append (called from insert_event), so the two can never go out of
    /// sync. Unlike the heads/entity_meta projections this is NOT a fold: a
    /// revision's facets are a pure function of that one event's own body, so
    /// there is nothing to rebuild-from-scratch and no "tip_seq" to track -
    /// it runs on every content-bearing event, in any order, unconditionally.
    ///
    /// Only FactCreated/FactRevised carry real content (a retract's body is a
    /// tombstone, a supersede points elsewhere, resolve/reassert/echo/
    /// reproject touch no body at all - same rule `footer::defects` applies).
    /// Repo chunks carry their OWN footer shape (`[repo file | ...]`), which
    /// `parse_facets` already fails to match, but the id is filtered first
    /// too, for clarity and so a future chunk footer accidentally shaped like
    /// `[memory/...]` still could not slip into this table.
    fn apply_facets_delta(
        conn: &Connection,
        kind: EventKind,
        entity_id: &str,
        body: &str,
        this_hash: &str,
    ) -> SqlResult<()> {
        if !matches!(kind, EventKind::FactCreated | EventKind::FactRevised) {
            return Ok(());
        }
        if crate::repo::is_chunk_id(entity_id) {
            return Ok(());
        }
        let Some(facets) = crate::footer::parse_facets(body) else { return Ok(()) };
        Self::upsert_facets_row(conn, entity_id, this_hash, &facets)
    }

    /// Read a fact's typed metadata: the facets-table row for this EXACT
    /// revision when one exists, else parsed fresh from `body` (an event
    /// written before the facets table existed, or a store that has not been
    /// backfilled yet). The one place a caller reads metadata through, so the
    /// projection and the footer can never answer a caller differently.
    /// Fails open to the footer parse on ANY read error (including "no such
    /// table" on a store from before this feature, opened read-only) - a
    /// broken facets read must cost nothing but the optimization.
    pub fn read_facets(&self, entity_id: &str, rev_hash: &str, body: &str) -> crate::footer::Facets {
        self.query_facets_row(entity_id, rev_hash)
            .unwrap_or_else(|| crate::footer::parse_facets(body).unwrap_or_default())
    }

    fn query_facets_row(&self, entity_id: &str, rev_hash: &str) -> Option<crate::footer::Facets> {
        self.conn
            .query_row(
                "SELECT fact_type, tags, triggers, anchors, expires, provenance, project, mimir_id
                 FROM fact_facets WHERE entity_id = ? AND rev_hash = ?",
                params![entity_id, rev_hash],
                |r| {
                    let tags: String = r.get(1)?;
                    let triggers: String = r.get(2)?;
                    let anchors: String = r.get(3)?;
                    Ok(crate::footer::Facets {
                        fact_type: r.get(0)?,
                        tags: serde_json::from_str(&tags).unwrap_or_default(),
                        triggers: serde_json::from_str(&triggers).unwrap_or_default(),
                        anchors: serde_json::from_str(&anchors).unwrap_or_default(),
                        expires: r.get(4)?,
                        provenance: r.get(5)?,
                        project: r.get(6)?,
                        mimir_id: r.get(7)?,
                    })
                },
            )
            .ok()
    }

    /// The DISPLAY body a consumer receives for one exact revision: when the
    /// STORED body is footer-free (the post-migration shape - see `thor
    /// migrate-facets-store`), compose its footer back from the facets row
    /// and append it; otherwise `body` already carries its own footer inline
    /// (the pre-migration shape, and the permanent fallback for any event a
    /// migration could not convert losslessly - see `thor
    /// verify-footer-roundtrip`), so it is returned completely unchanged.
    /// This is the ONE seam a reader should call instead of handing out
    /// `body` raw, so a migrated store looks byte-for-byte like a
    /// pre-migration one to every existing consumer - `get`/`history`/the
    /// pinned brief all go through it (see `with_display_bodies`).
    ///
    /// Deliberately checked in THIS order (inline footer first): on the
    /// CURRENT, un-migrated store every body still carries its footer
    /// inline even though a facets row also exists (M3 writes both), so
    /// composing unconditionally whenever a facets row is present would
    /// double-append the footer. Checking `extract` first makes this
    /// function a safe no-op on every store that has not been migrated,
    /// with no flag or store-version marker required.
    ///
    /// Fails open on any facets-read error (including "no such table" on a
    /// store from before M3): the safest output when nothing can be
    /// composed is exactly what has always been returned - `body`, verbatim.
    pub fn display_body(&self, entity_id: &str, rev_hash: &str, body: &str) -> String {
        if crate::footer::extract(body).is_some() {
            return body.to_string();
        }
        match self.query_facets_row(entity_id, rev_hash) {
            Some(facets) if !facets.fact_type.is_empty() => {
                crate::footer::compose_from_facets(body, &facets)
            }
            _ => body.to_string(),
        }
    }

    /// Map every event's body through `display_body` - for callers that fetch
    /// a batch of events purely to SHOW them to a consumer (the CLI/MCP
    /// `get`, `history` and pinned-brief surfaces). Internal fold/integrity
    /// code (cas, auditor, the backfill, the migration itself) must keep
    /// reading `Event.body` raw and never route through this: it exists
    /// only at the display boundary.
    pub fn with_display_bodies(&self, events: Vec<Event>) -> Vec<Event> {
        events
            .into_iter()
            .map(|mut e| {
                e.body = self.display_body(&e.entity_id, &e.this_hash, &e.body);
                e
            })
            .collect()
    }

    /// True when a facets row already exists for this exact revision (the
    /// backfill's idempotence check: re-running it must not re-count or
    /// re-write what an earlier run, or a live write, already filled in).
    pub fn facets_row_exists(&self, entity_id: &str, rev_hash: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM fact_facets WHERE entity_id = ? AND rev_hash = ?",
                params![entity_id, rev_hash],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Write one backfilled facets row (`thor backfill-facets`). Public
    /// because the CLI backfill runs over already-existing events, outside
    /// the append transaction that `apply_facets_delta` rides on live writes.
    pub fn write_facets_row(
        &self,
        entity_id: &str,
        rev_hash: &str,
        facets: &crate::footer::Facets,
    ) -> anyhow::Result<()> {
        Self::upsert_facets_row(&self.conn, entity_id, rev_hash, facets)?;
        Ok(())
    }

    /// Differential audit for the facets projection: where BOTH a facets row
    /// and a parseable footer exist for the same (entity, revision), they
    /// must describe the SAME metadata. An event's body is immutable once
    /// written, so for a given revision this can only disagree because of a
    /// write-time bug, a bad backfill, or direct tampering of the facets
    /// table - never benign staleness. Returns the mismatches found; empty =
    /// consistent.
    ///
    /// A MISSING row is deliberately not checked here at all: a store that
    /// predates this feature, or has not run `thor backfill-facets` yet, must
    /// stay green - only a row that CONTRADICTS its own event's footer is an
    /// error. Tolerant of a store that does not even have the table yet (a
    /// pre-facets store opened read-only, e.g. by `thor fsck`): that is the
    /// same "nothing to check" case, not a failure.
    ///
    /// A body carrying NO footer at all is ALSO not checked here (M4): that
    /// is the post-migration, footer-free storage shape (`thor
    /// migrate-facets-store`), where the facets row IS the metadata - there
    /// is no independent copy left in the body to compare it against. This
    /// is an accepted, permanent loss of THIS PARTICULAR cross-check for a
    /// migrated row (not an oversight): only a row whose body STILL carries
    /// a footer, and disagrees with it, can mean a write-time bug, a bad
    /// backfill, or tampering; a footer-free row's own tamper-evidence is
    /// the hash chain elsewhere, not this differential.
    pub fn verify_facets_projection(&self) -> anyhow::Result<Vec<String>> {
        let table_exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'fact_facets'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !table_exists {
            return Ok(Vec::new());
        }
        let mut issues = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT f.entity_id, f.rev_hash, f.fact_type, f.tags, f.triggers, f.anchors, \
             f.expires, f.provenance, f.project, f.mimir_id, e.body
             FROM fact_facets f
             JOIN event e ON e.entity_id = f.entity_id AND e.this_hash = f.rev_hash",
        )?;
        let rows = stmt.query_map([], |r| {
            let tags: String = r.get(3)?;
            let triggers: String = r.get(4)?;
            let anchors: String = r.get(5)?;
            let stored = crate::footer::Facets {
                fact_type: r.get(2)?,
                tags: serde_json::from_str(&tags).unwrap_or_default(),
                triggers: serde_json::from_str(&triggers).unwrap_or_default(),
                anchors: serde_json::from_str(&anchors).unwrap_or_default(),
                expires: r.get(6)?,
                provenance: r.get(7)?,
                project: r.get(8)?,
                mimir_id: r.get(9)?,
            };
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, stored, r.get::<_, String>(10)?))
        })?;
        for row in rows {
            let (entity_id, rev_hash, stored, body) = row?;
            let short = &rev_hash[..rev_hash.len().min(8)];
            match crate::footer::parse_facets(&body) {
                None if crate::footer::extract(&body).is_none() => {
                    // Footer-free storage (M4): the row is the sole record,
                    // nothing independent to cross-check it against.
                }
                None => issues.push(format!(
                    "facets row for {entity_id} rev {short} but its current body carries no \
                     parseable memory footer"
                )),
                Some(fresh) if fresh != stored => issues.push(format!(
                    "facets/footer mismatch for {entity_id} rev {short}: stored {stored:?} vs \
                     footer says {fresh:?}"
                )),
                Some(_) => {}
            }
        }
        Ok(issues)
    }

    /// One event's delta on the heads projection - the incremental mirror of
    /// the cas fold arms, applied inside the SAME transaction as the append.
    /// cas::compute_head_sets/compute_projects stay the authority; the
    /// differential fsck check and the equivalence tests pin this mirror to
    /// them. Any change to the fold MUST change both places.
    fn apply_projection_delta(
        conn: &Connection,
        seq: i64,
        kind: EventKind,
        entity_id: &str,
        parent_rev: Option<&str>,
        body: &str,
        this_hash: &str,
    ) -> SqlResult<()> {
        // Project fold mirror: birth on first sight for every kind EXCEPT a
        // reprojected event (whose arm only writes when its body parses -
        // a malformed reproject is a full no-op, not a birth).
        match kind {
            EventKind::FactReprojected => {
                if let Some(override_project) = crate::cas::parse_reproject_body(body) {
                    conn.execute(
                        "INSERT INTO entity_meta (entity_id, project) VALUES (?, ?)
                         ON CONFLICT(entity_id) DO UPDATE SET project = excluded.project",
                        params![entity_id, override_project],
                    )?;
                }
            }
            _ => {
                conn.execute(
                    "INSERT OR IGNORE INTO entity_meta (entity_id, project) VALUES (?, ?)",
                    params![entity_id, crate::repo::owner_project(entity_id)],
                )?;
            }
        }
        // Head-set fold mirror.
        match kind {
            EventKind::FactCreated => {
                conn.execute(
                    "INSERT OR IGNORE INTO head_state (entity_id, rev_hash, head_seq) VALUES (?, ?, ?)",
                    params![entity_id, this_hash, seq],
                )?;
            }
            EventKind::FactRevised | EventKind::FactSuperseded | EventKind::FactRetracted => {
                if let Some(parent) = parent_rev {
                    // Fast-forward when the cited parent IS a current head;
                    // the DELETE's row count is the membership test.
                    conn.execute(
                        "DELETE FROM head_state WHERE entity_id = ? AND rev_hash = ?",
                        params![entity_id, parent],
                    )?;
                }
                conn.execute(
                    "INSERT OR IGNORE INTO head_state (entity_id, rev_hash, head_seq) VALUES (?, ?, ?)",
                    params![entity_id, this_hash, seq],
                )?;
            }
            EventKind::FactResolved => {
                if let Some((keep_rev, discarded)) = crate::cas::parse_resolve_body(body) {
                    let current: std::collections::HashSet<String> = {
                        let mut stmt = conn.prepare(
                            "SELECT rev_hash FROM head_state WHERE entity_id = ?",
                        )?;
                        let rows = stmt.query_map(params![entity_id], |r| r.get::<_, String>(0))?;
                        rows.collect::<Result<_, _>>()?
                    };
                    let keep_also_discarded = discarded.iter().any(|d| *d == keep_rev);
                    let mut cited: std::collections::HashSet<String> =
                        discarded.iter().cloned().collect();
                    cited.insert(keep_rev.clone());
                    let valid =
                        !keep_also_discarded && current.contains(&keep_rev) && cited == current;
                    if valid {
                        for rev in &discarded {
                            conn.execute(
                                "DELETE FROM head_state WHERE entity_id = ? AND rev_hash = ?",
                                params![entity_id, rev],
                            )?;
                        }
                    }
                }
            }
            EventKind::FactReasserted | EventKind::FactEchoed | EventKind::FactReprojected => {}
        }
        Ok(())
    }

    /// Rebuild the heads projection from the whole log, inside the caller's
    /// transaction. Same delta function per event, so incremental and rebuilt
    /// state cannot diverge from each other by construction.
    fn rebuild_projection(conn: &Connection) -> SqlResult<i64> {
        conn.execute("DELETE FROM head_state", [])?;
        conn.execute("DELETE FROM entity_meta", [])?;
        let mut tip = 0i64;
        let mut stmt = conn.prepare(
            "SELECT seq, kind, entity_id, parent_rev, body, this_hash FROM event ORDER BY seq",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (seq, kind_s, entity_id, parent_rev, body, this_hash) = row?;
            let Some(kind) = EventKind::from_str(&kind_s) else {
                // An unknown kind cannot be folded; leave the projection
                // stale (tip stays behind), so readers keep falling back.
                return Err(rusqlite::Error::InvalidQuery);
            };
            Self::apply_projection_delta(
                conn,
                seq,
                kind,
                &entity_id,
                parent_rev.as_deref(),
                &body,
                &this_hash,
            )?;
            tip = seq;
        }
        conn.execute(
            "INSERT INTO projection_meta (k, v) VALUES ('tip_seq', ?)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            params![tip],
        )?;
        Ok(tip)
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

    // ---- Heads projection (M2) readers -------------------------------------
    //
    // Only meaningful when `heads_projection_current()` - every caller checks
    // that first and falls back to the cas fold otherwise. The check is two
    // index-only lookups (the contiguous-tip gap-scan mistake is deliberately
    // not repeated here: MAX(seq), never a scan).

    /// True when the heads projection covers the whole log.
    pub fn heads_projection_current(&self) -> bool {
        let tip: i64 = match self
            .conn
            .query_row("SELECT v FROM projection_meta WHERE k = 'tip_seq'", [], |r| r.get(0))
        {
            Ok(v) => v,
            Err(_) => return false,
        };
        let max_seq: i64 = match self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM event", [], |r| r.get(0))
        {
            Ok(v) => v,
            Err(_) => return false,
        };
        tip == max_seq && max_seq > 0
    }

    /// Is this rev a CURRENT head of its entity? (Point lookup on the PK.)
    pub fn projected_is_head(&self, entity_id: &str, rev_hash: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM head_state WHERE entity_id = ? AND rev_hash = ?",
                params![entity_id, rev_hash],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Number of current heads (>1 = DIVERGED).
    pub fn projected_head_count(&self, entity_id: &str) -> i64 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM head_state WHERE entity_id = ?",
                params![entity_id],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// The entity's effective project. Outer None = entity never seen;
    /// inner None = global.
    pub fn projected_project(&self, entity_id: &str) -> Option<Option<String>> {
        self.conn
            .query_row(
                "SELECT project FROM entity_meta WHERE entity_id = ?",
                params![entity_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
    }

    /// Every current head as its full event row, with the head count and the
    /// effective project of its entity - the anchored-pass workload (walk all
    /// heads once) without the O(log) fold. Ordered by entity for determinism.
    pub fn projected_head_events(&self) -> SqlResult<Vec<(Event, i64, Option<String>)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {}, (SELECT COUNT(*) FROM head_state h2 WHERE h2.entity_id = h.entity_id),
                    m.project
             FROM head_state h
             JOIN event e ON e.seq = h.head_seq
             LEFT JOIN entity_meta m ON m.entity_id = h.entity_id
             ORDER BY h.entity_id, h.rev_hash",
            EVENT_COLUMNS
                .split(", ")
                .map(|c| format!("e.{}", c))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map([], |row| {
            let ev = row_to_event(row)?;
            let n: i64 = row.get(12)?;
            let project: Option<String> = row.get(13)?;
            Ok((ev, n, project))
        })?;
        rows.collect()
    }

    /// The current heads of ONE entity as (rev_hash, head_seq) rows. Only
    /// meaningful when the projection is current.
    pub fn projected_heads_of(&self, entity_id: &str) -> SqlResult<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT rev_hash, head_seq FROM head_state WHERE entity_id = ? ORDER BY rev_hash")?;
        let rows = stmt.query_map(params![entity_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    /// One event by seq (the projected recall path fetches candidates
    /// point-wise instead of loading the whole log).
    pub fn get_event_by_seq(&self, seq: i64) -> SqlResult<Option<Event>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {} FROM event WHERE seq = ?", EVENT_COLUMNS))?;
        let mut rows = stmt.query_map(params![seq], row_to_event)?;
        rows.next().transpose()
    }

    /// Rebuild the heads projection from the log, in one transaction. The
    /// explicit form of what the next append would do anyway - for a store
    /// upgraded from a pre-M2 binary that wants the fast path before its
    /// first write (`thor fsck --rebuild-heads`).
    pub fn rebuild_heads_projection(&mut self) -> anyhow::Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let tip = Self::rebuild_projection(&tx)?;
        tx.commit()?;
        Ok(tip)
    }

    /// Differential audit: the stored projection must equal the from-scratch
    /// fold (heads, divergence and projects). Returns a list of human-readable
    /// mismatches; empty = consistent. `fsck` calls this; the M0 comment on
    /// init_schema always promised it.
    pub fn verify_heads_projection(&self) -> anyhow::Result<Vec<String>> {
        let mut issues = Vec::new();
        if !self.heads_projection_current() {
            issues.push("heads projection not current (tip_seq behind the log) - readers are on the fold fallback; the next append rebuilds it".to_string());
            return Ok(issues);
        }
        let events = self.get_all_events()?;
        let folded = crate::cas::compute_head_sets(&events);
        let folded_projects = crate::cas::compute_projects(&events);
        // Stored heads.
        let mut stored: std::collections::HashMap<String, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        {
            let mut stmt = self.conn.prepare("SELECT entity_id, rev_hash FROM head_state")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (e, h) = row?;
                stored.entry(e).or_default().insert(h);
            }
        }
        for (entity, hs) in &folded {
            match stored.get(entity) {
                Some(s) if *s == hs.heads => {}
                Some(_) => issues.push(format!("head-set mismatch for {}", entity)),
                None => issues.push(format!("entity {} missing from head_state", entity)),
            }
        }
        for entity in stored.keys() {
            if !folded.contains_key(entity) {
                issues.push(format!("stale entity {} in head_state", entity));
            }
        }
        // Stored projects.
        {
            let mut stmt = self.conn.prepare("SELECT entity_id, project FROM entity_meta")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            let mut stored_projects: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            for row in rows {
                let (e, p) = row?;
                stored_projects.insert(e, p);
            }
            for (entity, project) in &folded_projects {
                if stored_projects.get(entity) != Some(project) {
                    issues.push(format!("project mismatch for {}", entity));
                }
            }
        }
        Ok(issues)
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

        // A TOMBSTONE IS NOT A PARENT TO BUILD ON (2026-07-30, found by an audit
        // swarm). Liveness is decided everywhere - recall, the guard's anchor
        // pass, consolidate - by the KIND of the current head, and nothing here
        // looked at that kind: a revise citing a retracted head fast-forwarded
        // off the tombstone like any other head, and the fact was live again
        // with no word about it in the reply. A stale id from an earlier session
        // or a saved reference was enough. Refusing is the smaller rule and it
        // matches the store's own doctrine (a retraction is a decision, not a
        // pause): the way back is to store the corrected fact fresh, which
        // leaves a create in the log where a resurrection left nothing.
        // ...but a RETRACT of an already-retracted fact is not a revival, it is
        // the same decision arriving twice, and the inbox drain is at-least-once
        // by design (found by two adversarial verifiers, 2026-07-30: refusing it
        // made one replayed op fail its batch forever, so nothing the replica
        // captured afterwards ever reached the authority). It appends a second
        // tombstone, exactly as before, and the batch stays clean.
        if kind != EventKind::FactRetracted
            && events.iter().any(|e| e.this_hash == parent && e.kind == EventKind::FactRetracted)
        {
            return Err(conflict(
                "this fact was RETRACTED - a revise may not silently bring it back. Store the \
                 corrected fact fresh with remember (the retraction stays in history), or check \
                 with `history` whether you meant a different entity",
            )
            .into());
        }

        // Carry the fact's footer across a content-only REVISE. The footer is
        // the body's tail, not a field, so a caller who rewrites the body
        // without re-typing it silently drops the fact's type, tags, fires-when
        // vocabulary and the guard's anchors. Nothing errors and recall still
        // finds the fact, so the loss is invisible - it just stops firing at the
        // moment of action, which was the reason it was written. A new body that
        // brings its OWN footer wins (retyping stays a one-call operation).
        //
        // Revise only: a retract's body is the tombstone and a supersede points
        // elsewhere - neither should inherit the old metadata.
        let carried;
        let body = match kind {
            EventKind::FactRevised => {
                let prev = events.iter().find(|e| e.this_hash == parent);
                match prev.and_then(|p| crate::footer::carry_over(body, &p.body)) {
                    Some(b) => {
                        carried = b;
                        carried.as_str()
                    }
                    None => body,
                }
            }
            _ => body,
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

/// Ask FTS5 to verify its own inverted index, which the row-set check above
/// cannot do. The two are complements, not alternatives: verify_fts_projection
/// catches a missing, extra or orphan ROW, this catches a row that is present
/// but whose index blocks are damaged.
///
/// Why it matters: a corrupted segment inside event_fts_data leaves the row
/// count, the rowid set and the orphan set all perfectly intact, so the check
/// above reports OK - while MATCH silently returns FEWER rows, with no error.
/// Measured on this crate's bundled SQLite: 500 events, one damaged block, and
/// recall dropped to 259 of 500 while the row-set check passed. Nothing in THOR
/// can cause that (each FTS row is written in the append transaction, under WAL
/// and synchronous=FULL); it arrives from outside - a failing disk, a
/// filesystem that lied about fsync, a bad copy of a shipped replica, or
/// tampering. THOR ships stores over a wire and restores them from exports, so
/// "the program cannot cause it" is not the same as "it cannot happen here".
///
/// The `rank` argument is 0 deliberately. On a contentless table there is no
/// content table to compare against, and 1 (the "also compare the content"
/// form) behaves identically here - claiming otherwise would be theatre.
///
/// The damage is repairable without touching history: the index is a projection
/// of an append-only log, so rebuilding it loses nothing. `thor fsck --rebuild-fts`
/// does exactly that. It is deliberately NOT automatic: sync_fts returns early
/// when the counts match (which is the state after this kind of damage), and a
/// silent rewrite of an index at the moment the disk under it is suspect is the
/// wrong reflex.
pub fn verify_fts_integrity(conn: &Connection) -> Result<(), String> {
    conn.execute("INSERT INTO event_fts(event_fts, rank) VALUES('integrity-check', 0)", [])
        .map(|_| ())
        .map_err(|e| format!("FTS index is damaged: {}", e))
}

/// Rebuild the FTS index from the log, unconditionally. `sync_fts` is the
/// cheap guard that runs on every open and returns early when the counts
/// already match; this is the explicit repair for damage the counts cannot see
/// (see verify_fts_integrity). Losing the index costs nothing - it is derived.
pub fn rebuild_fts(conn: &Connection) -> Result<usize, String> {
    let db_err = |e: rusqlite::Error| format!("FTS rebuild failed: {}", e);
    conn.execute("INSERT INTO event_fts(event_fts) VALUES('delete-all')", [])
        .map_err(db_err)?;
    conn.execute(
        "INSERT INTO event_fts(rowid, body_ch) SELECT seq, body_ch FROM event",
        [],
    )
    .map_err(db_err)
}

#[cfg(test)]
mod fts_integrity_tests {
    use super::*;

    /// Damage one index block the way a bad disk would: flip a byte inside
    /// event_fts_data. Returns false when the table has no block to damage.
    ///
    /// It damages the LAST block on purpose. A contentless FTS5 index of a few
    /// dozen rows is a single segment, so damaging it takes the whole index down
    /// and even COUNT(*) fails - which proves nothing, because the row-set check
    /// would then error too. The interesting case is the realistic one: a store
    /// big enough to hold several segments, where one damaged segment leaves the
    /// row set perfectly intact and only search goes quietly lossy.
    fn damage_one_index_block(conn: &Connection) -> bool {
        let id: Option<i64> = conn
            .query_row("SELECT MAX(id) FROM event_fts_data", [], |r| r.get(0))
            .unwrap_or(None);
        let Some(id) = id else { return false };
        let mut block: Vec<u8> = match conn.query_row(
            "SELECT block FROM event_fts_data WHERE id = ?",
            params![id],
            |r| r.get(0),
        ) {
            Ok(b) => b,
            Err(_) => return false,
        };
        if block.len() < 64 {
            return false;
        }
        // Flip a byte AND drop a run of them: a lone flipped byte often lands in
        // a region FTS5 does not have to trust, so it can survive its own check.
        // Removing bytes changes the segment's structure, which is what a torn
        // write or a bad sector actually looks like.
        let mid = block.len() / 2;
        block[mid] ^= 0xFF;
        block.drain(mid + 1..mid + 20);
        conn.execute("UPDATE event_fts_data SET block = ? WHERE id = ?", params![block, id])
            .is_ok()
    }

    fn seeded(n: usize) -> EventStore {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..n {
            store
                .append_event(
                    "s",
                    "l",
                    "a",
                    EventKind::FactCreated,
                    &format!("e{i}"),
                    None,
                    &format!("alpha beta gamma delta fact number {i} with enough words to index"),
                )
                .unwrap();
        }
        store
    }

    #[test]
    fn healthy_store_passes_both_fts_checks() {
        let store = seeded(40);
        assert!(verify_fts_projection(&store.conn).is_ok(), "row set is intact");
        assert!(verify_fts_integrity(&store.conn).is_ok(), "index is intact");
    }

    /// The load-bearing test. A damaged index block leaves the row count, the
    /// rowid set and the orphan set perfectly intact, so the row-set check
    /// reports OK - which is exactly how fsck used to print "FTS projection: OK"
    /// over a store whose search had silently gone lossy.
    #[test]
    fn a_damaged_index_block_passes_the_row_set_check_and_fails_the_integrity_check() {
        let store = seeded(500);
        if !damage_one_index_block(&store.conn) {
            return; // no block to damage on this SQLite build; nothing to assert
        }
        assert!(
            verify_fts_projection(&store.conn).is_ok(),
            "the row-set check cannot see block damage - that is the whole point"
        );
        assert!(
            verify_fts_integrity(&store.conn).is_err(),
            "FTS5's own integrity check must catch what the row-set check cannot"
        );
    }

    #[test]
    fn rebuild_repairs_a_damaged_index() {
        let store = seeded(500);
        if !damage_one_index_block(&store.conn) {
            return;
        }
        assert!(verify_fts_integrity(&store.conn).is_err(), "damaged first");
        let rows = rebuild_fts(&store.conn).expect("rebuild runs");
        assert_eq!(rows, 500, "every event is re-indexed");
        assert!(verify_fts_integrity(&store.conn).is_ok(), "repaired");
        assert!(verify_fts_projection(&store.conn).is_ok(), "and still consistent");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The projection tables must equal the cas fold after EVERY append,
    /// across the full torture set of fold arms: fast-forward, stale branch,
    /// retract (both forms), valid resolve, partial/invalid resolve,
    /// keep-in-discarded resolve, resolve-then-FF, reproject (valid,
    /// malformed, before-birth), and the head-neutral kinds.
    #[test]
    fn heads_projection_equals_fold_after_every_append() {
        let mut store = EventStore::in_memory().unwrap();
        let assert_projection = |store: &EventStore, step: &str| {
            assert!(store.heads_projection_current(), "projection current after {}", step);
            let issues = store.verify_heads_projection().unwrap();
            assert!(issues.is_empty(), "after {}: {:?}", step, issues);
        };
        let a = store
            .append_event("s", "l", "t", EventKind::FactCreated, "P:mem-a", None, "a v1")
            .unwrap();
        assert_projection(&store, "create");
        let a2 = store
            .append_event("s", "l", "t", EventKind::FactRevised, "P:mem-a", Some(&a.this_hash), "a v2")
            .unwrap();
        assert_projection(&store, "fast-forward");
        store
            .append_event("s", "l", "t", EventKind::FactRevised, "P:mem-a", Some("stale"), "a branch")
            .unwrap();
        assert_projection(&store, "stale branch (diverged)");
        assert_eq!(store.projected_head_count("P:mem-a"), 2, "diverged = two heads");
        // invalid partial resolve = no-op
        let bad = serde_json::json!({ "keep_rev": a2.this_hash, "discarded": ["nonsense"] }).to_string();
        store
            .append_event("s", "l", "t", EventKind::FactResolved, "P:mem-a", None, &bad)
            .unwrap();
        assert_projection(&store, "invalid resolve (no-op)");
        assert_eq!(store.projected_head_count("P:mem-a"), 2);
        // valid resolve down to one head
        let heads: Vec<String> = {
            let events = store.get_all_events().unwrap();
            crate::cas::compute_head_sets(&events)["P:mem-a"].heads.iter().cloned().collect()
        };
        let discarded: Vec<&String> = heads.iter().filter(|h| **h != a2.this_hash).collect();
        let good = serde_json::json!({ "keep_rev": a2.this_hash, "discarded": discarded }).to_string();
        store
            .append_event("s", "l", "t", EventKind::FactResolved, "P:mem-a", None, &good)
            .unwrap();
        assert_projection(&store, "valid resolve");
        assert_eq!(store.projected_head_count("P:mem-a"), 1);
        // resolve-then-FF
        store
            .append_event("s", "l", "t", EventKind::FactRevised, "P:mem-a", Some(&a2.this_hash), "a v3")
            .unwrap();
        assert_projection(&store, "FF from resolved winner");
        // retract as fast-forward
        let b = store
            .append_event("s", "l", "t", EventKind::FactCreated, "01KGLOBALB", None, "b v1")
            .unwrap();
        store
            .append_event("s", "l", "t", EventKind::FactRetracted, "01KGLOBALB", Some(&b.this_hash), "[gone]")
            .unwrap();
        assert_projection(&store, "retract FF");
        // reproject valid, then a revise that must NOT clobber it
        let c = store
            .append_event("s", "l", "t", EventKind::FactCreated, "ProjC:mem-c", None, "c v1")
            .unwrap();
        store
            .append_event(
                "s", "l", "t", EventKind::FactReprojected, "ProjC:mem-c", None,
                &serde_json::json!({ "project": "ProjD" }).to_string(),
            )
            .unwrap();
        store
            .append_event("s", "l", "t", EventKind::FactRevised, "ProjC:mem-c", Some(&c.this_hash), "c v2")
            .unwrap();
        assert_projection(&store, "reproject + revise");
        assert_eq!(store.projected_project("ProjC:mem-c"), Some(Some("ProjD".to_string())));
        // malformed reproject = full no-op
        store
            .append_event("s", "l", "t", EventKind::FactReprojected, "ProjC:mem-c", None, "not json")
            .unwrap();
        assert_projection(&store, "malformed reproject");
        assert_eq!(store.projected_project("ProjC:mem-c"), Some(Some("ProjD".to_string())));
        // reproject BEFORE birth, then birth must not clobber the override
        store
            .append_event(
                "s", "l", "t", EventKind::FactReprojected, "ProjE:mem-e", None,
                &serde_json::json!({ "project": null }).to_string(),
            )
            .unwrap();
        store
            .append_event("s", "l", "t", EventKind::FactCreated, "ProjE:mem-e", None, "e v1")
            .unwrap();
        assert_projection(&store, "reproject before birth");
        assert_eq!(store.projected_project("ProjE:mem-e"), Some(None), "override to global sticks");
        // head-neutral echo
        store
            .append_event("s", "l", "t", EventKind::FactEchoed, "ProjC:mem-c", None, "{}")
            .unwrap();
        assert_projection(&store, "echo (head-neutral)");
    }

    /// A write that bypasses append (raw insert, the restore/pre-M2 shape)
    /// leaves the projection STALE - readers must fall back - and the next
    /// ordinary append rebuilds it in-transaction from the whole log.
    #[test]
    fn heads_projection_rebuilds_after_bypass() {
        let mut store = EventStore::in_memory().unwrap();
        let a = store
            .append_event("s", "l", "t", EventKind::FactCreated, "P:mem-a", None, "a v1")
            .unwrap();
        assert!(store.heads_projection_current());
        // bypass: raw event insert without the projection delta
        store
            .conn()
            .execute(
                "INSERT INTO event (seq, event_uuid, session_id, lineage_id, actor, kind, entity_id,
                 parent_rev, body, body_ch, prev_hash, this_hash)
                 VALUES (2, 'bypass-uuid', 's', 'l', 't', 'fact_revised', 'P:mem-a', ?, 'a v2', 'a v2', ?, 'bypass-hash')",
                params![&a.this_hash, &a.this_hash],
            )
            .unwrap();
        assert!(!store.heads_projection_current(), "bypass leaves the projection stale");
        // fold fallback still sees the truth
        let folded = crate::cas::compute_head_sets(&store.get_all_events().unwrap());
        assert_eq!(folded["P:mem-a"].heads.len(), 1);
        assert!(folded["P:mem-a"].heads.contains("bypass-hash"));
        // the next append rebuilds the whole projection
        store
            .append_event("s", "l", "t", EventKind::FactCreated, "P:mem-b", None, "b v1")
            .unwrap();
        assert!(store.heads_projection_current(), "append after bypass rebuilds");
        assert!(store.verify_heads_projection().unwrap().is_empty());
        assert!(store.projected_is_head("P:mem-a", "bypass-hash"));
    }

    /// projected_head_events serves every current head as its full event row
    /// with the entity's head count - the anchored-pass workload.
    #[test]
    fn projected_head_events_match_fold() {
        let mut store = EventStore::in_memory().unwrap();
        let a = store
            .append_event("s", "l", "t", EventKind::FactCreated, "P:mem-a", None, "a v1")
            .unwrap();
        store
            .append_event("s", "l", "t", EventKind::FactRevised, "P:mem-a", Some("stale"), "a branch")
            .unwrap();
        store
            .append_event("s", "l", "t", EventKind::FactCreated, "Q:doc.md#0", None, "doc chunk")
            .unwrap();
        let rows = store.projected_head_events().unwrap();
        let folded = crate::cas::compute_head_sets(&store.get_all_events().unwrap());
        let total: usize = folded.values().map(|h| h.heads.len()).sum();
        assert_eq!(rows.len(), total);
        for (ev, n, project) in &rows {
            assert!(folded[&ev.entity_id].heads.contains(&ev.this_hash));
            assert_eq!(*n as usize, folded[&ev.entity_id].heads.len());
            assert_eq!(
                project.as_deref(),
                crate::repo::owner_project(&ev.entity_id),
                "birth project served for {}",
                ev.entity_id
            );
        }
        assert!(rows.iter().any(|(e, n, _)| e.entity_id == "P:mem-a" && *n == 2));
        let _ = a;
    }

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

    /// A retraction is a decision, not a pause (2026-07-30, found by an audit
    /// swarm). Liveness is read everywhere from the KIND of the current head,
    /// and nothing checked that kind before accepting a parent - so a revise
    /// citing a retracted head fast-forwarded off the tombstone like any other
    /// head and the fact was simply live again, indistinguishable from one that
    /// had been revised twice, with no word about it in the reply. A stale id
    /// from an earlier session was enough to bring back something deliberately
    /// killed.
    #[test]
    fn a_retracted_fact_cannot_be_revived_by_revising_its_tombstone() {
        let mut store = EventStore::in_memory().unwrap();
        let v1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "the wrong fact")
            .unwrap();
        let dead = store
            .append_mutate_checked(
                "s", "l", "a", EventKind::FactRetracted, "e1", Some(&v1.this_hash), "wrong",
            )
            .unwrap();

        let auto = store.append_mutate_checked(
            "s", "l", "a", EventKind::FactRevised, "e1", None, "back from the dead",
        );
        let err = auto.expect_err("a revise off a tombstone must not silently revive the fact");
        let msg = format!("{err}");
        assert!(msg.contains("RETRACTED"), "the refusal says WHY: {msg}");
        assert!(msg.contains("remember"), "and names the way forward: {msg}");

        // Citing the tombstone explicitly is the same act, so it is refused too.
        let explicit = store.append_mutate_checked(
            "s", "l", "a", EventKind::FactRevised, "e1", Some(&dead.this_hash), "sneaky",
        );
        assert!(explicit.is_err(), "naming the tombstone as parent is not a loophole");

        // But a RETRACT of an already-retracted fact is the same decision
        // arriving twice, not a revival, and it must stay idempotent: the
        // replica inbox is at-least-once, and a deterministic refusal there
        // froze the whole batch (found by two adversarial verifiers).
        let again = store.append_mutate_checked(
            "s", "l", "a", EventKind::FactRetracted, "e1", None, "still wrong",
        );
        assert!(again.is_ok(), "a repeated retract must not fail: {:?}", again.err().map(|e| e.to_string()));

        // And the fact really did stay dead.
        let events = store.get_all_events().unwrap();
        let heads = crate::cas::compute_head_sets(&events);
        let head = heads.get("e1").unwrap().heads.iter().next().unwrap().clone();
        let kind = events.iter().find(|e| e.this_hash == head).map(|e| e.kind).unwrap();
        assert_eq!(kind, EventKind::FactRetracted, "the tombstone is still the head");
    }

    /// A revise that rewrites the body must keep the fact's footer: it holds
    /// the type, tags, fires-when vocabulary and the guard's ANCHORS, and losing
    /// them is invisible (recall still finds the fact - it just never fires at
    /// the moment of action again). Hit live: a revise meant to widen a memory's
    /// anchors wiped them instead.
    #[test]
    fn revise_keeps_the_facts_footer_but_retract_does_not_inherit_it() {
        let mut store = EventStore::in_memory().unwrap();
        let footer = crate::footer::compose(
            "gotcha",
            &["nas".into()],
            "global",
            &["ssh".into()],
            &["ssh admin@host".into()],
            None,
        );
        let v1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, &format!("old\n\n{footer}"))
            .unwrap();

        // content-only revise: caller sends no footer, the fact keeps its own
        let v2 = store
            .append_mutate_checked("s", "l", "a", EventKind::FactRevised, "e1", Some(&v1.this_hash), "new content")
            .unwrap();
        assert_eq!(crate::footer::strip(&v2.body), "new content");
        assert_eq!(crate::footer::anchors(&v2.body), vec!["ssh admin@host"], "anchors survive");
        assert_eq!(crate::footer::fact_type(&v2.body), Some(crate::repo::FactType::Gotcha));

        // a caller's own footer still wins (retyping in one call)
        let retyped = format!(
            "newer\n\n{}",
            crate::footer::compose("decision", &["x".into()], "global", &[], &["other".into()], None)
        );
        let v3 = store
            .append_mutate_checked("s", "l", "a", EventKind::FactRevised, "e1", Some(&v2.this_hash), &retyped)
            .unwrap();
        assert_eq!(crate::footer::anchors(&v3.body), vec!["other"], "caller's footer is respected");

        // a RETRACT must not inherit the metadata: its body is the tombstone
        let dead = store
            .append_mutate_checked("s", "l", "a", EventKind::FactRetracted, "e1", Some(&v3.this_hash), "[retracted: gone]")
            .unwrap();
        assert_eq!(dead.body, "[retracted: gone]", "tombstone stays verbatim");
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

    /// The core M3 guarantee: a facets row appears for a typed fact in the
    /// SAME call that appends it (no separate step, nothing to fall out of
    /// step with), and it matches what the footer itself says.
    #[test]
    fn facets_row_is_written_in_the_same_transaction_as_the_append() {
        let mut store = EventStore::in_memory().unwrap();
        let footer = crate::footer::compose_full(
            "gotcha",
            &["deploy".into()],
            "global",
            &["scp".into()],
            &["deploy/watcher.sh".into()],
            Some("2027-01-15"),
            Some("verified"),
        );
        let body = format!("never deploy on friday\n\n{}", footer);
        let ev = store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, &body).unwrap();

        let got = store.read_facets("e1", &ev.this_hash, &body);
        assert_eq!(got, crate::footer::parse_facets(&body).unwrap(), "row matches the footer");
        assert!(store.facets_row_exists("e1", &ev.this_hash), "the row exists right away");
    }

    /// A revise mints a NEW revision - and therefore a NEW facets row keyed on
    /// the new rev_hash - while the OLD revision's row stays exactly as it
    /// was (facets are per-revision, never mutated in place).
    #[test]
    fn revise_writes_a_new_facets_row_per_revision_and_keeps_the_old_one() {
        let mut store = EventStore::in_memory().unwrap();
        let v1_footer = crate::footer::compose("gotcha", &["a".into()], "global", &[], &[], None);
        let v1_body = format!("v1 content\n\n{}", v1_footer);
        let v1 = store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, &v1_body).unwrap();

        let v2_footer = crate::footer::compose("decision", &["b".into()], "global", &[], &[], None);
        let v2_body = format!("v2 content\n\n{}", v2_footer);
        let v2 = store
            .append_mutate_checked(
                "s", "l", "a", EventKind::FactRevised, "e1", Some(&v1.this_hash), &v2_body,
            )
            .unwrap();

        assert!(store.facets_row_exists("e1", &v1.this_hash), "v1's row is untouched");
        assert!(store.facets_row_exists("e1", &v2.this_hash), "v2 got its own row");
        let old = store.read_facets("e1", &v1.this_hash, &v1_body);
        let new = store.read_facets("e1", &v2.this_hash, &v2_body);
        assert_eq!(old.fact_type, "gotcha");
        assert_eq!(new.fact_type, "decision");
    }

    /// A fact with no footer at all gets no facets row - and reading it must
    /// still work, falling back to the (empty) footer parse. This is the
    /// unconditional additive guarantee: a store that never backfills, or a
    /// fact that was never typed, must behave exactly as before.
    #[test]
    fn a_fact_with_no_footer_gets_no_facets_row_and_reads_fall_back_cleanly() {
        let mut store = EventStore::in_memory().unwrap();
        let ev = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "a plain untyped note")
            .unwrap();
        assert!(!store.facets_row_exists("e1", &ev.this_hash), "no footer, no row");
        let got = store.read_facets("e1", &ev.this_hash, "a plain untyped note");
        assert_eq!(got, crate::footer::Facets::default(), "falls back to an empty facets value");
    }

    /// A store that predates this table (or one opened before it was ever
    /// created) must not error: `verify_facets_projection` treats a missing
    /// table exactly like zero rows, not a failure.
    #[test]
    fn verify_facets_projection_tolerates_a_store_with_no_facets_table_at_all() {
        let store = EventStore::in_memory().unwrap();
        store.conn().execute("DROP TABLE fact_facets", []).unwrap();
        let issues = store.verify_facets_projection().unwrap();
        assert!(issues.is_empty(), "a missing table is not a contradiction: {issues:?}");
    }

    /// The check's whole job: a facets row that CONTRADICTS its own event's
    /// (immutable) body is a real problem - direct tampering of the
    /// derived table, or a bad backfill - and must be reported with the
    /// mismatching entity/rev named, not silently accepted.
    #[test]
    fn verify_facets_projection_catches_a_tampered_row() {
        let mut store = EventStore::in_memory().unwrap();
        let footer = crate::footer::compose("gotcha", &["deploy".into()], "global", &[], &[], None);
        let body = format!("never deploy on friday\n\n{}", footer);
        let ev = store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, &body).unwrap();

        assert!(store.verify_facets_projection().unwrap().is_empty(), "clean before tampering");

        // Simulate tampering / a bad backfill: the row now claims a DIFFERENT
        // fact_type than the event's own (immutable) body actually carries.
        store
            .conn()
            .execute(
                "UPDATE fact_facets SET fact_type = 'decision' WHERE entity_id = 'e1' AND rev_hash = ?",
                params![ev.this_hash],
            )
            .unwrap();

        let issues = store.verify_facets_projection().unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("e1"), "{issues:?}");
        assert!(issues[0].contains("mismatch"), "{issues:?}");
    }

    /// A store with no facets rows YET (not backfilled) must stay green - the
    /// exact non-negotiable from the fsck check's own doc: missing is not an
    /// error, only a contradiction is.
    #[test]
    fn verify_facets_projection_is_clean_when_nothing_has_been_backfilled() {
        let mut store = EventStore::in_memory().unwrap();
        // An event whose body carries no footer at all never gets a row -
        // this is the everyday "not backfilled / never typed" shape.
        store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "untyped").unwrap();
        assert!(store.verify_facets_projection().unwrap().is_empty());
    }

    /// `write_facets_row` / `facets_row_exists` are the backfill's own
    /// primitives (it runs outside the append transaction, over
    /// already-existing events) - round-trip them directly.
    #[test]
    fn write_facets_row_is_idempotent_and_visible_to_facets_row_exists() {
        let mut store = EventStore::in_memory().unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "untyped").unwrap();
        let facets = crate::footer::Facets {
            fact_type: "gotcha".to_string(),
            tags: vec!["x".to_string()],
            ..Default::default()
        };
        assert!(!store.facets_row_exists("e1", "rev-a"));
        store.write_facets_row("e1", "rev-a", &facets).unwrap();
        assert!(store.facets_row_exists("e1", "rev-a"));
        // Writing again (the idempotent backfill re-run case) must not error.
        store.write_facets_row("e1", "rev-a", &facets).unwrap();
        let got = store.read_facets("e1", "rev-a", "irrelevant when a row exists");
        assert_eq!(got, facets);
    }

    /// THE CORE M4 SAFETY PROPERTY: on the CURRENT (un-migrated) storage
    /// shape every body still carries its own footer inline even though a
    /// facets row also exists (M3 writes both) - so `display_body` must
    /// return it COMPLETELY UNCHANGED, never double-appending a second,
    /// composed footer. Catches a regression that composes whenever a
    /// facets row is present instead of checking the body first.
    #[test]
    fn display_body_never_double_appends_when_the_body_still_carries_its_own_footer() {
        let mut store = EventStore::in_memory().unwrap();
        let footer = crate::footer::compose("gotcha", &["deploy".into()], "global", &[], &[], None);
        let body = format!("never deploy on friday\n\n{}", footer);
        let ev = store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, &body).unwrap();
        assert!(store.facets_row_exists("e1", &ev.this_hash), "M3 wrote a row alongside the footer");
        assert_eq!(
            store.display_body("e1", &ev.this_hash, &body),
            body,
            "a body that already carries its footer must never gain a second, composed one"
        );
    }

    /// The post-migration shape: a footer-free stored body plus a matching
    /// facets row must display EXACTLY the footer the row describes, glued
    /// back onto the content - this is the whole point of `display_body`,
    /// the read-side counterpart of `thor migrate-facets-store`.
    #[test]
    fn display_body_composes_the_footer_back_for_a_footer_free_body() {
        let store = EventStore::in_memory().unwrap();
        let facets = crate::footer::Facets {
            fact_type: "gotcha".to_string(),
            tags: vec!["deploy".to_string()],
            project: Some("global".to_string()),
            ..Default::default()
        };
        store.write_facets_row("e1", "rev-x", &facets).unwrap();
        let footer_free_body = "never deploy on friday";
        let displayed = store.display_body("e1", "rev-x", footer_free_body);
        assert_eq!(
            displayed,
            "never deploy on friday\n\n[memory/gotcha | tags: deploy | project: global]"
        );
    }

    /// A footer-free body with NO matching facets row (an event that was
    /// never typed to begin with - there is nothing to compose) must pass
    /// through unchanged, exactly like a pre-M3 store: `display_body` must
    /// never invent a footer out of nothing.
    #[test]
    fn display_body_passes_a_footer_free_body_through_when_no_facets_row_exists() {
        let store = EventStore::in_memory().unwrap();
        assert_eq!(store.display_body("e1", "rev-missing", "just a note"), "just a note");
    }

    /// `with_display_bodies` is the batch form `get`/`history` actually call:
    /// on a store that has never run `backfill-facets` (no facets rows for
    /// anything written before M3 existed - the exact shape the real store
    /// has TODAY) every event must come back byte-identical to what
    /// `get_all_events` itself returned. This is the explicit "an old store
    /// without facets just keeps working" guarantee.
    #[test]
    fn with_display_bodies_is_a_no_op_on_a_store_with_no_facets_rows_at_all() {
        let mut store = EventStore::in_memory().unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "a plain untyped note").unwrap();
        let footer = crate::footer::compose("decision", &["x".into()], "global", &[], &[], None);
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e2", None, &format!("v1\n\n{footer}"))
            .unwrap();
        let raw = store.get_all_events().unwrap();
        let displayed = store.with_display_bodies(raw.clone());
        let raw_bodies: Vec<&str> = raw.iter().map(|e| e.body.as_str()).collect();
        let displayed_bodies: Vec<&str> = displayed.iter().map(|e| e.body.as_str()).collect();
        assert_eq!(displayed_bodies, raw_bodies, "every body already carries its own footer inline");
    }

    /// The READ side of the same guarantee, taken further: even a store that
    /// has since LOST its facets table entirely (opened read-only after
    /// external damage, or a binary older than M3 touching the file) must
    /// not error or panic on `display_body` - it fails open to the body
    /// verbatim, exactly like `read_facets` already documents for any facets
    /// read error. Events are written FIRST, table dropped AFTER: dropping it
    /// before writing a typed fact is a different, pre-existing gap in the
    /// M3 write path (`apply_facets_delta` propagates a missing-table SQL
    /// error on WRITE) that this task did not touch and is not what "an old
    /// store keeps working" is about - reading is what must stay resilient.
    #[test]
    fn display_body_fails_open_when_the_facets_table_is_gone_entirely() {
        let mut store = EventStore::in_memory().unwrap();
        let footer = crate::footer::compose("decision", &["x".into()], "global", &[], &[], None);
        let body = format!("v1\n\n{footer}");
        let ev = store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, &body).unwrap();
        store.conn().execute("DROP TABLE fact_facets", []).unwrap();
        assert_eq!(store.display_body("e1", &ev.this_hash, &body), body, "the body already carries its footer");
        assert_eq!(
            store.display_body("e1", &ev.this_hash, "footer-free with no table to consult"),
            "footer-free with no table to consult",
            "no table to compose from - fails open to the body verbatim, never a panic"
        );
    }

    /// Post-migration self-consistency (M4): a facets row whose event body
    /// carries NO footer at all is the expected, everyday shape after
    /// `thor migrate-facets-store` - not a contradiction. Before this fix
    /// `verify_facets_projection` (and therefore `thor fsck`) would have
    /// flagged EVERY successfully migrated fact as "carries no parseable
    /// memory footer", which would have made fsck fail on every real
    /// migrated store - exactly the opposite of task 4's requirement.
    #[test]
    fn verify_facets_projection_accepts_a_footer_free_body_with_a_matching_row() {
        let mut store = EventStore::in_memory().unwrap();
        // A migrated event: body has no inline footer, but a facets row
        // exists for it (written by the migration, not by apply_facets_delta,
        // which never fires here since a footer-free body has no footer for
        // it to parse - insert the row directly to model that end state
        // without depending on the migration command itself).
        let ev = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "never deploy on friday")
            .unwrap();
        assert!(!store.facets_row_exists("e1", &ev.this_hash), "no footer, so no row yet - by construction");
        let facets = crate::footer::Facets {
            fact_type: "gotcha".to_string(),
            tags: vec!["deploy".to_string()],
            ..Default::default()
        };
        store.write_facets_row("e1", &ev.this_hash, &facets).unwrap();
        let issues = store.verify_facets_projection().unwrap();
        assert!(issues.is_empty(), "a footer-free body with a matching row is not a contradiction: {issues:?}");
    }
}
