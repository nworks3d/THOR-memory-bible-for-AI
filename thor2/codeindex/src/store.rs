//! The sqlite-backed index: one `repo_meta` row naming the repository root
//! and the commit the whole index is at, one `file` row per indexed text
//! file (path, git blob id, the commit its content was last read from), and
//! one `chunk` row per stored piece of that file's text (see
//! `chunk::chunk_text`).
//!
//! This is a lookup table, not the append-only hash-chained log `thor-core`
//! keeps for facts: code is an archive here (requirement 5 of the design
//! note this crate was built against) - there is no serve path, no
//! injection, nothing fires on its own. The only way anything here is ever
//! read is a caller explicitly asking `search` or `status` a question.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::chunk::{chunk_text, looks_binary};
use crate::gitutil::{self, ChangedPath, GitError};

/// Everything that can go wrong building, refreshing or reading the index.
/// Every variant is meant to be shown to the caller - a codeindex operation
/// is always "read and answer" or "write and declare", and per this crate's
/// own status contract (requirement 4) neither one may fail silently.
#[derive(Debug)]
pub enum CodeIndexError {
    /// `path` is not inside a git working tree - refused, not guessed at.
    NotAGitRepo(std::path::PathBuf),
    /// A real repository git will not read as this user. Kept apart from
    /// `NotAGitRepo` because the fix is completely different, and calling
    /// this one "not a repository" sends people looking for the wrong thing.
    GitRefusesRepo { path: std::path::PathBuf, said: String },
    Git(GitError),
    Sql(rusqlite::Error),
    /// `refresh` was called before `build_full` ever ran against this store.
    NotIndexed,
}

impl std::fmt::Display for CodeIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodeIndexError::NotAGitRepo(p) => {
                write!(f, "{} is not inside a git repository - codeindex only ever indexes from git, so there is nothing to read here. Run `git init` there first if this is meant to be code", p.display())
            }
            CodeIndexError::GitRefusesRepo { path, said } => write!(
                f,
                "{} IS a git repository, but git refuses to read it as you: {said}. This is what a repository on a \
                 network share looks like - the folder is owned by another account. Trust it once with \
                 `git config --global --add safe.directory '%(prefix)///<server>/<share>/<path to the repo>'` and \
                 build the index again",
                path.display()
            ),
            CodeIndexError::Git(e) => write!(f, "{e}"),
            CodeIndexError::Sql(e) => write!(f, "index store error: {e}"),
            CodeIndexError::NotIndexed => {
                write!(f, "no index exists yet for this store - call build_full before refresh or search")
            }
        }
    }
}

impl std::error::Error for CodeIndexError {}

impl From<GitError> for CodeIndexError {
    fn from(e: GitError) -> Self {
        CodeIndexError::Git(e)
    }
}

impl From<rusqlite::Error> for CodeIndexError {
    fn from(e: rusqlite::Error) -> Self {
        CodeIndexError::Sql(e)
    }
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) a code index at `path`.
    pub fn open(path: &Path) -> Result<Self, CodeIndexError> {
        let conn = Connection::open(path)?;
        Self::init(&conn)?;
        Ok(Store { conn })
    }

    pub fn in_memory() -> Result<Self, CodeIndexError> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Store { conn })
    }

    fn init(conn: &Connection) -> Result<(), CodeIndexError> {
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS repo_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                repo_root TEXT NOT NULL,
                indexed_commit TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS file (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                blob_id TEXT NOT NULL,
                commit_id TEXT NOT NULL,
                size INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_file_path ON file(path);
            CREATE TABLE IF NOT EXISTS chunk (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE,
                chunk_index INTEGER NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                text TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunk_file ON chunk(file_id);
            ",
        )?;
        // Symbols live in this same database, written in the same transaction
        // as the chunk they came from, so there is no sidecar to drift and no
        // rebuild command anyone has to remember (CONTRACT R7).
        crate::symbols::init(conn)?;
        Ok(())
    }

    /// Read access to the underlying connection for the sibling query modules
    /// in this crate. Not public outside it: everything a caller needs is a
    /// function here or in `symbols`, so nobody assembles their own SQL
    /// against a schema they do not own.
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    /// The commit the index as a whole is at, if anything has been indexed.
    pub fn indexed_commit(&self) -> Result<Option<String>, CodeIndexError> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT indexed_commit FROM repo_meta WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        Ok(v)
    }

    pub fn repo_root(&self) -> Result<Option<String>, CodeIndexError> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT repo_root FROM repo_meta WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        Ok(v)
    }

    pub fn file_count(&self) -> Result<i64, CodeIndexError> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM file", [], |r| r.get(0))?)
    }

    pub fn chunk_count(&self) -> Result<i64, CodeIndexError> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM chunk", [], |r| r.get(0))?)
    }
}

