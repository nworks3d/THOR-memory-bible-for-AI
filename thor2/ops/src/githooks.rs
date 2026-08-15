//! The git side of installation: one machine-wide hooks directory, so the code
//! index refreshes itself after every commit in every repository without anyone
//! having to remember to run anything.
//!
//! Why machine-wide and not one hook per repository. `codeindex::store::refresh`
//! is cheap, commit-diff driven, and a no-op the moment HEAD has not moved - but
//! nothing ever called it, so an index was only fresh if a person remembered.
//! That is CONTRACT R7 word for word ("If the design needs someone to remember
//! something, that is a design defect"), and measured on 2026-08-15 it cost a
//! lot: with an index three days behind, 83% of `where_used` sites named a line
//! that no longer held the symbol, against 0% from a fresh one.
//!
//! THE DEFECT THIS MODULE MUST NOT CAUSE. Setting `core.hooksPath` makes git
//! ignore every repository's own `.git/hooks` directory outright. This project's
//! own repository has a `pre-commit` hook that refuses to let the maintainer's
//! private identifiers reach a public remote. A naive install would switch that
//! gate off without a word - a gate going quiet is this project's worst failure
//! class, and it would have been caused by the change meant to help. So this
//! directory holds a dispatcher per event and every one of them ends by handing
//! control back to the repository's own hook.
//!
//! `--git-common-dir` and never `--git-path hooks`: once `core.hooksPath` is
//! set, the latter answers with THIS directory and the chain would point at
//! itself. That single word is the difference between a working gate and a
//! silent one.

use std::fs;
use std::path::Path;

use crate::install::HookOutcome;

/// Every hook event this directory answers for. A repository hook whose event
/// has no dispatcher here would be silently skipped once `core.hooksPath` is
/// set, so this list is a safety floor, not a convenience: it covers every
/// event in use plus the common neighbours someone may add later.
pub const DISPATCHED_EVENTS: &[&str] = &[
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-rebase",
    "post-rewrite",
];

const HEADER: &str = r#"#!/bin/sh
# Installed by THOR. This directory is git's machine-wide core.hooksPath, which
# makes git ignore every repository's own .git/hooks. Each dispatcher here
# therefore hands control back to the repository's own hook, so wiring one
# machine-wide hook never silences a local one.
"#;

/// The tail every dispatcher ends with. Kept in one place because getting it
/// wrong in one file is exactly how a repository's own hook goes quiet.
const CHAIN: &str = r#"
common="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"
if [ -z "$common" ]; then common="$(git rev-parse --git-common-dir 2>/dev/null)"; fi
repo_hook="$common/hooks/@EVENT@"
if [ -x "$repo_hook" ]; then
	exec "$repo_hook" "$@"
elif [ -f "$repo_hook" ]; then
	exec sh "$repo_hook" "$@"
fi
exit 0
"#;

/// The work `post-commit` does before it chains: resolve which project this
/// repository is, and refresh that project's index if one exists.
///
/// Resolution mirrors `serve::project` exactly - a `.thor-project` marker file's
/// first non-blank line wins, otherwise the repository directory's basename -
/// because an index this cannot find is an index `serve::lookup` cannot find
/// either. A repository THOR has no index for is left completely alone: this
/// never builds a full index unasked.
const REFRESH: &str = r#"
CODEINDEX="@CODEINDEX@"
INDEX_ROOT="@INDEX_ROOT@"

root="$(git rev-parse --show-toplevel 2>/dev/null)"
if [ -n "$root" ]; then
	marker="$root/.thor-project"
	if [ -f "$marker" ]; then
		key="$(grep -v '^[[:space:]]*$' "$marker" | head -n 1 | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
	else
		key="$(basename "$root")"
	fi
	if [ -n "$key" ] && [ -f "$INDEX_ROOT/$key.db" ]; then
		"$CODEINDEX" "$INDEX_ROOT/$key.db" "$root" refresh >/dev/null 2>&1
	fi
fi
"#;

/// The exact text of one dispatcher. Pure, so a test can read it without
/// touching a filesystem or a git config.
pub fn dispatcher_body(event: &str, codeindex_exe: &str, index_root: &str) -> String {
    let mut body = String::from(HEADER);
    if event == "post-commit" {
        body.push_str(
            &REFRESH
                .replace("@CODEINDEX@", codeindex_exe)
                .replace("@INDEX_ROOT@", index_root),
        );
    }
    body.push_str(&CHAIN.replace("@EVENT@", event));
    body
}

