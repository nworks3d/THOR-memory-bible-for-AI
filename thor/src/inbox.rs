//! Capture inbox: when THOR runs as a REPLICA (the NAS remote MCP), a write from
//! the mobile/web connector must NOT append to the local hash-chained event log.
//! That would fork the sync with the PC authority - the replica's log has to stay
//! a pure prefix of the authority's, or the next `thor ship` is rejected as a
//! fork ("replayed hash does not match - shipping cannot heal this").
//!
//! Instead the write is serialized as one JSON line into an append-only inbox
//! file (env THOR_CAPTURE_INBOX picks the path; presence of that env is what puts
//! the MCP server in divert mode - see mcp::ThorServer::from_shared). The
//! authority drains it with `thor drain-inbox`, run from the PC's hourly ship
//! job: it replays each op as a proper event in ITS log and ships the result
//! back, so the capture reaches the replica the normal, non-forking way.
//!
//! The inbox is deliberately NOT part of thor.db: the replica store is then only
//! ever touched by `thor recv`, so it can never diverge. The trade-off is that a
//! capture is not visible in the replica's own recall until the next drain+ship
//! (bounded by the ship interval); it is a capture channel, not a live write.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// One captured write, enough to replay it verbatim on the authority.
///
/// `body` already carries its composed footer (`remember` composes it before the
/// divert point), so the drain never re-derives footer state - footer.rs stays
/// the sole owner of that format. The mobile-assigned `entity_id` is preserved so
/// a later revise/retract in the same inbox chains onto the right entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxOp {
    /// "create" | "revise" | "retract".
    pub op: String,
    pub entity_id: String,
    pub body: String,
    /// The CAS parent for revise/retract (None auto-fills a single head on apply).
    #[serde(default)]
    pub parent_rev: Option<String>,
    /// Unix seconds at capture, for human/debug ordering only (not correctness).
    #[serde(default)]
    pub ts: String,
    /// Unique per capture, for logging/traceability across the divert->drain hop.
    #[serde(default)]
    pub capture_id: String,
}

/// Unix-seconds timestamp string. Best-effort; only used for human ordering.
pub fn now_ts() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Append one op as a single JSON line. `O_APPEND` makes a one-line write atomic
/// against other MCP connections appending the same file (POSIX small-write
/// atomicity), so concurrent captures never interleave a partial line.
pub fn append(path: &Path, op: &InboxOp) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut line = serde_json::to_string(op)?;
    line.push('\n');
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// Read every op in file order. A blank line is skipped; an unparseable line is a
/// hard error (the drain must never silently drop a capture).
pub fn read_all(path: &Path) -> anyhow::Result<Vec<InboxOp>> {
    let f = std::fs::File::open(path)?;
    let mut ops = Vec::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let op: InboxOp =
            serde_json::from_str(t).map_err(|e| anyhow::anyhow!("inbox line {}: {e}", i + 1))?;
        ops.push(op);
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_then_read_all_roundtrips_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        let a = InboxOp {
            op: "create".into(),
            entity_id: "proj:mem-a".into(),
            body: "first\n\n[memory/note | project: proj]".into(),
            parent_rev: None,
            ts: "1".into(),
            capture_id: "c1".into(),
        };
        let b = InboxOp {
            op: "revise".into(),
            entity_id: "proj:mem-a".into(),
            body: "second".into(),
            parent_rev: Some("deadbeef".into()),
            ts: "2".into(),
            capture_id: "c2".into(),
        };
        append(&path, &a).unwrap();
        append(&path, &b).unwrap();
        let got = read_all(&path).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].op, "create");
        assert_eq!(got[0].entity_id, "proj:mem-a");
        assert_eq!(got[0].body, a.body);
        assert_eq!(got[1].op, "revise");
        assert_eq!(got[1].parent_rev.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn read_all_skips_blank_lines_but_errors_on_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        std::fs::write(&path, "{\"op\":\"create\",\"entity_id\":\"x\",\"body\":\"b\"}\n\n").unwrap();
        assert_eq!(read_all(&path).unwrap().len(), 1);
        std::fs::write(&path, "not json\n").unwrap();
        assert!(read_all(&path).is_err());
    }
}
