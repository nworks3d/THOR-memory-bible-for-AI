//! Capture inbox: where a write lands when it arrives somewhere that is NOT
//! the authority.
//!
//! WHY THIS EXISTS AT ALL. The log is append-only with a hash chain, and a
//! replica's copy has to stay a pure prefix of the authority's - that is the
//! whole basis on which `ops::transport` can refuse a gap or a fork. So a
//! replica must never append to its own log. If the NAS connector wrote a
//! remembered fact straight into the replica store, the next shipment from
//! the PC would be rejected as a fork ("same height, different tip"), and the
//! only way back is to rebuild the replica from scratch.
//!
//! So the write is not applied there. It is serialized as one JSON line into
//! an append-only file next to the store, and the authority drains that file
//! (`sync drain`), replaying each captured call as a proper event in ITS log.
//! The result then ships back to the replica the normal, non-forking way.
//!
//! WHAT IS CAPTURED, AND WHY IT IS THE TOOL CALL. 1.0 captured a finished
//! body string, because a 1.0 fact WAS a body string. A 2.0 item is
//! structured - kind, bindings, falsifier, check, severity, project - and it
//! only becomes an item by passing the write gate, which needs the store to
//! answer questions the replica cannot answer correctly (is this target
//! already at its anchor limit, does this id already exist). Capturing the
//! finished item would therefore mean gating twice, against two different
//! stores, and trusting the wrong one.
//!
//! So what is captured is the CALL: the tool's name and its arguments,
//! verbatim. The drain replays it through the same handler the local agent
//! reaches, so the gate runs exactly once, at the authority, against the
//! authority's own store. A refusal then surfaces in the drain report rather
//! than being decided by a store that is up to an hour behind.
//!
//! THE ROTATION, AND WHY DELIVERY IS AT-LEAST-ONCE. A pull renames
//! `inbox.jsonl` to `inbox.jsonl.draining` in one atomic step, so captures
//! arriving during the drain land in a fresh file and are never lost. The
//! authority deletes the draining file only after it has applied the batch
//! (the ack). If it dies in between, the next pull serves the SAME batch
//! again - delivery is at-least-once by design, never at-most-once, because
//! losing a captured thought is worse than replaying one. That is why a
//! repeated retract has to stay idempotent in `event_store` rather than
//! failing the batch (found by two adversarial verifiers, 2026-07-30: a
//! deterministic refusal there froze one batch forever, and nothing captured
//! afterwards ever reached the authority again).

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// One captured tool call, enough to replay it verbatim at the authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxOp {
    /// The tool that was called: "remember", "revise" or "retract". Kept as a
    /// string rather than an enum on purpose - an authority running a newer
    /// build must be able to READ a capture from an older replica and say
    /// plainly that it does not know that tool, instead of failing to parse
    /// the whole file and stalling every other capture behind it.
    pub tool: String,
    /// The tool's own arguments, exactly as the caller sent them. Replayed
    /// by deserializing into that tool's argument struct, so the drain never
    /// has to know what the fields mean.
    pub args: serde_json::Value,
    /// Unix seconds at capture. Human and debug ordering only; correctness
    /// comes from file order, never from this.
    #[serde(default)]
    pub ts: String,
    /// Unique per capture, so one write can be followed across the hop from
    /// the replica's log line to the authority's drain report.
    #[serde(default)]
    pub capture_id: String,
}

