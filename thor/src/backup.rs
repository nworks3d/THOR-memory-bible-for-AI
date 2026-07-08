//! GitHub backup + restore for THOR's event log.
//!
//! The backup IS the event log, exported as canonical append-only JSONL. Because
//! the log only ever grows, each day's export is a near-pure git append -
//! delta-compresses to almost nothing, diffs are human-readable, retention is
//! just git history. Restore replays the log into a fresh store and REQUIRES
//! every replayed `this_hash` to equal the recorded one, so a restore that does
//! not faithfully reconstruct the store fails loudly instead of silently
//! producing a different memory. (Replay-determinism is THOR's own proven M0
//! property; this makes it the backup's integrity guarantee.)

use crate::event_store::{EventKind, EventStore};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Do not push more than once per this many hours (debounce for the SessionStart hook).
const DEBOUNCE_HOURS: u64 = 20;

fn git(repo: &Path, args: &[&str]) -> anyhow::Result<()> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Age in hours of the last commit that touched thor/, or None if there is none.
fn last_thor_commit_age_hours(repo: &Path) -> Option<u64> {
    let out = Command::new("git")
        .arg("-C").arg(repo)
        .args(["log", "-1", "--format=%ct", "--", "thor/"])
        .output().ok()?;
    let ts: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(ts) / 3600)
}

/// Automated backup: export the log to <repo>/thor/events.jsonl, then commit and
/// push (git handles the credentials). Debounced to once per DEBOUNCE_HOURS
/// unless `force`. THOR only ever touches thor/ - it pulls --rebase first so it
/// never collides with mimir's root-level backup in the same repo. Returns a
/// human-readable status line.
pub fn backup_to_repo(store: &EventStore, repo: &Path, force: bool) -> anyhow::Result<String> {
    if !force {
        if let Some(age) = last_thor_commit_age_hours(repo) {
            if age < DEBOUNCE_HOURS {
                return Ok(format!("backup is {age}h old (< {DEBOUNCE_HOURS}h) - skipping"));
            }
        }
    }
    let thor_dir = repo.join("thor");
    std::fs::create_dir_all(&thor_dir)?;
    let n = {
        let mut f = std::fs::File::create(thor_dir.join("events.jsonl"))?;
        export_jsonl(store, &mut f)?
    };
    // sync with the shared repo (mimir's daily task also pushes here), then stage thor/ only
    git(repo, &["pull", "--rebase", "--autostash", "origin", "main"])?;
    git(repo, &["add", "thor/"])?;
    // nothing changed? do not make an empty commit
    let clean = Command::new("git").arg("-C").arg(repo)
        .args(["diff", "--cached", "--quiet", "--", "thor/"]).status()?.success();
    if clean {
        return Ok(format!("no change since last backup ({n} events) - nothing to commit"));
    }
    git(repo, &["commit", "-m", &format!("thor backup ({n} events)")])?;
    git(repo, &["push", "origin", "main"])?;
    Ok(format!("pushed thor backup ({n} events)"))
}

/// Write the whole event log as one JSON object per line, ordered by seq.
/// Returns the number of events written.
pub fn export_jsonl(store: &EventStore, out: &mut impl Write) -> anyhow::Result<usize> {
    let mut events = store.get_all_events()?;
    events.sort_by_key(|e| e.seq);
    for e in &events {
        let rec = serde_json::json!({
            "seq": e.seq,
            "session_id": e.session_id,
            "lineage_id": e.lineage_id,
            "actor": e.actor,
            "kind": e.kind.as_str(),
            "entity_id": e.entity_id,
            "parent_rev": e.parent_rev,
            "body": e.body,
            "this_hash": e.this_hash,
        });
        writeln!(out, "{}", serde_json::to_string(&rec)?)?;
    }
    Ok(events.len())
}