/// Write every dispatcher into `dir`, reporting what happened to each. A file
/// whose content already matches is left untouched, so a second install is a
/// no-op rather than a rewrite.
pub fn install_git_hooks(
    dir: &Path,
    codeindex_exe: &str,
    index_root: &str,
) -> std::io::Result<Vec<(String, HookOutcome)>> {
    fs::create_dir_all(dir)?;
    let mut results = Vec::new();

    for event in DISPATCHED_EVENTS {
        let path = dir.join(event);
        let wanted = dispatcher_body(event, codeindex_exe, index_root);

        // Compare as bytes: a stray BOM or CRLF from an earlier hand-edit must
        // read as "different", not as "already fine".
        let outcome = match fs::read(&path) {
            Ok(existing) if existing == wanted.as_bytes() => HookOutcome::AlreadyPresent,
            Ok(_) => {
                fs::write(&path, &wanted)?;
                HookOutcome::Replaced
            }
            Err(_) => {
                fs::write(&path, &wanted)?;
                HookOutcome::Added
            }
        };

        // On unix a hook git cannot execute is a hook that does not run. On
        // Windows the bit does not exist and git runs it through its own sh.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }

        results.push(((*event).to_string(), outcome));
    }

    Ok(results)
}

/// What wiring `core.hooksPath` did, or refused to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HooksPathOutcome {
    /// Nothing was configured before; this run pointed git at our directory.
    Set,
    /// Already pointing here. Nothing written.
    AlreadyOurs,
    /// Something else owns this setting. Left exactly as it was, and reported -
    /// taking it over would silently disable whatever that directory does.
    Foreign(String),
}

/// Decide what to do about `core.hooksPath`, given whatever git reports now.
/// Split out from the git call so the decision is testable without touching the
/// machine's real global configuration.
pub fn decide_hooks_path(current: Option<&str>, ours: &str) -> HooksPathOutcome {
    match current.map(str::trim).filter(|c| !c.is_empty()) {
        None => HooksPathOutcome::Set,
        Some(c) if paths_equal(c, ours) => HooksPathOutcome::AlreadyOurs,
        Some(c) => HooksPathOutcome::Foreign(c.to_string()),
    }
}

/// Read git's current global `core.hooksPath`, decide, and set it only when it
/// is free. Shelling out to `git` rather than editing its config file by hand,
/// the same line `codeindex::gitutil` already takes: git owns that format.
///
/// A directory someone else configured is LEFT ALONE and reported. Taking it
/// over would disable whatever that directory does, which is the exact failure
/// this whole module is built to avoid.
pub fn wire_hooks_path(dir: &Path) -> std::io::Result<HooksPathOutcome> {
    let ours = dir.to_string_lossy().replace('\\', "/");

    let current = std::process::Command::new("git")
        .args(["config", "--global", "--get", "core.hooksPath"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());

    let outcome = decide_hooks_path(current.as_deref(), &ours);
    if outcome == HooksPathOutcome::Set {
        let status = std::process::Command::new("git")
            .args(["config", "--global", "core.hooksPath", &ours])
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(
                "git config --global core.hooksPath did not succeed",
            ));
        }
    }
    Ok(outcome)
}

