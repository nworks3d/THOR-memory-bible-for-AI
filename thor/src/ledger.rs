//! Local hook/debounce state in ONE SQLite sidecar (`thor-ledger.db`, next to
//! the store), following the review round that hardened the old JSON files:
//! per-key reads and writes under SQLite's own locking, so concurrent hook
//! processes (parallel PreToolUse guards, two sessions' Stop hooks, a pin from
//! the CLI while the MCP server pins too) can no longer lose each other's
//! entries or wipe the ledger via a partial read - the failure modes the
//! atomic-replace JSON interim fix could only soften.
//!
//! Four namespaces:
//! - `capture`: session_id -> unix ts (the once-per-session capture nudge;
//!   claimed with an atomic INSERT so "once" is exact under concurrency);
//! - `guard-seen`: "session|file" -> ts (positive) or {ts, neg} (negative cache);
//! - `courier-seen`: session_id -> {ts, count, seen} (per-session injection ledger);
//! - `pins`: one row holding the pinned entity-id list (mutated in a write
//!   transaction, so concurrent pin/unpin serialize instead of last-wins).
//!
//! This is LOCAL state (like the flag files), never part of the hash-chained
//! log: deleting `thor-ledger.db` only resets debounces - it can never lose a
//! fact. Every operation is fail-open: any error degrades to "no state" (a
//! repeat advisory at worst), never to blocking a hook. Legacy JSON sidecars
//! are imported once on first open and then left untouched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Entries older than this are pruned on write, so the ledger never grows unbounded.
pub const PRUNE_AGE_SECS: u64 = 48 * 60 * 60;

/// THOR's per-user data directory (store, rulebooks, ledgers, flag files):
/// %LOCALAPPDATA%\thor on Windows, else $XDG_DATA_HOME/thor, else
/// $HOME/.local/share/thor. `None` when no per-user base dir is resolvable -
/// callers must treat that as "no store", NEVER fall back to a cwd-relative
/// path: hooks run with cwd = the user's project, so a relative path would
/// plant store files inside the repo and OPEN whatever thor.db a cloned repo
/// ships (attacker-controlled "memories" injected as trusted context).
pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME").ok().map(|h| Path::new(&h).join(".local").join("share"))
        })?;
    Some(base.join("thor"))
}

/// True when a flag file (THOR-SILENT.flag, THOR-PRIMARY.flag) sits next to the
/// store. Flag files ARE the flip valve: create or delete one to change phase
/// with NO code change and NO settings edit. Shared by the courier and the
/// guard so the kill switch silences every THOR surface consistently. Flags
/// deliberately stay FILES (not ledger rows): flipping must never require a
/// working SQLite open.
pub fn flag_present(db: &Path, name: &str) -> bool {
    db.parent().map(|dir| dir.join(name).exists()).unwrap_or(false)
}

fn ledger_path(db: &Path) -> PathBuf {
    db.with_file_name("thor-ledger.db")
}

/// Open (and initialize) the ledger sidecar. `None` on any failure - callers
/// then behave statelessly, exactly like a missing JSON sidecar used to.
fn conn(db: &Path) -> Option<rusqlite::Connection> {
    let c = rusqlite::Connection::open(ledger_path(db)).ok()?;
    let _ = c.busy_timeout(std::time::Duration::from_millis(250));
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS kv (
           ns TEXT NOT NULL,
           k  TEXT NOT NULL,
           v  TEXT NOT NULL,
           ts INTEGER NOT NULL,
           PRIMARY KEY (ns, k)
         );",
    )
    .ok()?;
    migrate_legacy_json(&c, db);
    Some(c)
}

