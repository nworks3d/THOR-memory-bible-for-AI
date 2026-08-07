//! "Which project is this session in", resolved from a filesystem location -
//! the ONE place in the workspace that performs this resolution. Before this
//! module existed, `serve session-start` and the `hook` channel's SessionStart
//! branch both required the caller to name the project explicitly (see
//! `bin/serve.rs`'s old doc comment: "there is no cwd-to-project resolver in
//! this workspace"); CONTRACT.md's "session start hands back this project's
//! standing rules" cannot hold without one, so this closes that gap.
//!
//! Resolution order, cheapest and most explicit first:
//! 1. A marker file (see [`MARKER_FILE_NAMES`]) directly in the starting
//!    directory - its first non-blank line, trimmed, IS the project key.
//! 2. Otherwise the repository this directory belongs to, found by walking up
//!    to the nearest `.git`. A linked worktree carries a `.git` FILE pointing
//!    at its main repository, and resolution follows it: a worktree is a
//!    place you work, never a project of its own (see [`git_root`]).
//! 3. That repository's own marker file, if it has one.
//! 4. Otherwise that repository directory's basename.
//! 5. Otherwise, no project: the caller falls back to the global layer only.
//!
//! Steps 2 to 4 exist because both of this module's measured defects had one
//! shape: an item named a project that resolution could never produce. See
//! [`MARKER_FILE_NAMES`] and [`git_root`] for the two cases, and
//! [`orphaned_project_keys`] for the inventory that makes the next one
//! visible instead of silent.
//!
//! Every step degrades to `None` on anything unreadable (a missing directory,
//! a permission error) rather than erroring - this is a READ used by an
//! injection surface (session start), so it must never block and never speak
//! on failure (R5), exactly like `live::live_items`.
//!
//! `serve/tests/single_project_resolution.rs` greps the whole workspace for a
//! second definition of this function and fails if one appears anywhere but here.

use std::path::Path;

/// A file named this, sitting directly in a directory, wins over any git-root
/// inference for that directory - see [`resolve_project`].
pub const MARKER_FILE_NAME: &str = ".thor-project";

/// May an item declared under `item_project` be served to a session sitting in
/// `session_project`? The ONE place that answers this - every injection
/// surface calls it instead of writing the comparison out again.
///
/// - An item with no project of its own is global: it applies everywhere.
/// - An item that names a project applies only inside that project.
/// - A session whose project could not be resolved gets the global layer
///   only. That is deliberately the conservative direction: serving one
///   project's rules inside another is worse than serving fewer, because a
///   rule that does not apply here teaches the reader to skim past the block.
///
/// THE DEFECT THIS CLOSES, measured on the live store (2026-08-03). Session
/// start already scoped by project; the moment-of-action surface did not, so
/// every moment-bound rule in the store competed for that surface's four
/// slots in every project. The store holds 136 moment-bound rules belonging
/// to one business project against 66 for this one and 74 global, so a coding
/// session here was mostly being shown another project's rules - `deploy`
/// alone had 48 applicable and showed 4. It was caught in the act: writing a
/// Dockerfile for the NAS drew rules about a website's dev directory, an
/// admin HTTP route and a production database wipe. Every one of those rules
/// is correct. Not one of them was about this.
pub fn applies_to(item_project: Option<&str>, session_project: Option<&str>) -> bool {
    match item_project {
        None => true,
        Some(owner) => session_project == Some(owner),
    }
}

/// Resolve the project key for `start_dir`, or `None` for "global only".
/// Never panics, never blocks, never returns an error: every failure mode
/// (missing directory, unreadable marker file, no git root anywhere above
/// `start_dir`) collapses to `None`, the same "nothing to say" the rest of
/// the read/serve boundary uses.
pub fn resolve_project(start_dir: &Path) -> Option<String> {
    if let Some(name) = read_marker(start_dir) {
        return Some(name);
    }
    // A worktree's root is not the project - see `git_root`'s own note.
    let root = git_root(start_dir)?;
    if root != start_dir {
        if let Some(name) = read_marker(&root) {
            return Some(name);
        }
    }
    root.file_name().and_then(|n| n.to_str()).map(str::to_string)
}