/// Git stores this path as written. The same directory can arrive with either
/// slash on Windows, and a false "someone else owns this" would stop the
/// installer for no reason.
fn paths_equal(a: &str, b: &str) -> bool {
    a.replace('\\', "/").trim_end_matches('/') == b.replace('\\', "/").trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "C:/fake/codeindex.exe";
    const ROOT: &str = "C:/fake/codeindex";

    #[test]
    fn install_writes_a_post_commit_hook_that_calls_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let results = install_git_hooks(dir.path(), EXE, ROOT).unwrap();

        assert!(results.iter().all(|(_, o)| *o == HookOutcome::Added));
        let body = fs::read_to_string(dir.path().join("post-commit")).unwrap();
        assert!(body.contains(EXE), "the hook must call the codeindex binary");
        assert!(body.contains("refresh"), "and it must call refresh");
    }

    /// THE DEFECT THIS PREVENTS: `core.hooksPath` switches off every
    /// repository's own hooks. A dispatcher that does not chain would silence
    /// this project's private-data gate without a word.
    #[test]
    fn every_dispatcher_hands_control_back_to_the_repository_hook() {
        let dir = tempfile::tempdir().unwrap();
        install_git_hooks(dir.path(), EXE, ROOT).unwrap();

        for event in DISPATCHED_EVENTS {
            let body = fs::read_to_string(dir.path().join(event)).unwrap();
            assert!(
                body.contains(&format!("hooks/{event}")),
                "{event} must chain to the repository's own {event}"
            );
            assert!(
                body.contains("exec \"$repo_hook\""),
                "{event} must actually exec it, not merely name it"
            );
        }
    }

    #[test]
    fn the_chain_uses_git_common_dir_so_it_never_points_at_itself() {
        let body = dispatcher_body("pre-commit", EXE, ROOT);
        assert!(body.contains("--git-common-dir"));
        assert!(
            !body.contains("--git-path hooks"),
            "--git-path hooks answers with OUR directory once core.hooksPath is set, \
             so the chain would exec itself forever"
        );
    }

    #[test]
    fn an_existing_foreign_hooks_path_is_never_taken_over() {
        let ours = "C:/Users/dev/thor2/githooks";
        assert_eq!(decide_hooks_path(None, ours), HooksPathOutcome::Set);
        assert_eq!(decide_hooks_path(Some(""), ours), HooksPathOutcome::Set);
        assert_eq!(decide_hooks_path(Some(ours), ours), HooksPathOutcome::AlreadyOurs);
        assert_eq!(
            decide_hooks_path(Some("C:\\Users\\dev\\thor2\\githooks\\"), ours),
            HooksPathOutcome::AlreadyOurs,
            "the same directory with the other slash is still ours"
        );
        assert_eq!(
            decide_hooks_path(Some("D:/someone/else/hooks"), ours),
            HooksPathOutcome::Foreign("D:/someone/else/hooks".to_string())
        );
    }

    #[test]
    fn installing_twice_reports_already_present_and_rewrites_nothing() {
        let dir = tempfile::tempdir().unwrap();
        install_git_hooks(dir.path(), EXE, ROOT).unwrap();
        let before = fs::read(dir.path().join("post-commit")).unwrap();

        let second = install_git_hooks(dir.path(), EXE, ROOT).unwrap();
        assert!(second.iter().all(|(_, o)| *o == HookOutcome::AlreadyPresent));
        assert_eq!(before, fs::read(dir.path().join("post-commit")).unwrap());
    }

    /// A commit must never fail or stall because the index could not be
    /// refreshed. git ignores post-commit's exit code, but a hook that exits
    /// non-zero still prints noise, and `set -e` would abandon the chain.
    #[test]
    fn the_post_commit_body_never_lets_a_failure_escape() {
        let body = dispatcher_body("post-commit", EXE, ROOT);
        assert!(body.contains(">/dev/null 2>&1"), "refresh output stays out of the way");
        assert!(body.trim_end().ends_with("exit 0"));
        assert!(!body.contains("set -e"), "set -e would skip the chain on any failure");
    }

    #[test]
    fn the_post_commit_body_only_refreshes_when_an_index_exists() {
        let body = dispatcher_body("post-commit", EXE, ROOT);
        assert!(
            body.contains("[ -f \"$INDEX_ROOT/$key.db\" ]"),
            "a repository THOR has no index for must be left completely alone"
        );
    }

    /// The list is a safety floor: an event missing here is a repository hook
    /// git silently stops calling.
    #[test]
    fn the_dispatcher_set_covers_every_hook_event_git_can_call_here() {
        for event in ["pre-commit", "post-commit", "post-checkout", "post-merge", "pre-push"] {
            assert!(
                DISPATCHED_EVENTS.contains(&event),
                "{event} is in use in the owner's repositories and must have a dispatcher"
            );
        }
    }

    #[test]
    fn the_hook_files_are_written_with_lf_and_no_bom() {
        let dir = tempfile::tempdir().unwrap();
        install_git_hooks(dir.path(), EXE, ROOT).unwrap();

        for event in DISPATCHED_EVENTS {
            let bytes = fs::read(dir.path().join(event)).unwrap();
            assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "{event} carries a BOM");
            assert!(!bytes.contains(&b'\r'), "{event} carries CRLF, which git's sh chokes on");
        }
    }
}
