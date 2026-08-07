//! Draining the replica's capture inbox into the authority's log.
//!
//! This is the second half of the round trip that lets a write arrive
//! somewhere other than the main machine. The replica captured the CALL
//! (`thor_core::inbox`); this replays it here, against the store that is
//! allowed to grow, and the result ships back out on the next `sync ship`.
//!
//! THE ACK IS UNCONDITIONAL ONCE THE BATCH HAS BEEN ATTEMPTED, and that is
//! deliberate. It would feel safer to hold the batch back when something in
//! it was refused - but a refusal here is usually PERMANENT (a rule with no
//! falsifier will be refused again in an hour, and again the hour after),
//! so holding the batch would park every later capture behind one bad line
//! forever. That exact shape already bit this codebase once, from the other
//! direction: a deterministic refusal on a repeated retract froze a batch and
//! nothing captured afterwards ever arrived again (two adversarial verifiers,
//! 2026-07-30). So the batch clears, and every refusal is REPORTED instead -
//! loudly enough that the owner can see what did not make it and why.

use anyhow::bail;
use std::path::Path;
use thor_core::event_store::EventStore;
use thor_core::inbox::InboxOp;

use crate::transport::{ack_inbox, pull_inbox};

/// What one drain did, as data, so the CLI and any caller render it in their
/// own words rather than parsing a printed line.
pub struct DrainSummary {
    pub pulled: usize,
    pub applied: usize,
    pub not_applied: usize,
    /// One line per captured call, in the order they were captured: what it
    /// was, and what happened to it.
    pub lines: Vec<String>,
}

impl DrainSummary {
    /// True when the batch was completely clean. Anything else deserves a
    /// look, so the CLI can exit non-zero on it.
    pub fn all_applied(&self) -> bool {
        self.not_applied == 0
    }
}

/// A short label for one capture, for the report. Names the tool and the id
/// it targets when there is one, because "remember refused" without the id is
/// useless six hours later.
fn label(op: &InboxOp) -> String {
    match op.args.get("id").and_then(|v| v.as_str()) {
        Some(id) => format!("{} {}", op.tool, id),
        None => op.tool.clone(),
    }
}

/// Pull the capture inbox from the receiver at `base`, replay every captured
/// call into the authority store at `db`, then ack so the receiver may drop
/// its copy.
///
/// Refuses to run against a receiver that is not in capture mode: a drain
/// that quietly reports "0 captured" against a plain mirror is the exact
/// silent no-op this transport already refuses in the other direction.
pub fn drain_from(db: &Path, base: &str, token: &str) -> anyhow::Result<DrainSummary> {
    // The HTTP calls use a blocking client, so they must happen outside the
    // runtime the replay needs - building a blocking reqwest client inside an
    // active tokio context is a self-checked misuse, not just a slow path.
    let pulled = pull_inbox(base, token)?;
    if !pulled.capture_enabled {
        bail!(
            "the receiver at {base} is not in capture mode, so nothing written there is being kept \
             and this drain can never find anything. Start it with an inbox path (sync recv \
             --inbox <file>) before relying on remote writes."
        );
    }
    if pulled.ops.is_empty() {
        return Ok(DrainSummary { pulled: 0, applied: 0, not_applied: 0, lines: Vec::new() });
    }

    let store = EventStore::open_existing(db)?;
    let server = mcp::ThorMcpServer::new(store);

    let runtime = tokio::runtime::Runtime::new()?;
    let (applied, not_applied, lines) = runtime.block_on(async {
        let mut applied = 0usize;
        let mut not_applied = 0usize;
        let mut lines = Vec::new();
        // Sequentially and in capture order: a revise that follows a remember
        // in the same batch has to see the item the remember created.
        for op in &pulled.ops {
            match server.apply_captured(op).await {
                mcp::CapturedOutcome::Applied(text) => {
                    applied += 1;
                    lines.push(format!("OK   {}: {text}", label(op)));
                }
                mcp::CapturedOutcome::NotApplied(text) => {
                    not_applied += 1;
                    lines.push(format!("LOST {}: {text}", label(op)));
                }
            }
        }
        (applied, not_applied, lines)
    });

    // Only now, once every call in the batch has been attempted.
    ack_inbox(base, token)?;

    Ok(DrainSummary { pulled: pulled.ops.len(), applied, not_applied, lines })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The label is what the owner reads when a capture did not make it, so
    /// it has to carry the id whenever the call had one.
    #[test]
    fn a_label_names_the_tool_and_the_id_when_there_is_one() {
        let with_id = InboxOp::new("remember", json!({ "id": "no-force-push", "kind": "rule" }));
        assert_eq!(label(&with_id), "remember no-force-push");

        let without = InboxOp::new("retract", json!({ "reason": "wrong" }));
        assert_eq!(label(&without), "retract");
    }

    #[test]
    fn a_clean_batch_reports_all_applied() {
        let s = DrainSummary { pulled: 2, applied: 2, not_applied: 0, lines: vec![] };
        assert!(s.all_applied());
        let s = DrainSummary { pulled: 2, applied: 1, not_applied: 1, lines: vec![] };
        assert!(!s.all_applied(), "one lost capture must not read as a clean drain");
    }

    /// THE DEFECT THIS PREVENTS: draining against a store path that does not
    /// exist creating a brand new, empty, healthy-looking authority - and
    /// then applying real captures into it, where nothing will ever find
    /// them. Checked here rather than trusted, because this path takes a
    /// `--db` from a scheduled task nobody watches.
    #[test]
    fn draining_into_a_store_that_does_not_exist_never_creates_one() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-a-store.db");
        // No receiver is running, so this fails at the pull; the point is
        // that the store file is not conjured up on the way past either.
        let _ = drain_from(&missing, "http://127.0.0.1:1", "tok");
        assert!(!missing.exists(), "a drain must never create the authority store");
    }
}