/// Marker file names, in the order they are tried. `.thor-project` is this
/// build's own name; `.thor` is 1.0's, and it is honoured because it is the
/// one that actually exists on disk.
///
/// THE DEFECT THIS CLOSES, found by auditing the migration (2026-08-03).
/// 2.0 invented a new marker name and nothing ever wrote it: a filesystem
/// walk finds five `.thor` markers and zero `.thor-project`. So resolution
/// always fell through to the git-root basename. For four of the five repos
/// the basename happens to equal the key, which is why this stayed hidden.
/// For the fifth the directory is spelled with a space where the key has a
/// hyphen, so `applies_to` compared two different strings and 110 fireable
/// items - 92 rules and 18 orientations, one of them irreversible - could
/// never be selected in their own checkout. Reproduced live: asking that
/// repo what governs one of its own files answered "no item governs this".
///
/// Harmless until the moment surface started scoping by project earlier the
/// same day, exactly like the `project: "global"` case. A correct filter over
/// an identity nobody translated deletes whatever the identity names.
const MARKER_FILE_NAMES: &[&str] = &[MARKER_FILE_NAME, ".thor"];

/// The first marker file's first non-blank line, trimmed - `None` when none
/// is present, unreadable, or empty/whitespace-only (an empty marker names
/// nothing, so resolution falls through rather than stopping on a blank
/// answer).
fn read_marker(dir: &Path) -> Option<String> {
    for name in MARKER_FILE_NAMES {
        let Ok(content) = std::fs::read_to_string(dir.join(name)) else { continue };
        let Some(line) = content.lines().next().map(str::trim) else { continue };
        if !line.is_empty() {
            return Some(line.to_string());
        }
    }
    None
}

/// Walk `start_dir` and its ancestors (inclusive) looking for the nearest one
/// that contains a `.git` entry, and return THAT DIRECTORY. `None` when no
/// ancestor up to the filesystem root has one.
///
/// Public because the checkout a project lives in is the other half of
/// resolving it, and callers that need both (the MCP server picking which
/// code index applies to the directory it was started in) must not walk for
/// it themselves - the whole point of this module is that project resolution
/// happens in exactly one place (CONTRACT R6).
/// A LINKED WORKTREE resolves to the repository it belongs to, not to itself.
///
/// THE DEFECT THIS CLOSES, found by auditing the migration (2026-08-03). In a
/// linked worktree `.git` is a FILE, not a directory, holding a line like
/// `gitdir: <main>/.git/worktrees/<slug>`. The old walk stopped there and
/// handed back the worktree directory, whose basename is a generated slug.
/// Reproduced live: starting a session inside one of the owner's worktrees
/// resolved the project to `hungry-wright-171e5d`, so every rule that repo
/// owns was scoped out of every pushed surface - in the very checkout where
/// the work was happening. This build's own agent tooling creates worktrees,
/// so it is not a corner case.
///
/// Read from the file rather than by running git: this is on the path of a
/// hook that fires on every tool call, and it must never block on a
/// subprocess. Anything unexpected in the file falls back to the directory
/// itself, which is the old behaviour and never worse than it.
pub fn git_root(start_dir: &Path) -> Option<std::path::PathBuf> {
    let mut dir: Option<&Path> = Some(start_dir);
    while let Some(d) = dir {
        let dot_git = d.join(".git");
        if dot_git.is_dir() {
            return Some(d.to_path_buf());
        }
        if dot_git.is_file() {
            return Some(main_root_of_worktree(&dot_git).unwrap_or_else(|| d.to_path_buf()));
        }
        dir = d.parent();
    }
    None
}

/// The main repository root a linked worktree points at, read from its `.git`
/// file. `None` for any shape this does not recognise, so the caller keeps
/// the old answer instead of inventing one.
fn main_root_of_worktree(dot_git_file: &Path) -> Option<std::path::PathBuf> {
    let content = std::fs::read_to_string(dot_git_file).ok()?;
    let gitdir = content.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
    let gitdir = Path::new(gitdir);
    // <main>/.git/worktrees/<slug>  ->  <main>
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let git_dir = worktrees.parent()?;
    if git_dir.file_name()? != ".git" {
        return None;
    }
    git_dir.parent().map(Path::to_path_buf)
}

