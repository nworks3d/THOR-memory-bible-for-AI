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

/// Age in hours of the last commit reachable from local `HEAD` that touched
/// `subdir`, or None if there is none. Used only when there is no upstream
/// to measure from instead - see `last_commit_age_hours_at`.
fn last_commit_age_hours(repo: &Path, subdir: &str) -> Option<u64> {
    last_commit_age_hours_at(repo, subdir, "HEAD")
}

/// `last_commit_age_hours`, measured from an arbitrary `rev` instead of
/// always `HEAD` - the seam `backup_to_repo` uses to measure the debounce
/// from the upstream (what the REMOTE actually has) rather than the local
/// branch. A commit that was made locally but never pushed must not make
/// the NEXT run think a backup just landed - see this file's own doc
/// comment on the defect this closes.
fn last_commit_age_hours_at(repo: &Path, subdir: &str, rev: &str) -> Option<u64> {
    let pathspec = format!("{subdir}/");
    let out = Command::new("git")
        .arg("-C").arg(repo)
        .args(["log", "-1", "--format=%ct", rev, "--", &pathspec])
        .output().ok()?;
    let ts: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(ts) / 3600)
}

/// Whether the branch checked out in `repo` has a configured upstream at
/// all (`@{u}`, git's own name for it) - a repo with no remote configured,
/// or a branch never set to track one, has no upstream to compare against,
/// and every check below falls back to today's LOCAL-only behaviour rather
/// than guessing at one. Deliberately NOT hard-coded to `origin/main`: the
/// branch actually checked out here may track a different remote, a
/// different branch name, or both - see `upstream_remote_and_branch`.
fn has_upstream(repo: &Path) -> bool {
    Command::new("git")
        .arg("-C").arg(repo)
        .args(["rev-parse", "--verify", "-q", "@{u}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// How many commits local `HEAD` carries that the upstream (`@{u}`) does
/// not - the exact count of backup commits a previous run made but could
/// not push. `None` when it cannot be determined (git failed to run at
/// all) - callers already gate this behind `has_upstream`, so an
/// unparseable count here is treated the same as "nothing to catch up"
/// rather than guessed at.
fn commits_ahead_of_upstream(repo: &Path) -> Option<u64> {
    let out = Command::new("git")
        .arg("-C").arg(repo)
        .args(["rev-list", "--count", "@{u}..HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The checked-out branch's configured upstream, split into (remote,
/// branch) - e.g. `("origin", "main")` for a branch tracking `origin/main`,
/// but just as well `("upstream", "release/2.0")` for one tracking
/// `upstream/release/2.0` (a branch name may itself contain `/`, so only
/// the FIRST `/` splits remote from branch - a remote name never does).
/// `None` with no upstream configured, the same case `has_upstream` reports
/// `false` for; callers fall back to today's `origin`/`main` pair then, so
/// a repo with no upstream at all keeps behaving exactly as before this
/// existed.
fn upstream_remote_and_branch(repo: &Path) -> Option<(String, String)> {
    let out = Command::new("git")
        .arg("-C").arg(repo)
        .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let full = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (remote, branch) = full.split_once('/')?;
    Some((remote.to_string(), branch.to_string()))
}

/// Automated backup: export the log to <repo>/<subdir>/events.jsonl, then commit
/// and push (git handles the credentials). Debounced to once per DEBOUNCE_HOURS
/// unless `force`. Only ever touches `subdir` - it pulls --rebase first so it
/// never collides with anything else living in the same repo. Returns a
/// human-readable status line.
///
/// `subdir` is a parameter, and this is the one place this file departs from the
/// 1.0 original it was ported from. 1.0 hardcoded "thor". A 2.0 store is a
/// DIFFERENT hash chain over the same memory, so writing it to the same
/// `thor/events.jsonl` would overwrite 1.0's backup with a log that does not
/// continue it - destroying the fallback at the exact moment a switchover makes
/// you need it. Two chains, two directories, no shared file.
pub fn backup_to_repo(
    store: &EventStore,
    repo: &Path,
    subdir: &str,
    force: bool,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        !subdir.is_empty()
            && !subdir.contains('/')
            && !subdir.contains('\\')
            && subdir != "..",
        "backup subdirectory must be a single plain directory name, got {subdir:?}"
    );

    // A previous run may have committed and then failed to PUSH (network
    // down, the NAS asleep, whatever) - that commit is still sitting on the
    // local branch, invisible to anyone who only ever reads the remote.
    // Push it now, before anything else and regardless of the debounce
    // below: the data is only actually backed up once it leaves this
    // machine, and the debounce exists to limit how often we commit, never
    // to sit on a commit that already happened.
    let upstream = has_upstream(repo);
    // Resolved once, from the branch actually checked out here - falls back
    // to today's `origin`/`main` pair only when no upstream is configured
    // at all, so that case keeps behaving exactly as it did before this
    // existed (see `upstream_remote_and_branch`'s own doc comment).
    let (remote, branch) = upstream_remote_and_branch(repo).unwrap_or_else(|| ("origin".to_string(), "main".to_string()));
    let mut pushed_note: Option<String> = None;
    if upstream {
        if let Some(ahead) = commits_ahead_of_upstream(repo) {
            if ahead > 0 {
                git(repo, &["push", &remote, &branch])?;
                pushed_note = Some(format!("pushed {ahead} backup commit(s) that had not reached the remote"));
            }
        }
    }
    let prefix = |msg: String| match &pushed_note {
        Some(note) => format!("{note}; {msg}"),
        None => msg,
    };

    if !force {
        // Measured from what the REMOTE has, not the local branch - a local
        // commit that never reached the upstream must not make the NEXT run
        // think a backup just landed (see this file's own doc comment). With
        // no upstream configured at all, there is nothing to measure from
        // but the local branch, so that stays today's behaviour unchanged.
        let age = if upstream {
            last_commit_age_hours_at(repo, subdir, "@{u}")
        } else {
            last_commit_age_hours(repo, subdir)
        };
        if let Some(age) = age {
            if age < DEBOUNCE_HOURS {
                return Ok(prefix(format!("backup is {age}h old (< {DEBOUNCE_HOURS}h) - skipping")));
            }
        }
    }
    let out_dir = repo.join(subdir);
    std::fs::create_dir_all(&out_dir)?;
    let n = {
        let mut f = std::fs::File::create(out_dir.join("events.jsonl"))?;
        export_jsonl(store, &mut f)?
    };
    // sync with the shared repo (other backups push here too), then stage ours only
    let pathspec = format!("{subdir}/");
    git(repo, &["pull", "--rebase", "--autostash", &remote, &branch])?;
    git(repo, &["add", &pathspec])?;
    // nothing changed? do not make an empty commit
    let clean = Command::new("git").arg("-C").arg(repo)
        .args(["diff", "--cached", "--quiet", "--", &pathspec]).status()?.success();
    if clean {
        return Ok(prefix(format!("no change since last backup ({n} events) - nothing to commit")));
    }
    git(repo, &["commit", "-m", &format!("{subdir} backup ({n} events)")])?;
    git(repo, &["push", &remote, &branch])?;
    Ok(prefix(format!("pushed {subdir} backup ({n} events)")))
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
    fn test_reproject_survives_roundtrip() {
        let mut src = EventStore::in_memory().unwrap();
        src.append_event("s", "l", "act", EventKind::FactCreated, "ProjA:mem-x", None, "a decision")
            .unwrap();
        src.append_event(
            "s",
            "l",
            "act",
            EventKind::FactReprojected,
            "ProjA:mem-x",
            None,
            r#"{"project":"ProjB"}"#,
        )
        .unwrap();
        let mut buf: Vec<u8> = Vec::new();
        export_jsonl(&src, &mut buf).unwrap();
        let mut dst = EventStore::in_memory().unwrap();
        let n = restore_jsonl(&mut dst, Cursor::new(&buf)).unwrap();
        assert_eq!(n, 2, "the fact_reprojected event replays with a verified hash");
        // the effective project reassignment travelled to the fresh replica
        let projects = crate::cas::compute_projects(&dst.get_all_events().unwrap());
        assert_eq!(projects["ProjA:mem-x"], Some("ProjB".to_string()));
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

    /// The defect this guards against: a subdirectory carrying a path could
    /// escape the directory it is supposed to own - `--subdir ../thor` would
    /// write over 1.0's backup, which is the one fallback a switchover must
    /// not destroy. Refused before anything is exported or staged.
    #[test]
    fn a_subdirectory_that_is_a_path_is_refused() {
        let store = EventStore::in_memory().unwrap();
        let repo = std::path::Path::new(".");
        for bad in ["", "..", "../thor", "a/b", "a\\b"] {
            let err = backup_to_repo(&store, repo, bad, true);
            assert!(
                err.is_err(),
                "subdir {bad:?} must be refused before any git command runs"
            );
        }
    }

    // ---------------------------------------------- real-git fixture tests
    //
    // Everything below runs the real `git` binary against a throwaway bare
    // remote plus a working clone - the only way to prove the catch-up push
    // (step 2) against the actual failure shape it exists to heal: a commit
    // that landed locally while a push to origin failed.

    /// Run `git <args>` in `dir`, panicking with stdout+stderr on failure -
    /// test fixture plumbing only. Production code never panics on a failed
    /// git call; see `git` above, which turns failure into an `Err`.
    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {} failed:\nstdout: {}\nstderr: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A bare remote plus a working clone tracking it on `main`, with one
    /// initial (empty) commit already pushed so `origin/main` exists - the
    /// smallest fixture every test below builds on.
    struct GitFixture {
        _dir: tempfile::TempDir,
        remote: std::path::PathBuf,
        work: std::path::PathBuf,
    }

    fn make_git_fixture() -> GitFixture {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        git_ok(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
        let out = Command::new("git")
            .args(["clone", remote.to_str().unwrap(), work.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "clone failed: {}", String::from_utf8_lossy(&out.stderr));
        git_ok(&work, &["checkout", "-b", "main"]);
        git_ok(&work, &["config", "user.email", "test@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Test"]);
        git_ok(&work, &["commit", "--allow-empty", "-m", "initial"]);
        git_ok(&work, &["push", "-u", "origin", "main"]);
        GitFixture { _dir: dir, remote, work }
    }

    /// THE EXACT DEFECT, end to end: a run whose commit lands locally but
    /// whose push fails (here, a broken PUSH url - the deterministic way to
    /// get "pull succeeds, push fails" without racing a real network) must
    /// leave that commit stuck ahead of `origin/main`. The NEXT run, even
    /// well inside the debounce window, must push the stuck commit first
    /// and say so - never silently wait out another DEBOUNCE_HOURS while
    /// origin stays behind.
    #[test]
    fn a_commit_that_never_reached_origin_is_pushed_on_the_next_run_and_reported() {
        let fx = make_git_fixture();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);

        // Break ONLY the push URL: `pull` (fetch) still reaches the real
        // remote and succeeds, so the run gets all the way to `commit`
        // before `push` fails - exactly the shape backup_to_repo's own doc
        // comment on this defect describes.
        git_ok(&fx.work, &["remote", "set-url", "--push", "origin", "/no/such/path"]);

        let first = backup_to_repo(&store, &fx.work, "thor2", true);
        assert!(first.is_err(), "a broken push URL must fail the run: {first:?}");
        assert_eq!(
            commits_ahead_of_upstream(&fx.work),
            Some(1),
            "the commit must land locally even though the push failed"
        );

        // Restore connectivity, then run again well inside the debounce
        // window (force = false): the catch-up push must still happen.
        git_ok(&fx.work, &["remote", "set-url", "--push", "origin", fx.remote.to_str().unwrap()]);
        let second = backup_to_repo(&store, &fx.work, "thor2", false).unwrap();
        assert!(
            second.contains("pushed 1 backup commit(s) that had not reached the remote"),
            "must say what it caught up: {second}"
        );
        assert_eq!(commits_ahead_of_upstream(&fx.work), Some(0), "origin must now have the commit");
    }

    /// The two "nothing to catch up" shapes together: right after a clean
    /// push, local and upstream are equal, so the new catch-up logic must
    /// add nothing to the output and must read exactly as it always has -
    /// and a second, forced run with nothing new to commit must still
    /// succeed rather than fail.
    #[test]
    fn local_equal_to_upstream_is_unchanged_and_a_forced_no_op_run_still_succeeds() {
        let fx = make_git_fixture();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);

        let out = backup_to_repo(&store, &fx.work, "thor2", true).unwrap();
        assert!(
            out.starts_with("pushed thor2 backup ("),
            "a first real backup with nothing ahead of origin must read exactly as before: {out}"
        );
        assert!(!out.contains("had not reached the remote"));
        assert_eq!(commits_ahead_of_upstream(&fx.work), Some(0));

        // Forced again, nothing changed: must not fail, and must read
        // exactly as the existing "nothing to commit" line always has.
        let out2 = backup_to_repo(&store, &fx.work, "thor2", true);
        assert!(out2.is_ok(), "a forced run with nothing new to commit must not fail: {out2:?}");
        assert_eq!(out2.unwrap(), "no change since last backup (3 events) - nothing to commit");
    }

    /// A repo with no `origin` remote at all (or one whose `main` was never
    /// fetched) has nothing to catch up and nothing to measure the debounce
    /// against upstream with - the debounce must keep working from local
    /// history alone, exactly as it did before this file learned about
    /// upstreams, and it must never attempt a push that would only fail
    /// loudly against a remote that does not exist.
    #[test]
    fn no_upstream_configured_keeps_todays_local_only_debounce() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        git_ok(dir.path(), &["init", "-b", "main", work.to_str().unwrap()]);
        git_ok(&work, &["config", "user.email", "test@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(work.join("thor2")).unwrap();
        std::fs::write(work.join("thor2").join("events.jsonl"), "{}\n").unwrap();
        git_ok(&work, &["add", "thor2/"]);
        git_ok(&work, &["commit", "-m", "prior local backup, no remote configured yet"]);

        let store = EventStore::in_memory().unwrap();
        let out = backup_to_repo(&store, &work, "thor2", false).unwrap();
        assert!(
            out.contains("h old (< 20h) - skipping"),
            "with no origin at all, the debounce must still work from local history alone: {out}"
        );
        assert!(!out.contains("had not reached the remote"));
    }

    /// THE DEFECT STEP 2c CLOSES: every upstream check here used to be
    /// hard-coded to `origin/main`. A branch tracking a DIFFERENT remote
    /// name AND a DIFFERENT branch name - so a hard-coded pair could not
    /// accidentally still work - proves the catch-up push, the debounce
    /// age check, and the final pull/push all resolve the REAL configured
    /// upstream (`@{u}`) instead.
    #[test]
    fn catch_up_and_backup_work_against_an_upstream_that_is_not_origin_main() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        git_ok(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
        let out = Command::new("git")
            .args(["clone", remote.to_str().unwrap(), work.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "clone failed: {}", String::from_utf8_lossy(&out.stderr));
        git_ok(&work, &["checkout", "-b", "release"]);
        git_ok(&work, &["config", "user.email", "test@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Test"]);
        git_ok(&work, &["remote", "rename", "origin", "upstream"]);
        git_ok(&work, &["commit", "--allow-empty", "-m", "initial"]);
        git_ok(&work, &["push", "-u", "upstream", "release"]);

        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);

        // Break ONLY the push URL, the same deterministic shape the
        // origin/main fixture above uses: `pull` (fetch) still reaches the
        // real remote and succeeds, so the run gets all the way to `commit`
        // before `push` fails.
        git_ok(&work, &["remote", "set-url", "--push", "upstream", "/no/such/path"]);
        let first = backup_to_repo(&store, &work, "thor2", true);
        assert!(first.is_err(), "a broken push URL must fail the run: {first:?}");
        assert_eq!(
            commits_ahead_of_upstream(&work),
            Some(1),
            "commits_ahead_of_upstream must resolve @{{u}} (upstream/release here), not a hard-coded \
             origin/main that does not exist in this fixture"
        );

        // Restore connectivity, then run again well inside the debounce
        // window (force = false): the catch-up push must still happen,
        // against the real upstream/release, not origin/main.
        git_ok(&work, &["remote", "set-url", "--push", "upstream", remote.to_str().unwrap()]);
        let second = backup_to_repo(&store, &work, "thor2", false).unwrap();
        assert!(
            second.contains("pushed 1 backup commit(s) that had not reached the remote"),
            "must catch up against the real upstream (upstream/release): {second}"
        );
        assert_eq!(commits_ahead_of_upstream(&work), Some(0), "the real upstream must now have the commit");
    }
}