/// Insert one file's row and its chunks from freshly read blob content.
/// Callers are responsible for removing any existing row for `path` first
/// (a plain insert, never an upsert, keeps this one code path identical for
/// both a brand new file and a changed one). A binary blob writes nothing
/// and returns `false`, so a caller can tell "indexed" from "skipped".
/// Shared by `build_full` and `refresh`'s single-file reread, so a full
/// build and an incremental update can never disagree about how a file is
/// stored.
fn insert_file(
    tx: &rusqlite::Transaction,
    path: &str,
    blob_id: &str,
    commit: &str,
    content: &[u8],
) -> Result<bool, CodeIndexError> {
    if looks_binary(content) {
        return Ok(false);
    }
    let text = String::from_utf8_lossy(content);
    let chunks = chunk_text(&text);
    tx.execute(
        "INSERT INTO file (path, blob_id, commit_id, size) VALUES (?, ?, ?, ?)",
        params![path, blob_id, commit, content.len() as i64],
    )?;
    let file_id = tx.last_insert_rowid();
    for c in &chunks {
        tx.execute(
            "INSERT INTO chunk (file_id, chunk_index, start_line, end_line, text) VALUES (?, ?, ?, ?, ?)",
            params![file_id, c.chunk_index, c.start_line, c.end_line, c.text],
        )?;
        crate::symbols::record_chunk(tx, tx.last_insert_rowid(), &c.text, c.start_line)?;
    }
    Ok(true)
}

/// Outcome of a full build: everything the index knows was thrown away and
/// rebuilt from the current HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStats {
    pub commit: String,
    pub files_indexed: usize,
    pub files_skipped_binary: usize,
    pub chunks_written: usize,
}

/// Build the index from scratch at the repository's current HEAD. Every
/// existing file/chunk row is discarded first, so the result never carries
/// forward a row a later refresh should have removed. `path` may be any
/// directory inside the working tree; the actual root git reports is what
/// gets stored.
pub fn build_full(store: &mut Store, path: &Path) -> Result<BuildStats, CodeIndexError> {
    match gitutil::git_verdict(path) {
        gitutil::GitVerdict::Repo => {}
        gitutil::GitVerdict::NoRepo => return Err(CodeIndexError::NotAGitRepo(path.to_path_buf())),
        gitutil::GitVerdict::Refused(said) => {
            return Err(CodeIndexError::GitRefusesRepo { path: path.to_path_buf(), said })
        }
    }
    let root = gitutil::repo_root(path)?;
    let commit = gitutil::head_commit(&root)?;
    let entries = gitutil::list_tree(&root, &commit)?;

    let tx = store.conn.unchecked_transaction()?;
    tx.execute("DELETE FROM chunk", [])?;
    tx.execute("DELETE FROM file", [])?;

    let mut files_indexed = 0usize;
    let mut files_skipped_binary = 0usize;
    for entry in &entries {
        let content = gitutil::read_blob(&root, &entry.blob_id)?;
        if insert_file(&tx, &entry.path, &entry.blob_id, &commit, &content)? {
            files_indexed += 1;
        } else {
            files_skipped_binary += 1;
        }
    }
    tx.execute(
        "INSERT INTO repo_meta (id, repo_root, indexed_commit) VALUES (1, ?, ?)
         ON CONFLICT(id) DO UPDATE SET repo_root = excluded.repo_root, indexed_commit = excluded.indexed_commit",
        params![root.to_string_lossy(), commit],
    )?;
    let chunks_written: i64 = tx.query_row("SELECT COUNT(*) FROM chunk", [], |r| r.get(0))?;
    tx.commit()?;

    Ok(BuildStats { commit, files_indexed, files_skipped_binary, chunks_written: chunks_written as usize })
}

/// Outcome of a refresh: which commit the index moved from and to, and how
/// many files were actually reread. `no_op` is true when the index was
/// already at the repository's current HEAD - the cheap path that costs one
/// `git rev-parse` and touches no file content at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshStats {
    pub previous_commit: String,
    pub new_commit: String,
    pub files_changed: usize,
    pub files_deleted: usize,
    pub no_op: bool,
}