/// Every project key held by a live item that NO reachable checkout resolves
/// to. The inventory `doctor` needs, because the two defects this module has
/// now had are both of one shape - an item names a project nothing answers to
/// - and neither was visible from any surface. A rule scoped out is
/// indistinguishable from a rule that simply never matched.
///
/// `resolvable` is what the caller found by resolving every checkout it knows
/// about; anything an item claims that is not in there is orphaned.
pub fn orphaned_project_keys<'a>(
    item_projects: impl IntoIterator<Item = &'a str>,
    resolvable: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeMap<String, usize> {
    let mut out: std::collections::BTreeMap<String, usize> = Default::default();
    for key in item_projects {
        if !resolvable.contains(key) {
            *out.entry(key.to_string()).or_default() += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_marker_file_names_the_project_directly() {
        let dir = tempdir();
        fs::write(dir.path().join(MARKER_FILE_NAME), "widget-factory\n").unwrap();
        assert_eq!(resolve_project(dir.path()), Some("widget-factory".to_string()));
    }

    #[test]
    fn a_marker_file_wins_over_a_git_roots_basename() {
        let dir = tempdir();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(MARKER_FILE_NAME), "overridden-name\n").unwrap();
        assert_eq!(resolve_project(dir.path()), Some("overridden-name".to_string()));
    }

    #[test]
    fn the_git_roots_basename_is_used_when_there_is_no_marker() {
        let dir = tempdir();
        let repo = dir.path().join("repo-name");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(resolve_project(&repo), Some("repo-name".to_string()));
    }

    #[test]
    fn a_subdirectory_of_a_git_repo_still_resolves_the_repo_roots_name() {
        let dir = tempdir();
        let repo = dir.path().join("repo-name");
        let sub = repo.join("src").join("deep");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(resolve_project(&sub), Some("repo-name".to_string()));
    }

    #[test]
    fn a_git_file_not_only_a_git_directory_counts_as_a_repo_root() {
        // Worktrees and submodules record `.git` as a FILE pointing elsewhere,
        // not a directory - this must count too, since this function only
        // checks for the entry's existence, never its shape.
        let dir = tempdir();
        let repo = dir.path().join("worktree-name");
        fs::create_dir(&repo).unwrap();
        fs::write(repo.join(".git"), "gitdir: /somewhere/else\n").unwrap();
        assert_eq!(resolve_project(&repo), Some("worktree-name".to_string()));
    }

    #[test]
    fn no_marker_and_no_git_root_yields_no_project() {
        let dir = tempdir();
        assert_eq!(resolve_project(dir.path()), None, "an ordinary directory outside any git repo must resolve to global-only");
    }

    #[test]
    fn a_marker_in_a_parent_directory_is_never_inherited_by_a_child() {
        // Pins the deliberate scope choice: unlike the git-root walk, the
        // marker check never climbs - it names the directory you are
        // standing in, not something a parent folder hints at.
        let dir = tempdir();
        let child = dir.path().join("child");
        fs::create_dir(&child).unwrap();
        fs::write(dir.path().join(MARKER_FILE_NAME), "parent-project\n").unwrap();
        assert_eq!(resolve_project(&child), None, "a parent's marker must not leak into its child");
    }

    #[test]
    fn a_blank_marker_falls_through_to_the_git_root() {
        let dir = tempdir();
        let repo = dir.path().join("repo-name");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        fs::write(repo.join(MARKER_FILE_NAME), "   \n").unwrap();
        assert_eq!(resolve_project(&repo), Some("repo-name".to_string()), "a whitespace-only marker names nothing, so the git root still applies");
    }

    #[test]
    fn a_missing_starting_directory_resolves_to_no_project_not_an_error() {
        let dir = tempdir();
        let ghost = dir.path().join("does-not-exist");
        assert_eq!(resolve_project(&ghost), None);
    }

    #[test]
    fn only_the_first_line_of_the_marker_is_used() {
        let dir = tempdir();
        fs::write(dir.path().join(MARKER_FILE_NAME), "real-name\nextra ignored content\n").unwrap();
        assert_eq!(resolve_project(dir.path()), Some("real-name".to_string()));
    }

    // ------------------------------------- the two measured resolution bugs

    /// THE DEFECT THIS PREVENTS (2026-08-03). 2.0 invented the marker name
    /// `.thor-project` and nothing ever wrote one: the owner's machine holds
    /// five `.thor` markers and zero `.thor-project`. Resolution therefore
    /// always fell through to the git-root basename, which happened to equal
    /// the key for four of five repos - and for the fifth the directory has a
    /// space where the key has a hyphen. 110 fireable items, one of them
    /// irreversible, could never be selected in their own checkout.
    #[test]
    fn the_one_zero_marker_name_still_names_the_project() {
        let dir = tempdir();
        let repo = dir.path().join("acetone smoother");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".thor"), "Acetone-Smoother
").unwrap();
        assert_eq!(
            resolve_project(&repo).as_deref(),
            Some("Acetone-Smoother"),
            "a legacy marker must win over the directory's own spelling"
        );
    }

    /// The new name still wins when both are present - a repo that adopts
    /// 2.0's own marker is not overruled by a leftover.
    #[test]
    fn the_two_zero_marker_name_wins_when_both_exist() {
        let dir = tempdir();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".thor"), "old-key
").unwrap();
        fs::write(repo.join(MARKER_FILE_NAME), "new-key
").unwrap();
        assert_eq!(resolve_project(&repo).as_deref(), Some("new-key"));
    }

    /// THE DEFECT THIS PREVENTS (2026-08-03). In a linked worktree `.git` is
    /// a FILE pointing at `<main>/.git/worktrees/<slug>`. Resolution used to
    /// stop there and hand back the worktree's own basename - a generated
    /// slug - so every rule the repository owns was scoped out in the very
    /// checkout where the work was happening. Reproduced live before the fix:
    /// a session in one of the owner's worktrees resolved to a slug.
    #[test]
    fn a_linked_worktree_resolves_to_the_repository_it_belongs_to() {
        let dir = tempdir();
        let main = dir.path().join("acme-shop");
        fs::create_dir_all(main.join(".git").join("worktrees").join("hungry-wright-171e5d")).unwrap();

        let wt = dir.path().join("hungry-wright-171e5d");
        fs::create_dir_all(&wt).unwrap();
        let gitdir = main.join(".git").join("worktrees").join("hungry-wright-171e5d");
        fs::write(wt.join(".git"), format!("gitdir: {}
", gitdir.display())).unwrap();

        assert_eq!(git_root(&wt).as_deref(), Some(main.as_path()), "the main repo, not the worktree");
        assert_eq!(
            resolve_project(&wt).as_deref(),
            Some("acme-shop"),
            "a worktree is a place you work, never a project of its own"
        );
    }

    /// And it picks up the repository's marker, not just its basename.
    #[test]
    fn a_worktree_inherits_its_repositorys_marker() {
        let dir = tempdir();
        let main = dir.path().join("checkout-dir");
        fs::create_dir_all(main.join(".git").join("worktrees").join("slug")).unwrap();
        fs::write(main.join(".thor"), "Real-Project
").unwrap();

        let wt = dir.path().join("slug");
        fs::create_dir_all(&wt).unwrap();
        let gitdir = main.join(".git").join("worktrees").join("slug");
        fs::write(wt.join(".git"), format!("gitdir: {}
", gitdir.display())).unwrap();

        assert_eq!(resolve_project(&wt).as_deref(), Some("Real-Project"));
    }

    /// A `.git` file this does not recognise must never make things worse
    /// than the old behaviour.
    #[test]
    fn an_unrecognised_git_file_falls_back_to_the_directory_itself() {
        let dir = tempdir();
        let odd = dir.path().join("odd-checkout");
        fs::create_dir_all(&odd).unwrap();
        fs::write(odd.join(".git"), "something entirely different
").unwrap();
        assert_eq!(resolve_project(&odd).as_deref(), Some("odd-checkout"));
    }

    /// The inventory that makes the NEXT one of these visible instead of
    /// silent: a key no checkout resolves to is reported, with its weight.
    #[test]
    fn orphaned_keys_are_reported_with_how_many_items_hold_them() {
        let resolvable: std::collections::BTreeSet<String> =
            ["Sensor-Board".to_string(), "acme-shop".to_string()].into_iter().collect();
        let held = ["acme-shop", "Acetone-Smoother", "Acetone-Smoother", "Sensor-Board"];
        let orphans = orphaned_project_keys(held, &resolvable);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans.get("Acetone-Smoother"), Some(&2));
    }
}