/// One-time import of the pre-SQLite JSON sidecars, so live debounce state and
/// (critically) the PINNED standing rules survive the upgrade. Idempotent via a
/// meta row; the legacy files are left in place (dead) rather than deleted.
fn migrate_legacy_json(c: &rusqlite::Connection, db: &Path) {
    let migrated: Option<i64> = c
        .query_row("SELECT 1 FROM kv WHERE ns='meta' AND k='migrated'", [], |r| r.get(0))
        .ok();
    if migrated.is_some() {
        return;
    }
    let now = crate::review::now_secs();
    for (ns, file) in [
        ("capture", "thor-capture.json"),
        ("guard-seen", "thor-guard-seen.json"),
        ("courier-seen", "thor-courier-seen.json"),
    ] {
        for (k, v) in legacy_read_map(&db.with_file_name(file)) {
            let ts = v
                .as_u64()
                .or_else(|| v.get("ts").and_then(|t| t.as_u64()))
                .unwrap_or(now);
            let _ = c.execute(
                "INSERT OR IGNORE INTO kv (ns, k, v, ts) VALUES (?, ?, ?, ?)",
                rusqlite::params![ns, k, v.to_string(), ts as i64],
            );
        }
    }
    let legacy_pins = legacy_read_pins(&db.with_file_name("thor-pins.json"));
    if !legacy_pins.is_empty() {
        let _ = c.execute(
            "INSERT OR IGNORE INTO kv (ns, k, v, ts) VALUES ('pins', 'list', ?, ?)",
            rusqlite::params![serde_json::json!(legacy_pins).to_string(), now as i64],
        );
    }
    let _ = c.execute(
        "INSERT OR IGNORE INTO kv (ns, k, v, ts) VALUES ('meta', 'migrated', '1', ?)",
        rusqlite::params![now as i64],
    );
}

fn legacy_read_map(path: &Path) -> HashMap<String, serde_json::Value> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Object(obj)) => obj.into_iter().collect(),
        _ => HashMap::new(),
    }
}

fn legacy_read_pins(path: &Path) -> Vec<String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("pins").and_then(|p| p.as_array()).map(|a| {
                a.iter().filter_map(|s| s.as_str().map(String::from)).collect()
            })
        })
        .unwrap_or_default()
}

/// Read one entry. `None` = absent or any error (fail-open).
pub fn get(db: &Path, ns: &str, key: &str) -> Option<serde_json::Value> {
    let c = conn(db)?;
    let raw: String = c
        .query_row("SELECT v FROM kv WHERE ns=? AND k=?", rusqlite::params![ns, key], |r| r.get(0))
        .ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write one entry (last write wins for THIS key only - other keys are never
/// touched, which is the whole point vs the old whole-map JSON writes) and
/// prune entries past PRUNE_AGE_SECS in the same namespace.
pub fn upsert(db: &Path, ns: &str, key: &str, value: &serde_json::Value) {
    let now = crate::review::now_secs();
    if let Some(c) = conn(db) {
        let _ = c.execute(
            "INSERT INTO kv (ns, k, v, ts) VALUES (?, ?, ?, ?)
             ON CONFLICT (ns, k) DO UPDATE SET v=excluded.v, ts=excluded.ts",
            rusqlite::params![ns, key, value.to_string(), now as i64],
        );
        let _ = c.execute(
            "DELETE FROM kv WHERE ns=? AND ts < ?",
            rusqlite::params![ns, (now.saturating_sub(PRUNE_AGE_SECS)) as i64],
        );
    }
}

/// Atomically claim a key: true iff the row was newly inserted. False when it
/// already exists OR on any error - for a once-per-session nudge, an error must
/// mean silence, never a repeat. This is the exactly-once primitive the JSON
/// contains-then-insert could not provide under concurrency.
pub fn insert_once(db: &Path, ns: &str, key: &str, value: &serde_json::Value) -> bool {
    let now = crate::review::now_secs();
    match conn(db) {
        Some(c) => c
            .execute(
                "INSERT OR IGNORE INTO kv (ns, k, v, ts) VALUES (?, ?, ?, ?)",
                rusqlite::params![ns, key, value.to_string(), now as i64],
            )
            .map(|inserted| inserted == 1)
            .unwrap_or(false),
        None => false,
    }
}

/// Atomically increment a counter entry (created at 1). Used for the access
/// namespace: reads (MCP get, recall serves) are a RANKING/decay signal, never
/// facts - they live here in the local ledger and must never bloat the synced
/// hash-chained log. Fail-open like every ledger op.
pub fn increment(db: &Path, ns: &str, key: &str) {
    let now = crate::review::now_secs();
    if let Some(c) = conn(db) {
        let _ = c.execute(
            "INSERT INTO kv (ns, k, v, ts) VALUES (?, ?, '1', ?)
             ON CONFLICT (ns, k) DO UPDATE SET
               v = CAST(COALESCE(CAST(kv.v AS INTEGER), 0) + 1 AS TEXT),
               ts = excluded.ts",
            rusqlite::params![ns, key, now as i64],
        );
    }
}

/// Read a counter written by increment(). 0 = absent or any error.
pub fn counter(db: &Path, ns: &str, key: &str) -> u64 {
    get(db, ns, key).and_then(|v| v.as_u64()).unwrap_or(0)
}

/// Counters for the GIVEN keys only (`k IN (...)`): the hot-path read. The
/// access/noise namespaces grow one row per entity ever read/marked and are
/// never pruned, so a per-prompt caller must never scan the whole namespace.
pub fn counters_for(db: &Path, ns: &str, keys: &[String]) -> HashMap<String, u64> {
    if keys.is_empty() {
        return HashMap::new();
    }
    let Some(c) = conn(db) else { return HashMap::new() };
    let placeholders = vec!["?"; keys.len()].join(",");
    let sql = format!("SELECT k, v FROM kv WHERE ns = ? AND k IN ({})", placeholders);
    let Ok(mut stmt) = c.prepare(&sql) else { return HashMap::new() };
    let params = std::iter::once(ns.to_string()).chain(keys.iter().cloned());
    let mut out = HashMap::new();
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    });
    if let Ok(rows) = rows {
        for (k, v) in rows.flatten() {
            if let Ok(n) = v.parse::<u64>() {
                out.insert(k, n);
            }
        }
    }
    out
}