/// Bring an existing index up to the repository's current HEAD by rereading
/// only the files git's own tree diff reports as different between the
/// previously indexed commit and the new one (requirement 3: a refresh never
/// needs a full reindex). Errors with `NotIndexed` if `build_full` never ran.
pub fn refresh(store: &mut Store, path: &Path) -> Result<RefreshStats, CodeIndexError> {
    match gitutil::git_verdict(path) {
        gitutil::GitVerdict::Repo => {}
        gitutil::GitVerdict::NoRepo => return Err(CodeIndexError::NotAGitRepo(path.to_path_buf())),
        gitutil::GitVerdict::Refused(said) => {
            return Err(CodeIndexError::GitRefusesRepo { path: path.to_path_buf(), said })
        }
    }
    let root = gitutil::repo_root(path)?;
    let previous_commit = store.indexed_commit()?.ok_or(CodeIndexError::NotIndexed)?;
    let new_commit = gitutil::head_commit(&root)?;

    if previous_commit == new_commit {
        // The cheap no-op path: one git call already answered, nothing else
        // to check, no file content touched.
        return Ok(RefreshStats {
            previous_commit: previous_commit.clone(),
            new_commit,
            files_changed: 0,
            files_deleted: 0,
            no_op: true,
        });
    }

    let changes = gitutil::changed_paths(&root, &previous_commit, &new_commit)?;
    let mut files_changed = 0usize;
    let mut files_deleted = 0usize;

    let tx = store.conn.unchecked_transaction()?;
    for change in &changes {
        match change {
            ChangedPath::Added(path) | ChangedPath::Modified(path) => {
                reread_one(&tx, &root, path, &new_commit)?;
                files_changed += 1;
            }
            ChangedPath::Deleted(path) => {
                tx.execute("DELETE FROM file WHERE path = ?", [path])?;
                files_deleted += 1;
            }
            ChangedPath::Renamed { from, to } => {
                tx.execute("DELETE FROM file WHERE path = ?", [from])?;
                reread_one(&tx, &root, to, &new_commit)?;
                files_changed += 1;
            }
        }
    }
    tx.execute(
        "UPDATE repo_meta SET indexed_commit = ? WHERE id = 1",
        params![new_commit],
    )?;
    tx.commit()?;

    Ok(RefreshStats { previous_commit, new_commit, files_changed, files_deleted, no_op: false })
}

/// Reread exactly one path's current blob at `commit` and replace its row +
/// chunks. Looks up the blob id with a single targeted `ls-tree <path>`
/// (see `gitutil::blob_at`), never a whole-tree listing, so a refresh's
/// cost stays proportional to the number of CHANGED files, never the size
/// of the repository.
fn reread_one(
    tx: &rusqlite::Transaction,
    root: &Path,
    path: &str,
    commit: &str,
) -> Result<(), CodeIndexError> {
    tx.execute("DELETE FROM file WHERE path = ?", [path])?;
    let Some(entry) = gitutil::blob_at(root, commit, path)? else {
        // Reported as added/modified by the diff but gone by the time we
        // looked (e.g. replaced by a directory) - nothing sane to store,
        // treat as removed rather than guessing.
        return Ok(());
    };
    let content = gitutil::read_blob(root, &entry.blob_id)?;
    insert_file(tx, path, &entry.blob_id, commit, &content)?;
    Ok(())
}

/// Where an answer comes from, and whether the world has moved on since:
/// requirement 2 of the design note - every answer names its commit AND
/// whether the working copy diverges from it. Being behind is fine; being
/// silently behind is not, so a field that could not be determined is
/// `None`, never defaulted to a value that would claim "nothing changed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub indexed_commit: String,
    /// `Err(reason)` when the repository could not be reached right now
    /// (moved, deleted, no git on PATH, ...) - never silently treated as "no
    /// drift".
    pub current_commit: Result<String, String>,
    /// Files whose blob differs between `indexed_commit` and `current_commit`.
    /// `None` only when `current_commit` itself is `Err`.
    pub files_differ: Option<usize>,
    /// Tracked files with uncommitted changes in the working tree right now.
    /// `None` only when `current_commit` itself is `Err`.
    pub uncommitted_changed: Option<usize>,
}

/// Compute the provenance of the current index state against `path`'s
/// repository. Degrades instead of failing: a repository that cannot be
/// reached right now still returns a `Provenance` (with `current_commit` as
/// `Err`), because "I don't know if this is stale" is itself a valid, honest
/// answer this crate must be able to give.
pub fn provenance(store: &Store, path: &Path) -> Result<Provenance, CodeIndexError> {
    let indexed_commit = store.indexed_commit()?.ok_or(CodeIndexError::NotIndexed)?;

    if !gitutil::is_git_repo(path) {
        return Ok(Provenance {
            indexed_commit,
            current_commit: Err(format!("{} is not inside a git repository", path.display())),
            files_differ: None,
            uncommitted_changed: None,
        });
    }
    let root = match gitutil::repo_root(path) {
        Ok(r) => r,
        Err(e) => {
            return Ok(Provenance {
                indexed_commit,
                current_commit: Err(e.to_string()),
                files_differ: None,
                uncommitted_changed: None,
            })
        }
    };
    let current_commit = gitutil::head_commit(&root).map_err(|e| e.to_string());
    let files_differ = match &current_commit {
        Ok(current) => gitutil::changed_path_count(&root, &indexed_commit, current).ok(),
        Err(_) => None,
    };
    let uncommitted_changed = match &current_commit {
        Ok(_) => gitutil::dirty_file_count(&root).ok(),
        Err(_) => None,
    };
    Ok(Provenance { indexed_commit, current_commit, files_differ, uncommitted_changed })
}

