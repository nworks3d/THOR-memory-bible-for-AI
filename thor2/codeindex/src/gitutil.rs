//! Thin wrapper around the `git` binary, run as a subprocess. `codeindex`
//! never parses `.git` internals directly: every fact about a repository
//! (its HEAD commit, its tracked files at a commit, a file's content, whether
//! the working tree is dirty) comes from asking git itself, so this crate
//! agrees with git by construction instead of by a second implementation of
//! git's own rules.
//!
//! Every function here takes the repository root explicitly and runs git
//! with `-C <root>`, so callers never depend on the process' current
//! directory.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Something went wrong running git, or the answer git gave was not what a
/// caller could use (not a repository, an unreadable blob, ...). Carries the
/// git command that was run so a failure is diagnosable, not just "git
/// failed".
#[derive(Debug, Clone)]
pub struct GitError {
    pub command: String,
    pub message: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "git command '{}' failed: {}", self.command, self.message)
    }
}

impl std::error::Error for GitError {}

fn describe(args: &[&str]) -> String {
    format!("git {}", args.join(" "))
}

/// Run git and return its raw stdout bytes on success. A non-zero exit maps
/// to `Err` carrying stderr (or a stand-in message when stderr was empty),
/// never a panic and never a value that pretends success.
fn run_git_bytes(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let mut full_args = vec!["-C"];
    let root_str = repo_root.to_string_lossy().into_owned();
    full_args.push(&root_str);
    full_args.extend_from_slice(args);

    let output = Command::new("git").args(&full_args).output().map_err(|e| GitError {
        command: describe(args),
        message: format!("could not start the git process: {e}"),
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            format!("exit code {:?}", output.status.code())
        } else {
            stderr
        };
        return Err(GitError { command: describe(args), message });
    }
    Ok(output.stdout)
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<String, GitError> {
    let bytes = run_git_bytes(repo_root, args)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

/// True when `path` is inside a git working tree. This is the gate every
/// entry point calls first: a directory that fails this must be refused with
/// a named error, never silently treated as an empty repository.
pub fn is_git_repo(path: &Path) -> bool {
    matches!(git_verdict(path), GitVerdict::Repo)
}

/// What git says about a folder, in the three answers that mean different
/// things to whoever has to fix it.
///
/// THE DEFECT THIS CLOSES, measured 2026-08-18 on a project living on the
/// owner's NAS share: git refused the repository with "detected dubious
/// ownership" (the share is owned by another SID), the check above only saw
/// "not a success", and the tool reported "is not inside a git repository" -
/// about a folder holding a perfectly good repository with its whole history.
/// A wrong diagnosis costs more than no diagnosis: the honest next step is
/// one config line, and nothing said so.
pub enum GitVerdict {
    Repo,
    NoRepo,
    /// A repository git will not read as this user, with git's own words.
    Refused(String),
}

pub fn git_verdict(path: &Path) -> GitVerdict {
    let out = Command::new("git")
        .args(["-C", &path.to_string_lossy(), "rev-parse", "--is-inside-work-tree"])
        .output();
    match out {
        Ok(o) if o.status.success() => GitVerdict::Repo,
        Ok(o) => {
            let said = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if said.contains("dubious ownership") || said.contains("safe.directory") {
                GitVerdict::Refused(said)
            } else {
                GitVerdict::NoRepo
            }
        }
        Err(_) => GitVerdict::NoRepo,
    }
}

/// The top-level directory of the repository `path` is inside. Callers store
/// this, not the path they were given, so a build and a later refresh from a
/// different subdirectory of the same repo agree on one root.
pub fn repo_root(path: &Path) -> Result<PathBuf, GitError> {
    let out = run_git(path, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(out))
}

/// The commit HEAD currently points at, as a full 40-character hex sha.
pub fn head_commit(repo_root: &Path) -> Result<String, GitError> {
    run_git(repo_root, &["rev-parse", "HEAD"])
}

/// One tracked file at a commit: its repo-relative path and the git blob id
/// (sha) of its content at that commit. The blob id is content-addressed by
/// git itself, so two files with identical content share one id and any
/// content change changes the id - this is the "goedkoop" (cheap) per-file
/// change signal `refresh` relies on, and it comes from git's own tree diff
/// rather than a second hashing scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub blob_id: String,
}

/// Every blob git tracks at `commit`, recursively (submodules are skipped:
/// their entries have type `commit`, not `blob`, and are excluded here since
/// this index only ever holds this repository's own text).
pub fn list_tree(repo_root: &Path, commit: &str) -> Result<Vec<TreeEntry>, GitError> {
    let bytes = run_git_bytes(repo_root, &["ls-tree", "-r", "-z", "--full-tree", commit])?;
    let mut entries = Vec::new();
    for record in bytes.split(|b| *b == 0) {
        if record.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(record);
        // Format: "<mode> <type> <sha>\t<path>"
        let Some((meta, path)) = text.split_once('\t') else { continue };
        let mut fields = meta.split(' ');
        let (_mode, obj_type, sha) = (fields.next(), fields.next(), fields.next());
        if obj_type != Some("blob") {
            continue;
        }
        let Some(sha) = sha else { continue };
        entries.push(TreeEntry { path: path.to_string(), blob_id: sha.to_string() });
    }
    Ok(entries)
}

/// The blob for exactly one path at one commit, without listing the whole
/// tree - `git ls-tree` resolves a specific path directly via the tree's own
/// structure, so this stays cheap (one targeted git call) no matter how big
/// the repository is. `None` when nothing lives at that path at that commit
/// (already deleted, or replaced by something else by the time this looked).
pub fn blob_at(repo_root: &Path, commit: &str, path: &str) -> Result<Option<TreeEntry>, GitError> {
    let bytes = run_git_bytes(repo_root, &["ls-tree", "-z", commit, "--", path])?;
    for record in bytes.split(|b| *b == 0) {
        if record.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(record);
        let Some((meta, entry_path)) = text.split_once('\t') else { continue };
        if entry_path != path {
            continue;
        }
        let mut fields = meta.split(' ');
        let (_mode, obj_type, sha) = (fields.next(), fields.next(), fields.next());
        if obj_type != Some("blob") {
            continue;
        }
        if let Some(sha) = sha {
            return Ok(Some(TreeEntry { path: path.to_string(), blob_id: sha.to_string() }));
        }
    }
    Ok(None)
}

/// The raw content of one blob. Never assumed to be UTF-8 text here (the
/// caller decides whether it looks like text or binary); this function just
/// hands back exactly the bytes git stored.
pub fn read_blob(repo_root: &Path, blob_id: &str) -> Result<Vec<u8>, GitError> {
    run_git_bytes(repo_root, &["cat-file", "blob", blob_id])
}

/// One path git's tree diff reports as different between two commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangedPath {
    Added(String),
    Modified(String),
    Deleted(String),
    Renamed { from: String, to: String },
}