impl InboxOp {
    /// A capture with its timestamp and id filled in.
    pub fn new(tool: &str, args: serde_json::Value) -> Self {
        Self {
            tool: tool.to_string(),
            args,
            ts: now_ts(),
            capture_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

/// Unix-seconds timestamp string. Best effort: used for human ordering only.
fn now_ts() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// The draining slot next to the inbox: `<inbox>.draining`.
pub fn draining_path(inbox: &Path) -> PathBuf {
    let mut s = inbox.as_os_str().to_owned();
    s.push(".draining");
    PathBuf::from(s)
}

/// Append one op as a single JSON line.
///
/// Opened with append mode so a one-line write is atomic against other
/// connections appending the same file: several agent sessions can be talking
/// to one replica connector at the same time, and a half-written line would
/// make `read_all` fail for every capture behind it, not just that one.
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

/// Read every op in file order.
///
/// A blank line is skipped. An unparseable line is a HARD error: the drain
/// must never silently drop a capture, because the only evidence that a
/// thought was ever written down is the line itself. A loud failure gets the
/// file looked at; a skipped line gets nothing.
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

/// Rotate once and read the pending captures.
///
/// If a previous batch was pulled but never acked, that draining file is
/// served AGAIN and the current inbox is left alone - re-delivering a batch
/// is recoverable, dropping one is not. Returns an empty list when there is
/// nothing on either side, which is the normal hourly answer.
pub fn rotate_and_read(inbox: &Path) -> anyhow::Result<Vec<InboxOp>> {
    let draining = draining_path(inbox);
    if !draining.exists() {
        if !inbox.exists() {
            return Ok(Vec::new());
        }
        std::fs::rename(inbox, &draining)?;
    }
    read_all(&draining)
}

/// Drop the draining file: the authority applied the batch. Absent file is
/// success, not an error - an ack that arrives twice must not be a failure,
/// for the same reason the delivery is at-least-once in the first place.
pub fn drop_draining(inbox: &Path) -> anyhow::Result<()> {
    let draining = draining_path(inbox);
    if draining.exists() {
        std::fs::remove_file(&draining)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op(tool: &str, id: &str) -> InboxOp {
        InboxOp::new(tool, json!({ "id": id, "text": "something" }))
    }

    #[test]
    fn append_then_read_all_round_trips_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        append(&path, &op("remember", "a")).unwrap();
        append(&path, &op("revise", "b")).unwrap();

        let got = read_all(&path).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].tool, "remember");
        assert_eq!(got[0].args["id"], "a");
        assert_eq!(got[1].tool, "revise");
        assert_eq!(got[1].args["id"], "b");
        assert_ne!(got[0].capture_id, got[1].capture_id, "each capture is traceable on its own");
    }

    /// THE DEFECT THIS PREVENTS: a corrupt line being skipped, so a capture
    /// disappears with nothing anywhere saying it ever existed.
    #[test]
    fn read_all_skips_blank_lines_but_refuses_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        std::fs::write(&path, "{\"tool\":\"remember\",\"args\":{}}\n\n").unwrap();
        assert_eq!(read_all(&path).unwrap().len(), 1, "a blank line is not a capture");

        std::fs::write(&path, "not json\n").unwrap();
        let err = read_all(&path).expect_err("a broken line must be loud");
        assert!(err.to_string().contains("line 1"), "the error must name the line: {err}");
    }

    #[test]
    fn rotate_moves_the_inbox_aside_so_new_captures_are_not_swept_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        append(&path, &op("remember", "first")).unwrap();

        let batch = rotate_and_read(&path).unwrap();
        assert_eq!(batch.len(), 1);
        assert!(!path.exists(), "the live inbox is moved aside, not copied");
        assert!(draining_path(&path).exists());

        // A capture arriving mid-drain lands in a fresh file and is untouched
        // by the ack that follows.
        append(&path, &op("remember", "during")).unwrap();
        drop_draining(&path).unwrap();
        assert!(!draining_path(&path).exists());

        let next = rotate_and_read(&path).unwrap();
        assert_eq!(next.len(), 1, "the capture made during the drain survives");
        assert_eq!(next[0].args["id"], "during");
    }

    /// THE DEFECT THIS PREVENTS: an authority that dies between pull and ack
    /// losing the whole batch. The next pull must serve the same batch again.
    #[test]
    fn a_batch_that_was_never_acked_is_served_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        append(&path, &op("remember", "unacked")).unwrap();

        let first = rotate_and_read(&path).unwrap();
        assert_eq!(first.len(), 1);
        // No ack: the authority crashed here.
        append(&path, &op("remember", "later")).unwrap();

        let second = rotate_and_read(&path).unwrap();
        assert_eq!(second.len(), 1, "the un-acked batch is re-served, not merged with the new one");
        assert_eq!(second[0].args["id"], "unacked");
        assert!(path.exists(), "the newer capture stays queued until the old batch is acked");
    }

    #[test]
    fn an_empty_inbox_is_an_empty_batch_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-written.jsonl");
        assert!(rotate_and_read(&path).unwrap().is_empty());
        drop_draining(&path).expect("acking nothing is not a failure");
    }

    #[test]
    fn append_creates_the_directory_it_was_pointed_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("inbox.jsonl");
        append(&path, &op("remember", "x")).unwrap();
        assert_eq!(read_all(&path).unwrap().len(), 1);
    }
}