/// A human status line: requirement 4 of the design note. One template
/// covers every reachable state (clean or behind) so there is no branch that
/// can drift from the numbers it reports; only the "repository unreachable"
/// case is genuinely a different sentence, because there is nothing to
/// compare against.
pub fn human_status(p: &Provenance) -> String {
    match &p.current_commit {
        Ok(current) => {
            let differ = p
                .files_differ
                .map(|n| n.to_string())
                .unwrap_or_else(|| "an unknown number of".to_string());
            let uncommitted = p
                .uncommitted_changed
                .map(|n| n.to_string())
                .unwrap_or_else(|| "an unknown number of".to_string());
            format!(
                "Index is at commit {}. Your branch is at commit {}. {} file(s) differ between the two. {} file(s) in your working copy are uncommitted changes.",
                p.indexed_commit, current, differ, uncommitted
            )
        }
        Err(reason) => format!(
            "Index is at commit {}. The repository could not be checked just now ({}), so it is unknown whether your branch or working copy have moved on.",
            p.indexed_commit, reason
        ),
    }
}

/// One hit from `search`, with the file it came from and the commit that
/// content was read at.
///
/// `text` is an EXCERPT around the matching lines, not the whole stored
/// chunk. Chunks are 200 lines, which is the right size to store and far too
/// much to answer with: this tool's own job is to say WHERE something is
/// written so the reader can open the real file, and a two-hit answer used to
/// arrive as 400 lines of source. That is the noise this whole rebuild
/// exists to refuse. `start_line`/`end_line` describe the excerpt, so a line
/// number still points where it says it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: String,
    pub commit_id: String,
    pub chunk_index: i64,
    pub start_line: i64,
    pub end_line: i64,
    pub text: String,
}

/// Lines of context kept either side of a matching line. Enough to see what
/// the match sits in (a signature, the head of a block) without handing back
/// the file.
const EXCERPT_CONTEXT: usize = 3;

/// Cut `chunk_text` down to the lines that match `query`, plus
/// `EXCERPT_CONTEXT` either side, merging runs that overlap. Returns the
/// excerpt and its own absolute 1-based start and end line.
///
/// A chunk that somehow contains no matching line (only possible if the SQL
/// and this comparison ever disagree) falls back to the first window rather
/// than returning nothing - an empty excerpt would read as "found it, but it
/// is blank", which is worse than a slightly wrong one.
fn excerpt_around_match(chunk_text: &str, chunk_start_line: i64, query: &str) -> (String, i64, i64) {
    let needle = query.to_lowercase();
    let lines: Vec<&str> = chunk_text.lines().collect();
    if lines.is_empty() {
        return (String::new(), chunk_start_line, chunk_start_line);
    }

    let matching: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect();

    let keep: Vec<usize> = if matching.is_empty() {
        (0..lines.len().min(EXCERPT_CONTEXT * 2 + 1)).collect()
    } else {
        let mut keep = std::collections::BTreeSet::new();
        for &i in &matching {
            let from = i.saturating_sub(EXCERPT_CONTEXT);
            let to = (i + EXCERPT_CONTEXT).min(lines.len() - 1);
            for j in from..=to {
                keep.insert(j);
            }
        }
        keep.into_iter().collect()
    };

    let first = *keep.first().unwrap_or(&0);
    let last = *keep.last().unwrap_or(&0);

    // Runs that are not contiguous get an explicit gap marker, so nobody
    // reads two separate regions as one continuous piece of the file.
    let mut out = String::new();
    let mut previous: Option<usize> = None;
    for &i in &keep {
        if let Some(p) = previous {
            if i != p + 1 {
                out.push_str("...\n");
            }
        }
        out.push_str(lines[i]);
        out.push('\n');
        previous = Some(i);
    }

    (out, chunk_start_line + first as i64, chunk_start_line + last as i64)
}

/// Everything `search` answers with: the hits, AND where they came from.
/// Requirement 2 again - a hit's text is never handed back without also
/// saying what commit produced it and whether the working copy has moved on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchAnswer {
    pub hits: Vec<SearchHit>,
    pub provenance: Provenance,
}