/// Every path whose blob differs between `from_commit` and `to_commit` -
/// git's own tree diff, so this is exactly the git-blob-id comparison the
/// contract asks for, not a second one this crate invents. `-z` is used so a
/// path can never be mis-split on an embedded tab or newline.
pub fn changed_paths(
    repo_root: &Path,
    from_commit: &str,
    to_commit: &str,
) -> Result<Vec<ChangedPath>, GitError> {
    let bytes = run_git_bytes(
        repo_root,
        &["diff", "--name-status", "-z", from_commit, to_commit],
    )?;
    let tokens: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| String::from_utf8_lossy(r).into_owned())
        .collect();

    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let status = &tokens[i];
        let code = status.chars().next().unwrap_or('?');
        match code {
            'R' | 'C' => {
                let from = tokens.get(i + 1).cloned().unwrap_or_default();
                let to = tokens.get(i + 2).cloned().unwrap_or_default();
                out.push(ChangedPath::Renamed { from, to });
                i += 3;
            }
            'A' => {
                out.push(ChangedPath::Added(tokens.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            'D' => {
                out.push(ChangedPath::Deleted(tokens.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            _ => {
                // M (modified) and anything else git might report (T = type
                // change, U = unmerged, ...) are all treated as "reread it" -
                // the safe default for an archive that would rather re-index
                // a file than miss a change it did not recognise.
                out.push(ChangedPath::Modified(tokens.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
        }
    }
    Ok(out)
}

/// How many paths git's tree diff reports as different between two commits -
/// the cheap "N files differ" count for the human status report. Renames
/// count once, as one differing path.
pub fn changed_path_count(
    repo_root: &Path,
    from_commit: &str,
    to_commit: &str,
) -> Result<usize, GitError> {
    if from_commit == to_commit {
        return Ok(0);
    }
    Ok(changed_paths(repo_root, from_commit, to_commit)?.len())
}

/// How many tracked files have uncommitted changes in the working tree
/// (staged or unstaged), relative to HEAD. Newly created files that git does
/// not yet track (`??` in porcelain output) are deliberately NOT counted
/// here: they are not a change to an existing file, they are a file that
/// does not exist in the index's world yet at all. This is a documented
/// choice, not an oversight - see the crate-level docs on `Provenance`.
pub fn dirty_file_count(repo_root: &Path) -> Result<usize, GitError> {
    let out = run_git(repo_root, &["status", "--porcelain"])?;
    Ok(out
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with("??"))
        .count())
}
