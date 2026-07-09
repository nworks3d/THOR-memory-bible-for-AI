//! Fail-open JSON sidecars next to the store, following the review.rs watermark
//! pattern: any read/parse failure yields the empty default and any write failure
//! is ignored, so a ledger can never block or break the hook paths that use it.
//!
//! Three consumers:
//! - the Stop-hook capture nudge (`thor-capture.json`: session_id -> unix ts);
//! - the PreToolUse memory advisory (`thor-guard-seen.json`: "session|file" -> ts);
//! - the courier's per-session injection ledger (`thor-courier-seen.json`:
//!   session_id -> { ts, count, seen: { "<rev>|<diverged>": at_count } }).
//!
//! Ledgers are LOCAL state (like the flag files), never part of the hash-chained
//! log, so deleting one only resets a debounce - it can never lose a fact.
//!
//! Writes are atomic-replace (temp file + rename in the same directory): a
//! concurrent reader sees the old or the new ledger, never a truncated one -
//! else its fail-open "malformed -> empty" path could make it REWRITE the
//! ledger from empty and wipe every session's debounce state. Concurrent
//! read-modify-write can still lose the slower writer's entry (cost: one
//! repeated advisory/nudge, fail-open by design); the race-free fix is moving
//! this state into SQLite, tracked as a later step.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Entries older than this are pruned on write, so ledgers never grow unbounded.
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

pub fn capture_ledger_path(db: &Path) -> PathBuf {
    db.with_file_name("thor-capture.json")
}

pub fn guard_seen_path(db: &Path) -> PathBuf {
    db.with_file_name("thor-guard-seen.json")
}

pub fn courier_seen_path(db: &Path) -> PathBuf {
    db.with_file_name("thor-courier-seen.json")
}

pub fn pins_path(db: &Path) -> PathBuf {
    db.with_file_name("thor-pins.json")
}

/// True when a flag file (THOR-SILENT.flag, THOR-PRIMARY.flag) sits next to the
/// store. Flag files ARE the flip valve: create or delete one to change phase
/// with NO code change and NO settings edit. Shared by the courier and the
/// guard so the kill switch silences every THOR surface consistently.
pub fn flag_present(db: &Path, name: &str) -> bool {
    db.parent().map(|dir| dir.join(name).exists()).unwrap_or(false)
}

/// Read a JSON-object ledger as a string map. Fail-open: a missing, unreadable,
/// or malformed file is an empty map (the caller then behaves statelessly).
pub fn read_map(path: &Path) -> HashMap<String, serde_json::Value> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Object(obj)) => obj.into_iter().collect(),
        _ => HashMap::new(),
    }
}

/// Atomic-replace write: temp file in the SAME directory (rename is only atomic
/// within one filesystem), unique per process so concurrent writers never share
/// a temp file. On both Unix and Windows the rename replaces the destination.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Best-effort write (fail-open: an IO error is swallowed, matching the
/// review.rs watermark contract - state loss only means a repeat nudge).
pub fn write_map(path: &Path, map: &HashMap<String, serde_json::Value>) {
    let obj: serde_json::Map<String, serde_json::Value> =
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let _ = write_atomic(path, &serde_json::Value::Object(obj).to_string());
}

/// Drop entries whose timestamp (extracted by `ts_of`; None = undatable, drop it
/// too) is older than PRUNE_AGE_SECS. Called on write so stale sessions age out.
pub fn prune_old<F>(map: &mut HashMap<String, serde_json::Value>, now: u64, ts_of: F)
where
    F: Fn(&serde_json::Value) -> Option<u64>,
{
    map.retain(|_, v| match ts_of(v) {
        Some(ts) => now.saturating_sub(ts) <= PRUNE_AGE_SECS,
        None => false,
    });
}

/// The pinned entity ids (`thor pin` / the post-compaction brief). Order is
/// preserved; a malformed file is an empty list (fail-open).
pub fn read_pins(db: &Path) -> Vec<String> {
    let raw = match std::fs::read_to_string(pins_path(db)) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    v.get("pins")
        .and_then(|p| p.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

pub fn write_pins(db: &Path, pins: &[String]) -> std::io::Result<()> {
    let v = serde_json::json!({ "pins": pins });
    write_atomic(&pins_path(db), &v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_roundtrip_and_failopen() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.json");
        assert!(read_map(&p).is_empty(), "missing file -> empty map");
        std::fs::write(&p, "not json").unwrap();
        assert!(read_map(&p).is_empty(), "malformed file -> empty map (fail-open)");
        let mut m = HashMap::new();
        m.insert("s1".to_string(), serde_json::json!(123));
        write_map(&p, &m);
        assert_eq!(read_map(&p).get("s1").and_then(|v| v.as_u64()), Some(123));
    }

    #[test]
    fn prune_drops_old_and_undatable() {
        let mut m = HashMap::new();
        m.insert("fresh".to_string(), serde_json::json!(1_000_000));
        m.insert("old".to_string(), serde_json::json!(1_000_000 - PRUNE_AGE_SECS - 1));
        m.insert("junk".to_string(), serde_json::json!("no ts"));
        prune_old(&mut m, 1_000_000, |v| v.as_u64());
        assert!(m.contains_key("fresh"));
        assert!(!m.contains_key("old"), "past the prune age -> dropped");
        assert!(!m.contains_key("junk"), "undatable -> dropped");
    }

    #[test]
    fn pins_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(read_pins(&db).is_empty());
        write_pins(&db, &["e1".to_string(), "e2".to_string()]).unwrap();
        assert_eq!(read_pins(&db), vec!["e1", "e2"]);
        std::fs::write(pins_path(&db), "garbage").unwrap();
        assert!(read_pins(&db).is_empty(), "malformed pins file -> empty (fail-open)");
    }
}