/// Case-insensitive substring search over stored chunk text - an archive
/// lookup, not a ranked retrieval system (see the module docs on `Store`).
/// `%` and `_` in `query` are matched literally by escaping them for SQL
/// LIKE, so a query containing them searches for that literal text rather
/// than acting as a wildcard.
pub fn search(
    store: &Store,
    path: &Path,
    query: &str,
    limit: usize,
) -> Result<SearchAnswer, CodeIndexError> {
    let provenance = provenance(store, path)?;
    let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{escaped}%");

    let mut stmt = store.conn.prepare(
        "SELECT f.path, f.commit_id, c.chunk_index, c.start_line, c.end_line, c.text
         FROM chunk c JOIN file f ON f.id = c.file_id
         WHERE c.text LIKE ?1 ESCAPE '\\' COLLATE NOCASE
         ORDER BY f.path, c.chunk_index
         LIMIT ?2",
    )?;
    let hits = stmt
        .query_map(params![pattern, limit as i64], |row| {
            let chunk_start: i64 = row.get(3)?;
            let chunk_text: String = row.get(5)?;
            // Answer with the matching lines, not the 200-line chunk they
            // were stored in - see `SearchHit`'s own doc comment.
            let (text, start_line, end_line) = excerpt_around_match(&chunk_text, chunk_start, query);
            Ok(SearchHit {
                path: row.get(0)?,
                commit_id: row.get(1)?,
                chunk_index: row.get(2)?,
                start_line,
                end_line,
                text,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(SearchAnswer { hits, provenance })
}

#[cfg(test)]
mod git_verdict_tests {
    use super::*;

    /// A folder with no git in it is one answer; a folder git will not read as
    /// this user is a different one, and telling them apart is the whole point
    /// - the second one cost a real session a wrong diagnosis.
    #[test]
    fn a_refused_repository_does_not_report_itself_as_no_repository() {
        let refused = CodeIndexError::GitRefusesRepo {
            path: std::path::PathBuf::from("//share/project"),
            said: "fatal: detected dubious ownership in repository".to_string(),
        };
        let said = refused.to_string();
        assert!(said.contains("IS a git repository"), "{said}");
        assert!(said.contains("safe.directory"), "the way out has to be in it: {said}");
        assert!(!said.contains("is not inside a git repository"), "{said}");
    }

    #[test]
    fn a_folder_without_git_says_to_start_one() {
        let none = CodeIndexError::NotAGitRepo(std::path::PathBuf::from("/tmp/plain"));
        assert!(none.to_string().contains("git init"), "{none}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// A throwaway git repository under a tempdir, with a helper to commit
    /// whatever is currently on disk. Never touches the real owner repos;
    /// every fixture is created fresh here.
    struct TestRepo {
        dir: tempfile::TempDir,
    }

    impl TestRepo {
        fn init() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let run = |args: &[&str]| {
                let out = Command::new("git")
                    .arg("-C")
                    .arg(dir.path())
                    .args(args)
                    .output()
                    .expect("git must be on PATH for these tests");
                assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
            };
            run(&["init", "--quiet"]);
            run(&["config", "user.email", "test@example.invalid"]);
            run(&["config", "user.name", "codeindex tests"]);
            TestRepo { dir }
        }

        fn write(&self, rel_path: &str, content: &str) {
            let full = self.dir.path().join(rel_path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, content).unwrap();
        }

        fn write_bytes(&self, rel_path: &str, content: &[u8]) {
            let full = self.dir.path().join(rel_path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, content).unwrap();
        }

        fn remove(&self, rel_path: &str) {
            fs::remove_file(self.dir.path().join(rel_path)).unwrap();
        }

        fn commit(&self, message: &str) -> String {
            let run = |args: &[&str]| {
                let out = Command::new("git")
                    .arg("-C")
                    .arg(self.dir.path())
                    .args(args)
                    .output()
                    .expect("git must be on PATH for these tests");
                assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
            };
            run(&["add", "-A"]);
            run(&["commit", "--quiet", "-m", message]);
            let out = Command::new("git")
                .arg("-C")
                .arg(self.dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    // ---- requirement 1: index comes from git ----------------------------

    #[test]
    fn build_full_indexes_the_files_git_tracks_at_head() {
        let repo = TestRepo::init();
        repo.write("a.txt", "hello world");
        repo.write("sub/b.txt", "second file");
        let commit = repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        let stats = build_full(&mut store, repo.path()).unwrap();

        assert_eq!(stats.commit, commit);
        assert_eq!(stats.files_indexed, 2);
        assert_eq!(store.file_count().unwrap(), 2);
        assert_eq!(store.indexed_commit().unwrap(), Some(commit));
    }

    #[test]
    fn an_untracked_file_never_appears_in_the_index() {
        // The point of requirement 1: this is a git-derived index, not a
        // directory scan - a file git does not know about must be invisible
        // to it even though it sits right there on disk.
        let repo = TestRepo::init();
        repo.write("tracked.txt", "this one is committed");
        repo.commit("initial");
        repo.write("untracked.txt", "this one never was");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        let answer = search(&store, repo.path(), "never was", 10).unwrap();
        assert!(
            answer.hits.is_empty(),
            "an untracked file must not be searchable - the index reads git, not the directory"
        );
    }

    // ---- requirement: a non-git directory is refused, not guessed at ----

    #[test]
    fn a_directory_without_git_is_refused_not_silently_empty() {
        let dir = tempfile::tempdir().unwrap();
        // no `git init` here at all
        let mut store = Store::in_memory().unwrap();
        let err = build_full(&mut store, dir.path()).unwrap_err();
        assert!(
            matches!(err, CodeIndexError::NotAGitRepo(_)),
            "expected a named NotAGitRepo refusal, got: {err}"
        );
        assert_eq!(store.file_count().unwrap(), 0, "a refused build must write nothing");
    }

    #[test]
    fn refresh_against_a_non_git_directory_is_also_refused() {
        let repo = TestRepo::init();
        repo.write("a.txt", "hi");
        repo.commit("initial");
        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        // move the "repository" conceptually: point refresh at a plain temp
        // dir instead, simulating a store whose repo vanished.
        let not_a_repo = tempfile::tempdir().unwrap();
        let err = refresh(&mut store, not_a_repo.path()).unwrap_err();
        assert!(matches!(err, CodeIndexError::NotAGitRepo(_)));
    }

    #[test]
    fn refresh_before_any_build_full_is_refused_with_a_named_error() {
        let repo = TestRepo::init();
        repo.write("a.txt", "hi");
        repo.commit("initial");
        let mut store = Store::in_memory().unwrap();
        let err = refresh(&mut store, repo.path()).unwrap_err();
        assert!(matches!(err, CodeIndexError::NotIndexed));
    }

    // ---- requirement 2: provenance on every answer -----------------------

    #[test]
    fn a_stale_index_says_so_instead_of_looking_current() {
        let repo = TestRepo::init();
        repo.write("a.txt", "version one");
        let first_commit = repo.commit("v1");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        repo.write("a.txt", "version two");
        let second_commit = repo.commit("v2");
        assert_ne!(first_commit, second_commit);

        // the index was never refreshed after the second commit
        let answer = search(&store, repo.path(), "version", 10).unwrap();
        assert_eq!(answer.provenance.indexed_commit, first_commit);
        assert_eq!(answer.provenance.current_commit, Ok(second_commit));
        assert_eq!(
            answer.provenance.files_differ,
            Some(1),
            "exactly a.txt differs between the two commits"
        );
    }

    #[test]
    fn a_clean_matching_working_copy_reports_zero_drift() {
        let repo = TestRepo::init();
        repo.write("a.txt", "content");
        repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        let answer = search(&store, repo.path(), "content", 10).unwrap();
        assert_eq!(answer.provenance.files_differ, Some(0));
        assert_eq!(answer.provenance.uncommitted_changed, Some(0));
    }

    #[test]
    fn uncommitted_local_edits_are_counted_separately_from_commit_lag() {
        let repo = TestRepo::init();
        repo.write("a.txt", "original");
        repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        // dirty the working tree WITHOUT committing
        repo.write("a.txt", "edited but not committed");

        let answer = search(&store, repo.path(), "original", 10).unwrap();
        assert_eq!(answer.provenance.files_differ, Some(0), "HEAD has not moved");
        assert_eq!(answer.provenance.uncommitted_changed, Some(1), "but the working tree is dirty");
    }

    // ---- requirement 3: cheap refresh, per-file, never a forced full reindex

    #[test]
    fn refresh_with_no_new_commits_is_a_true_no_op() {
        let repo = TestRepo::init();
        repo.write("a.txt", "content");
        repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        let build = build_full(&mut store, repo.path()).unwrap();

        let stats = refresh(&mut store, repo.path()).unwrap();
        assert!(stats.no_op);
        assert_eq!(stats.files_changed, 0);
        assert_eq!(stats.previous_commit, build.commit);
        assert_eq!(stats.new_commit, build.commit);
    }

    #[test]
    fn a_refresh_after_changing_one_file_only_rereads_that_file() {
        let repo = TestRepo::init();
        repo.write("a.txt", "original a");
        repo.write("b.txt", "original b");
        repo.write("c.txt", "original c");
        repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        repo.write("b.txt", "CHANGED b");
        repo.commit("change b only");

        let stats = refresh(&mut store, repo.path()).unwrap();
        assert!(!stats.no_op);
        assert_eq!(stats.files_changed, 1, "only b.txt actually changed content");
        assert_eq!(stats.files_deleted, 0);

        // a.txt and c.txt must still read exactly as they were - proof that
        // refresh did not blindly rebuild everything
        let a = search(&store, repo.path(), "original a", 10).unwrap();
        assert_eq!(a.hits.len(), 1);
        let c = search(&store, repo.path(), "original c", 10).unwrap();
        assert_eq!(c.hits.len(), 1);
        let b_old = search(&store, repo.path(), "original b", 10).unwrap();
        assert!(b_old.hits.is_empty(), "the stale content of b.txt must be gone after refresh");
        let b_new = search(&store, repo.path(), "CHANGED b", 10).unwrap();
        assert_eq!(b_new.hits.len(), 1);
    }

    #[test]
    fn refresh_picks_up_a_newly_added_file() {
        let repo = TestRepo::init();
        repo.write("a.txt", "first");
        repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();
        assert_eq!(store.file_count().unwrap(), 1);

        repo.write("new.txt", "brand new file content");
        repo.commit("add new file");
        let stats = refresh(&mut store, repo.path()).unwrap();

        assert_eq!(stats.files_changed, 1);
        assert_eq!(store.file_count().unwrap(), 2);
        let hit = search(&store, repo.path(), "brand new", 10).unwrap();
        assert_eq!(hit.hits.len(), 1);
    }

    #[test]
    fn refresh_removes_a_deleted_file_from_the_index() {
        let repo = TestRepo::init();
        repo.write("a.txt", "keep me");
        repo.write("gone.txt", "delete me");
        repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();
        assert_eq!(store.file_count().unwrap(), 2);

        repo.remove("gone.txt");
        repo.commit("remove gone.txt");
        let stats = refresh(&mut store, repo.path()).unwrap();

        assert_eq!(stats.files_deleted, 1);
        assert_eq!(store.file_count().unwrap(), 1);
        let hit = search(&store, repo.path(), "delete me", 10).unwrap();
        assert!(hit.hits.is_empty());
    }

    #[test]
    fn refresh_follows_a_renamed_file_to_its_new_path() {
        // git's own diff reports a high-similarity move as one R(ename)
        // record, not a delete+add pair - this exercises that branch of
        // gitutil::changed_paths / refresh so it is not dead, untested code.
        let repo = TestRepo::init();
        repo.write("old_name.txt", "identical content that survives the move");
        repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();
        assert_eq!(store.file_count().unwrap(), 1);

        let out = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["mv", "old_name.txt", "new_name.txt"])
            .output()
            .unwrap();
        assert!(out.status.success(), "git mv failed: {}", String::from_utf8_lossy(&out.stderr));
        repo.commit("rename old_name.txt to new_name.txt");

        let stats = refresh(&mut store, repo.path()).unwrap();
        assert_eq!(stats.files_changed, 1, "the rename counts as one changed path");
        assert_eq!(store.file_count().unwrap(), 1, "still exactly one file, not two");

        let old = search(&store, repo.path(), "identical content", 10).unwrap();
        assert_eq!(old.hits.len(), 1);
        assert_eq!(old.hits[0].path, "new_name.txt", "found only under its new path");
    }

    #[test]
    fn refresh_updates_the_indexed_commit_even_when_nothing_changed_files_wise() {
        // An empty commit (allowed with --allow-empty) moves HEAD but
        // changes no file - refresh must still record the new commit, not
        // silently leave the index pointing at the old one.
        let repo = TestRepo::init();
        repo.write("a.txt", "content");
        let first = repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        let out = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["commit", "--quiet", "--allow-empty", "-m", "empty"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let second = gitutil::head_commit(repo.path()).unwrap();
        assert_ne!(first, second);

        let stats = refresh(&mut store, repo.path()).unwrap();
        assert!(!stats.no_op);
        assert_eq!(stats.files_changed, 0);
        assert_eq!(store.indexed_commit().unwrap(), Some(second));
    }

    // ---- binary files are archived as "not chunked", never as garbage ---

    #[test]
    fn a_binary_file_is_skipped_not_chunked_into_garbage() {
        let repo = TestRepo::init();
        repo.write_bytes("image.bin", &[0u8, 1, 2, 3, 0, 255, 254]);
        repo.write("readme.txt", "plain text file");
        repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        let stats = build_full(&mut store, repo.path()).unwrap();

        assert_eq!(stats.files_skipped_binary, 1);
        assert_eq!(stats.files_indexed, 1);
        assert_eq!(store.file_count().unwrap(), 1);
    }

    // ---- requirement 4: the human status string ---------------------------

    #[test]
    fn human_status_on_a_clean_working_copy_names_zero_of_everything() {
        let repo = TestRepo::init();
        repo.write("a.txt", "content");
        let commit = repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();
        let p = provenance(&store, repo.path()).unwrap();

        let text = human_status(&p);
        assert_eq!(
            text,
            format!(
                "Index is at commit {commit}. Your branch is at commit {commit}. 0 file(s) differ between the two. 0 file(s) in your working copy are uncommitted changes."
            )
        );
    }

    #[test]
    fn human_status_on_a_changed_working_copy_names_the_real_counts() {
        let repo = TestRepo::init();
        repo.write("a.txt", "v1");
        let first = repo.commit("v1");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        repo.write("a.txt", "v2");
        let second = repo.commit("v2");
        repo.write("a.txt", "v3 not committed");

        let p = provenance(&store, repo.path()).unwrap();
        let text = human_status(&p);
        assert_eq!(
            text,
            format!(
                "Index is at commit {first}. Your branch is at commit {second}. 1 file(s) differ between the two. 1 file(s) in your working copy are uncommitted changes."
            )
        );
    }

    // ---- chunk-level provenance travels with every hit --------------------

    #[test]
    fn every_search_hit_carries_the_commit_its_content_came_from() {
        let repo = TestRepo::init();
        repo.write("a.txt", "findable text here");
        let commit = repo.commit("initial");

        let mut store = Store::in_memory().unwrap();
        build_full(&mut store, repo.path()).unwrap();

        let answer = search(&store, repo.path(), "findable", 10).unwrap();
        assert_eq!(answer.hits.len(), 1);
        assert_eq!(answer.hits[0].commit_id, commit);
        assert_eq!(answer.hits[0].path, "a.txt");
    }

    // ------------------------------------------------------------ excerpts

    /// THE DEFECT THIS PREVENTS, seen in real use on 2026-08-03. Chunks are
    /// 200 lines, which is right for storage and far too much for an answer:
    /// a two-hit search arrived as roughly 400 lines of source. This tool's
    /// own job is to say WHERE something is written so the reader opens the
    /// real file, and burying that in the file itself is exactly the noise
    /// the rebuild exists to refuse.
    #[test]
    fn a_hit_answers_with_the_matching_lines_not_the_whole_chunk() {
        let mut body = String::new();
        for i in 1..=200 {
            body.push_str(&format!("line {i} of filler
"));
        }
        body.push_str("the needle lives here
");

        let (text, start, end) = excerpt_around_match(&body, 1, "needle");
        assert!(text.contains("the needle lives here"), "the matching line must be in the excerpt");
        let lines = text.lines().count();
        assert!(lines <= EXCERPT_CONTEXT * 2 + 1, "excerpt must stay small, got {lines} lines");
        assert!(
            lines < body.lines().count() / 10,
            "an excerpt of a 201-line chunk must be a small fraction of it, got {lines}"
        );
        assert_eq!(end, 201, "the last kept line is the match itself");
        assert_eq!(start, 201 - EXCERPT_CONTEXT as i64, "context above the match is kept");
    }

    /// Line numbers must describe the EXCERPT, not the chunk it came from -
    /// a number that points somewhere else is worse than no number.
    #[test]
    fn the_reported_lines_are_the_excerpts_own_absolute_lines() {
        let body = "a
b
TARGET
d
e
";
        // This chunk starts at line 101 of its file.
        let (text, start, end) = excerpt_around_match(body, 101, "target");
        assert!(text.contains("TARGET"), "case-insensitive match, like the SQL side");
        assert_eq!(start, 101, "three lines of context above line 3 clamps to the chunk start");
        assert_eq!(end, 105);
    }

    /// Two matches far apart must not read as one continuous piece of file.
    #[test]
    fn separated_matches_are_shown_with_an_explicit_gap() {
        let mut body = String::from("hit one
");
        for i in 0..50 {
            body.push_str(&format!("filler {i}
"));
        }
        body.push_str("hit two
");

        let (text, _, _) = excerpt_around_match(&body, 1, "hit");
        assert!(text.contains("hit one") && text.contains("hit two"));
        assert!(text.contains("..."), "a gap between kept runs must be marked: {text}");
        assert!(text.lines().count() < 20, "still an excerpt, not the chunk");
    }

    /// The fallback: never answer with nothing. An empty excerpt would read
    /// as "found it, and it is blank", which is worse than slightly wrong.
    #[test]
    fn a_chunk_with_no_matching_line_still_answers_with_something() {
        let body = "alpha
beta
gamma
";
        let (text, _, _) = excerpt_around_match(body, 1, "nothing-like-this");
        assert!(!text.trim().is_empty(), "must fall back to the head of the chunk");
    }
}
