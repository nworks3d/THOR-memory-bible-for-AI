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

    // THE VERSION GUARD, and it runs BEFORE a single call is applied.
    //
    // A replica running a newer build queues calls this authority has never
    // heard of. Discovering that one op at a time, halfway through a batch,
    // meant the batch was acked anyway and those writes were gone. Checking
    // the whole batch up front means the opposite: nothing is applied, nothing
    // is acked, and the queue is served again unchanged on the next drain (see
    // `inbox::rotate_and_read`, which re-serves an un-acked batch rather than
    // dropping it). So the fix is a deploy, not a recovery - update this
    // machine to the replica's build and drain again, and everything lands.
    //
    // Deliberately the whole batch and not just the unknown calls: applying
    // the known ones now would ack them away, and then the un-acked remainder
    // could never be replayed in the order it was captured.
    let unknown = mcp::unreplayable_tools(&pulled.ops);
    if !unknown.is_empty() {
        bail!(
            "this machine cannot replay {} of the queued call(s) - the connector is running a NEWER \
             build than this one. NOTHING was applied and the queue was NOT cleared, so no write is \
             lost: update this machine to the connector's build and drain again. Unknown here: {}",
            unknown.len(),
            unknown.join(", ")
        );
    }

    let store = EventStore::open_existing(db)?;
    // The replay server needs the SAME library the local session uses, or a
    // filing captured on the replica arrives here and is told there is no
    // library - which is how a queued recipe becomes a stranded one. Found by
    // draining a real queue rather than by reasoning about it.
    let server = mcp::ThorMcpServer::new(store).with_library(library::default_path(db));

    let runtime = tokio::runtime::Runtime::new()?;
    let (applied, not_applied, mut lines, stranded) = runtime.block_on(async {
        let mut applied = 0usize;
        let mut not_applied = 0usize;
        let mut lines = Vec::new();
        let mut stranded: Vec<&InboxOp> = Vec::new();
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
                    stranded.push(op);
                }
            }
        }
        (applied, not_applied, lines, stranded)
    });

    // NOTHING IS DROPPED WITHOUT A COPY, added 2026-08-17 after auditing this
    // path rather than after losing something.
    //
    // The ack above is unconditional on purpose (see this module's own header:
    // a permanent refusal must never park every later capture behind it). But
    // "cleared from the queue" was being read as "handled", and one refusal in
    // particular is NOT permanent: "unknown captured tool" simply means the
    // replica runs a newer build, and its own message even promised "the
    // capture is kept until it applies" - which was false, because the ack
    // drops the batch either way. Updating the authority fixed nothing,
    // because the call was already gone.
    //
    // So every call that did not land is written out beside the store first.
    // The queue still clears, the report still says what happened, and the
    // writes themselves are still on disk to look at or replay by hand.
    if !stranded.is_empty() {
        let path = stranded_path(db);
        match append_stranded(&path, &stranded) {
            Ok(()) => lines.push(format!(
                "the {} call(s) above were kept in {} - the queue clears either way, so this file is the only copy",
                stranded.len(),
                path.display()
            )),
            // Never let this stop the drain: the batch has already been
            // applied, and failing here would only strand it a second time.
            Err(e) => lines.push(format!(
                "WARNING: the {} call(s) above could NOT be written to {} ({e}), so they exist nowhere now",
                stranded.len(),
                path.display()
            )),
        }
    }

    // Only now, once every call in the batch has been attempted.
    ack_inbox(base, token)?;

    Ok(DrainSummary { pulled: pulled.ops.len(), applied, not_applied, lines })
}

/// Where a call that could not be replayed is parked: beside the store it was
/// meant for, so it travels with a backup of that store and is never somewhere
/// only this one machine knows about.
pub fn stranded_path(db: &Path) -> std::path::PathBuf {
    let mut name = db.file_name().unwrap_or_else(|| std::ffi::OsStr::new("thor.db")).to_os_string();
    name.push(".stranded-captures.jsonl");
    db.with_file_name(name)
}

/// Append the calls, one JSON object per line, in the order they were captured.
/// Appended and never rewritten: an earlier drain's strandings stay put.
fn append_stranded(path: &Path, ops: &[&InboxOp]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    for op in ops {
        writeln!(file, "{}", serde_json::to_string(op)?)?;
    }
    Ok(())
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
