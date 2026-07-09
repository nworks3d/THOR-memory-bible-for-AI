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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Entries older than this are pruned on write, so ledgers never grow unbounded.
pub const PRUNE_AGE_SECS: u64 = 48 * 60 * 60;

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

/// Best-effort write (fail-open: an IO error is swallowed, matching the
/// review.rs watermark contract - state loss only means a repeat nudge).
pub fn write_map(path: &Path, map: &HashMap<String, serde_json::Value>) {
    let obj: serde_json::Map<String, serde_json::Value> =
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let _ = std::fs::write(path, serde_json::Value::Object(obj).to_string());
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
    std::fs::write(pins_path(db), v.to_string())
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