/// All counters in a namespace (for consolidate's decay scan - an OFFLINE
/// command; per-prompt callers use counters_for). Empty on error.
pub fn counters(db: &Path, ns: &str) -> HashMap<String, u64> {
    let Some(c) = conn(db) else { return HashMap::new() };
    let mut out = HashMap::new();
    let Ok(mut stmt) = c.prepare("SELECT k, v FROM kv WHERE ns = ?") else {
        return out;
    };
    let rows = stmt.query_map([ns], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    });
    if let Ok(rows) = rows {
        for (k, v) in rows.flatten() {
            if let Ok(n) = v.parse::<u64>() {
                out.insert(k, n);
            }
        }
    }
    out
}

/// Remove one entry (absent = no-op).
pub fn remove(db: &Path, ns: &str, key: &str) {
    if let Some(c) = conn(db) {
        let _ = c.execute("DELETE FROM kv WHERE ns=? AND k=?", rusqlite::params![ns, key]);
    }
}

/// Remove every entry in `ns` whose key starts with `prefix` (the post-compaction
/// reset for one session's "session|file" guard-seen entries).
pub fn remove_prefix(db: &Path, ns: &str, prefix: &str) {
    if prefix.is_empty() {
        return;
    }
    if let Some(c) = conn(db) {
        // ESCAPE so a '%' or '_' in a session id cannot widen the match.
        let pattern = format!(
            "{}%",
            prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        let _ = c.execute(
            "DELETE FROM kv WHERE ns=? AND k LIKE ? ESCAPE '\\'",
            rusqlite::params![ns, pattern],
        );
    }
}

/// The pinned entity ids (`thor pin` / the post-compaction brief). Order is
/// preserved; any error is an empty list (fail-open).
pub fn read_pins(db: &Path) -> Vec<String> {
    get(db, "pins", "list")
        .and_then(|v| {
            v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        })
        .unwrap_or_default()
}

/// Read-modify-write the pin list inside ONE immediate write transaction, so
/// concurrent pin/unpin (CLI + MCP server, two sessions) serialize instead of
/// last-write-wins dropping a pin. Returns the resulting list.
pub fn mutate_pins<F>(db: &Path, mutate: F) -> std::io::Result<Vec<String>>
where
    F: FnOnce(Vec<String>) -> Vec<String>,
{
    let err = |e: String| std::io::Error::other(e);
    let mut c = conn(db).ok_or_else(|| err("ledger unavailable".into()))?;
    let tx = c
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| err(e.to_string()))?;
    let current: Vec<String> = tx
        .query_row("SELECT v FROM kv WHERE ns='pins' AND k='list'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default();
    let next = mutate(current);
    tx.execute(
        "INSERT INTO kv (ns, k, v, ts) VALUES ('pins', 'list', ?, ?)
         ON CONFLICT (ns, k) DO UPDATE SET v=excluded.v, ts=excluded.ts",
        rusqlite::params![serde_json::json!(next).to_string(), crate::review::now_secs() as i64],
    )
    .map_err(|e| err(e.to_string()))?;
    tx.commit().map_err(|e| err(e.to_string()))?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_upsert_remove_roundtrip_and_failopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(get(&db, "guard-seen", "s1|a.rs").is_none(), "absent -> None");
        upsert(&db, "guard-seen", "s1|a.rs", &serde_json::json!(123));
        assert_eq!(get(&db, "guard-seen", "s1|a.rs").and_then(|v| v.as_u64()), Some(123));
        // another namespace is invisible
        assert!(get(&db, "capture", "s1|a.rs").is_none());
        remove(&db, "guard-seen", "s1|a.rs");
        assert!(get(&db, "guard-seen", "s1|a.rs").is_none());
    }

    #[test]
    fn insert_once_claims_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(insert_once(&db, "capture", "s1", &serde_json::json!(1)), "first claim wins");
        assert!(!insert_once(&db, "capture", "s1", &serde_json::json!(2)), "second claim loses");
        assert_eq!(get(&db, "capture", "s1").and_then(|v| v.as_u64()), Some(1), "value is the winner's");
    }

    #[test]
    fn remove_prefix_only_hits_that_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        upsert(&db, "guard-seen", "s1|a.rs", &serde_json::json!(1));
        upsert(&db, "guard-seen", "s1|b.rs", &serde_json::json!(2));
        upsert(&db, "guard-seen", "s2|a.rs", &serde_json::json!(3));
        remove_prefix(&db, "guard-seen", "s1|");
        assert!(get(&db, "guard-seen", "s1|a.rs").is_none());
        assert!(get(&db, "guard-seen", "s1|b.rs").is_none());
        assert!(get(&db, "guard-seen", "s2|a.rs").is_some(), "other sessions untouched");
        remove_prefix(&db, "guard-seen", ""); // empty prefix must never wipe
        assert!(get(&db, "guard-seen", "s2|a.rs").is_some());
    }

    #[test]
    fn pins_roundtrip_and_transactional_mutate() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(read_pins(&db).is_empty());
        let after = mutate_pins(&db, |mut pins| {
            pins.push("e1".to_string());
            pins
        })
        .unwrap();
        assert_eq!(after, vec!["e1"]);
        mutate_pins(&db, |mut pins| {
            pins.push("e2".to_string());
            pins
        })
        .unwrap();
        assert_eq!(read_pins(&db), vec!["e1", "e2"], "order preserved");
        mutate_pins(&db, |mut pins| {
            pins.retain(|p| p != "e1");
            pins
        })
        .unwrap();
        assert_eq!(read_pins(&db), vec!["e2"]);
    }

    #[test]
    fn legacy_json_migrates_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        // legacy sidecars from the JSON era: pins + a capture debounce
        std::fs::write(db.with_file_name("thor-pins.json"), r#"{"pins":["rule-1","rule-2"]}"#)
            .unwrap();
        std::fs::write(db.with_file_name("thor-capture.json"), r#"{"s9":1234}"#).unwrap();
        assert_eq!(read_pins(&db), vec!["rule-1", "rule-2"], "pins survive the upgrade");
        assert!(
            !insert_once(&db, "capture", "s9", &serde_json::json!(9)),
            "a migrated capture debounce still counts as claimed"
        );
        // migration runs once: a legacy file appearing later is ignored
        std::fs::write(db.with_file_name("thor-pins.json"), r#"{"pins":["late"]}"#).unwrap();
        assert_eq!(read_pins(&db), vec!["rule-1", "rule-2"], "no re-import after the marker");
    }
}