/// Replay an exported log into `store` (which MUST be empty) in seq order,
/// verifying replay-determinism: every reconstructed `this_hash` must equal the
/// recorded one. Returns the number of events restored. Fails if the store is
/// not empty, a line is malformed, a kind is unknown, or any hash diverges.
pub fn restore_jsonl(store: &mut EventStore, reader: impl BufRead) -> anyhow::Result<usize> {
    if !store.get_all_events()?.is_empty() {
        anyhow::bail!("restore target is not empty - restore only into a fresh store");
    }
    // Parse all lines, then sort by seq so an out-of-order file still replays
    // in chain order.
    let mut recs: Vec<(i64, Value)> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        let seq = v.get("seq").and_then(|x| x.as_i64()).ok_or_else(|| anyhow::anyhow!("record missing seq"))?;
        recs.push((seq, v));
    }
    recs.sort_by_key(|(seq, _)| *seq);

    let s = |v: &Value, k: &str| -> anyhow::Result<String> {
        Ok(v.get(k).and_then(|x| x.as_str()).ok_or_else(|| anyhow::anyhow!("record missing field {k}"))?.to_string())
    };
    for (seq, v) in &recs {
        let kind_str = s(v, "kind")?;
        let kind = EventKind::from_str(&kind_str).ok_or_else(|| anyhow::anyhow!("unknown kind {kind_str} at seq {seq}"))?;
        let parent = v.get("parent_rev").and_then(|x| x.as_str());
        let ev = store.append_event(
            &s(v, "session_id")?,
            &s(v, "lineage_id")?,
            &s(v, "actor")?,
            kind,
            &s(v, "entity_id")?,
            parent,
            &s(v, "body")?,
        )?;
        let recorded = s(v, "this_hash")?;
        if ev.this_hash != recorded {
            anyhow::bail!(
                "replay mismatch at seq {seq}: reconstructed {} != recorded {recorded} - the backup does not faithfully reconstruct the store",
                ev.this_hash
            );
        }
    }
    Ok(recs.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn seed(store: &mut EventStore) -> Vec<String> {
        let a = store.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "first body").unwrap();
        let b = store.append_event("s", "l", "act", EventKind::FactRevised, "e1", Some(&a.this_hash), "second body").unwrap();
        store.append_event("s", "l", "act", EventKind::FactCreated, "e2", None, "other entity").unwrap();
        vec![a.this_hash, b.this_hash]
    }

    #[test]
    fn test_export_restore_roundtrip_is_bit_identical() {
        let mut src = EventStore::in_memory().unwrap();
        let src_hashes = seed(&mut src);
        // export
        let mut buf: Vec<u8> = Vec::new();
        let n = export_jsonl(&src, &mut buf).unwrap();
        assert_eq!(n, 3);
        // restore into a fresh store
        let mut dst = EventStore::in_memory().unwrap();
        let restored = restore_jsonl(&mut dst, Cursor::new(&buf)).unwrap();
        assert_eq!(restored, 3);
        // the restored store is byte-identical: same events, same hashes
        let src_all = src.get_all_events().unwrap();
        let dst_all = dst.get_all_events().unwrap();
        assert_eq!(src_all.len(), dst_all.len());
        for (a, b) in src_all.iter().zip(dst_all.iter()) {
            assert_eq!(a.this_hash, b.this_hash, "restored hash must match original");
            assert_eq!(a.body, b.body);
            assert_eq!(a.entity_id, b.entity_id);
        }
        // the head hashes survive (the head of e1 is the revised rev)
        assert!(dst_all.iter().any(|e| e.this_hash == src_hashes[1]));
    }

    #[test]
    fn test_restore_refuses_nonempty_store() {
        let mut dst = EventStore::in_memory().unwrap();
        seed(&mut dst);
        let err = restore_jsonl(&mut dst, Cursor::new(b"".to_vec()));
        assert!(err.is_err(), "restore must refuse a non-empty target");
    }

    #[test]
    fn test_restore_detects_tampered_hash() {
        let mut src = EventStore::in_memory().unwrap();
        seed(&mut src);
        let mut buf: Vec<u8> = Vec::new();
        export_jsonl(&src, &mut buf).unwrap();
        // corrupt a body but keep the recorded this_hash -> replay must diverge
        let tampered = String::from_utf8(buf).unwrap().replace("second body", "TAMPERED body");
        let mut dst = EventStore::in_memory().unwrap();
        let err = restore_jsonl(&mut dst, Cursor::new(tampered.into_bytes()));
        assert!(err.is_err(), "a tampered body must fail the replay-hash check");
    }
}
