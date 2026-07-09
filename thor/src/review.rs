//! Scope review: surface recently-added GLOBAL memories that carry NO project signal
//! (no `<project>:` prefix, no reproject, no mimir footer), so the agent can offer to
//! reproject the project-specific ones. This is the "propose, you confirm" safety net
//! for facts that landed global (e.g. remembered in a remote/cwd-less session).
//!
//! A watermark file next to the store records the last-reviewed seq and the last time
//! the SessionStart cue was shown, so the prompt fires at most once per DEBOUNCE window
//! and never re-surfaces already-reviewed facts.

use crate::event_store::{Event, EventKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Only re-prompt at SessionStart once per this window (seconds).
pub const DEBOUNCE_SECS: u64 = 24 * 60 * 60;

fn watermark_path(db: &Path) -> PathBuf {
    db.with_file_name("thor-review.json")
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Watermark {
    pub reviewed_seq: i64,
    pub prompted_at: u64,
}

pub fn read_watermark(db: &Path) -> Watermark {
    let raw = match std::fs::read_to_string(watermark_path(db)) {
        Ok(s) => s,
        Err(_) => return Watermark::default(),
    };
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    Watermark {
        reviewed_seq: v.get("reviewed_seq").and_then(|x| x.as_i64()).unwrap_or(0),
        prompted_at: v.get("prompted_at").and_then(|x| x.as_u64()).unwrap_or(0),
    }
}

pub fn write_watermark(db: &Path, wm: Watermark) {
    let v = serde_json::json!({ "reviewed_seq": wm.reviewed_seq, "prompted_at": wm.prompted_at });
    let _ = std::fs::write(watermark_path(db), v.to_string());
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A memory id with a mimir import footer is already project-attributed (or
/// confirmed global) by mimir - it carries a reliable signal, so it is not a review
/// candidate.
fn has_footer(body: &str) -> bool {
    body.contains("| project: ")
}

/// GLOBAL memories with no project signal, created after `reviewed_seq`. "No signal"
/// = an unprefixed id (born global), never reprojected, and no mimir footer. These
/// are the facts that may actually belong to a project (e.g. remembered cwd-less).
/// Returns (entity_id, first_body_line, create_seq), oldest first.
pub fn candidates(events: &[Event], reviewed_seq: i64) -> Vec<(String, String, i64)> {
    // first (create) seq + body per entity, and whether it was ever reprojected.
    let mut first_seq: HashMap<&str, i64> = HashMap::new();
    let mut create_body: HashMap<&str, &str> = HashMap::new();
    let mut reprojected: HashMap<&str, bool> = HashMap::new();
    for e in events {
        first_seq.entry(&e.entity_id).or_insert(e.seq);
        create_body.entry(&e.entity_id).or_insert(&e.body);
        if matches!(e.kind, EventKind::FactReprojected) {
            reprojected.insert(&e.entity_id, true);
        }
    }
    let mut out: Vec<(String, String, i64)> = Vec::new();
    for (eid, &seq) in &first_seq {
        if seq <= reviewed_seq {
            continue; // already reviewed
        }
        if eid.contains(':') {
            continue; // has a project prefix (chunk or scoped memory) - not global-no-signal
        }
        if *reprojected.get(eid).unwrap_or(&false) {
            continue; // already reprojected (touched deliberately)
        }
        let body = create_body.get(eid).copied().unwrap_or("");
        if has_footer(body) {
            continue; // mimir-attributed - trusted signal
        }
        let first_line = body.trim().lines().next().unwrap_or("").chars().take(90).collect();
        out.push((eid.to_string(), first_line, seq));
    }
    out.sort_by_key(|(_, _, seq)| *seq);
    out
}

/// The current max seq (the tip) - used to advance the watermark on `--mark`.
pub fn max_seq(events: &[Event]) -> i64 {
    events.iter().map(|e| e.seq).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventStore;

    #[test]
    fn candidates_only_no_signal_globals_after_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::new(&dir.path().join("r.db")).unwrap();
        // 1: an old global memory (before the watermark) - excluded
        store.append_event("s", "l", "a", EventKind::FactCreated, "mcp-old", None, "old global note").unwrap();
        // 2: a mimir-footer global - trusted, excluded
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "01KFOOT", None,
                "a fact\n\n[memory/note | project: global | mimir:01KFOOT]")
            .unwrap();
        // 3: a project-prefixed memory - not global, excluded
        store.append_event("s", "l", "a", EventKind::FactCreated, "ProjA:mem-x", None, "scoped").unwrap();
        // 4: a no-signal global memory AFTER the watermark - THE candidate
        let e4 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "mcp-new", None, "a new business decision")
            .unwrap();
        // 5: a no-signal global that was reprojected - already handled, excluded
        store.append_event("s", "l", "a", EventKind::FactCreated, "mcp-done", None, "handled").unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactReprojected, "mcp-done", None, r#"{"project":"ProjA"}"#)
            .unwrap();

        let events = store.get_all_events().unwrap();
        // watermark just below e4's seq so the two old globals (mcp-old, 01KFOOT) are past-watermark too,
        // proving the footer/prefix/reproject filters (not just the seq) do the work.
        let cands = candidates(&events, 0);
        let ids: Vec<&str> = cands.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(ids.contains(&"mcp-new"), "the no-signal global is a candidate");
        assert!(!ids.contains(&"01KFOOT"), "a mimir-footer global is trusted, not a candidate");
        assert!(!ids.contains(&"ProjA:mem-x"), "a project-prefixed memory is not a global candidate");
        assert!(!ids.contains(&"mcp-done"), "an already-reprojected memory is not a candidate");
        assert!(ids.contains(&"mcp-old"), "mcp-old is also a no-signal global (only the seq filter excludes it)");

        // watermark at e4's seq excludes everything up to and including it
        let cands2 = candidates(&events, e4.seq);
        assert!(cands2.iter().all(|(id, _, _)| id != "mcp-new"), "watermark excludes reviewed seqs");
    }
}
