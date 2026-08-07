//! The runner for `crate::item::Check`: runs one machine-runnable check
//! against a root the caller supplies, and reports one of three outcomes -
//! `Holds`, `Fails`, `CannotRun` - never a boolean. Collapsing "could not
//! tell" into "failed" would read a check that never actually ran as if it
//! had proven the fact false, which is worse than not running it at all.
//!
//! This module ends at the model boundary: nothing here is wired into
//! anything that blocks, serves, or ranks (see `gate::declare` for the
//! write-time rules on when an item may carry a check at all - that is
//! where this stops). It only runs a check and reports what happened.
//!
//! Four of the five `Check` forms answer by reading a file relative to
//! `root`. The fifth, `Check::Forbidden`, carries no path at all and so
//! never touches the filesystem - `run`'s own doc comment on its early
//! return explains why `Outcome::Holds` is the only answer that could ever
//! be correct for it.

use crate::item::Check;
use std::path::{Component, Path, PathBuf};

/// What running a `Check` found.
///
/// Three outcomes, never a boolean: `CannotRun` is a real, distinct answer
/// from both `Holds` and `Fails` - it means the check proved nothing either
/// way, not that it failed. Only a `Holds` outcome, observed at read time,
/// is ever meant to stand in for a prose falsifier that might already be
/// stale - a `Fails` or a `CannotRun` says nothing about whether the item's
/// TEXT is still true, only about the check itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The check's condition is true right now.
    Holds,
    /// The check's condition is false right now, decided by an actual
    /// look: `PathExists` on a path that is definitely not there, or
    /// `Contains`/`Absent` on a file that WAS read successfully and either
    /// did or did not hold the literal.
    Fails,
    /// The check could not be run at all, so it proves nothing either way:
    /// the file is missing (for `Contains`/`Absent` only - a missing file
    /// was never actually read, so "does it contain X" has no answer), any
    /// IO error, the content is not valid UTF-8, the file is over
    /// `MAX_CHECK_FILE_BYTES`, or the path does not stay inside the
    /// supplied root.
    CannotRun,
}

/// The largest file this runner will read into memory, in bytes: 1 MiB
/// (1,048,576 bytes).
///
/// Not a measured value - this is the first check any item can carry, so
/// there is no real corpus of check targets yet to measure. Chosen as a
/// round, generous bound instead: every realistic target (a piece of source
/// code, a doc file, a config file) measures in the tens of kilobytes at
/// most, so 1 MiB is headroom above any of them, while still bounding the
/// worst case if a check is ever pointed at a large generated or binary
/// file - one bad check can never force this runner to read an unbounded
/// amount of data into memory.
pub const MAX_CHECK_FILE_BYTES: u64 = 1_048_576;

/// The path named by `check`, before it is resolved against a root - `None`
/// for `Forbidden`, the one form with no path at all (see its own doc
/// comment on `Check`). `run` below treats `None` as a complete answer in
/// its own right and never reaches the root-resolution machinery this
/// exists to feed.
fn named_path(check: &Check) -> Option<&str> {
    match check {
        Check::PathExists { path } => Some(path),
        Check::Contains { path, .. } => Some(path),
        Check::Absent { path, .. } => Some(path),
        Check::AbsentAll { path, .. } => Some(path),
        Check::Forbidden { .. } => None,
    }
}

/// Resolve `relative` against `root`, refusing anything that could reach
/// outside it. Purely lexical - no filesystem access, so this works whether
/// or not the target exists, which matters because a missing target is a
/// real, meaningful outcome for every check form (see `Outcome`), not an
/// error to raise here.
///
/// Two escapes are refused, neither of which needs the path to exist to
/// recognise:
///   - an absolute path. Plain `Path::join` does not report an error for
///     this - it REPLACES `root` with the absolute path outright (a well
///     known footgun: `Path::new("/root").join("/etc/passwd")` is
///     `/etc/passwd`, not an error). `Path::is_absolute` alone is not
///     enough on Windows either: a root-anchored path with no drive prefix
///     (`\temp`) is not "absolute" by that method, yet `join` still
///     replaces everything except the prefix of `root` with it. Both
///     shapes are refused explicitly below, via `Component::Prefix` and
///     `Component::RootDir`, rather than trusting `is_absolute` alone.
///   - a `..` component, which walks back out of `root` one level at a
///     time regardless of whether the walk ever touches disk.
///
/// Returns `None` for either escape, and for an empty (or whitespace-only)
/// path, since there is nothing to resolve.
fn resolve_within_root(root: &Path, relative: &str) -> Option<PathBuf> {
    if relative.trim().is_empty() {
        return None;
    }
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return None;
    }
    for component in candidate.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return None,
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Some(root.join(candidate))
}

/// The second, non-lexical half of the containment guard: whether
/// `resolved` still lands inside `root` once every reparse point along the
/// way (a symlink or, on Windows, an NTFS junction/mount point) is actually
/// followed.
///
/// `resolve_within_root` above only rules out `..` and an absolute/
/// root-anchored path, purely by looking at the text of the path - it says
/// nothing about a symlink planted INSIDE `root` whose target lives
/// outside it (`root/link -> C:\Windows`, then a check path of
/// `link/whatever` is lexically clean but reads through `link` to
/// somewhere this check was never supposed to reach). Resolving that
/// requires an actual look at the filesystem, which is what this function
/// does, via `Path::canonicalize` (`std` alone - on Windows this calls
/// `GetFinalPathNameByHandleW`, which resolves every reparse point in the
/// path, symlink or junction, and needs no elevated privilege to do so:
/// only CREATING a symlink is privileged on Windows, not following one).
///
/// `resolved` need not exist: only the nearest EXISTING ancestor is ever
/// canonicalised, walking upward one component at a time. This matters for
/// two reasons:
///   - a missing target is a real, meaningful outcome for every check form
///     (see `Outcome`), never an error to raise here - `PathExists` on a
///     path that is simply not there must still come back `Fails`, not
///     `CannotRun`, exactly as it did before this guard existed.
///   - a symlink can still sit on an EXISTING ancestor of an otherwise
///     missing leaf (`root/link/not-written-yet.txt`, where `link` itself
///     escapes) - canonicalising only the leaf would miss that case
///     entirely, so the walk keeps popping the last component off and
///     retrying until it finds something that is actually there.
///
/// Returns `true` (treat as an escape) whenever `root` itself cannot be
/// canonicalised - there is then nothing to prove containment against, so
/// the safe default is to refuse rather than to guess.
fn escapes_root(root: &Path, resolved: &Path) -> bool {
    let Ok(canonical_root) = root.canonicalize() else {
        return true;
    };
    let mut candidate = resolved.to_path_buf();
    loop {
        match candidate.canonicalize() {
            Ok(canonical) => return !canonical.starts_with(&canonical_root),
            Err(_) => {
                if !candidate.pop() {
                    // No existing ancestor at all - only possible if `root`
                    // itself does not exist, already handled above. Kept as
                    // a safe, non-panicking default rather than reachable
                    // in practice.
                    return false;
                }
            }
        }
    }
}

/// Read `path` as a UTF-8 string, provided it exists, is a regular file
/// (not a directory), and is at most `MAX_CHECK_FILE_BYTES`. Every failure
/// mode collapses to `None` rather than a distinguishable error: the only
/// use `run` has for this is "so I can look for a literal in it", and every
/// one of these failure modes means the identical thing for that purpose -
/// this check cannot run. Never panics: every step is a `Result`/`Option`
/// combinator, never an `unwrap`.
fn read_bounded_utf8(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    if metadata.len() > MAX_CHECK_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

/// Run `check` against `root`. Never panics, and never reads or reports on
/// anything outside `root` - a path that tries to escape it comes back as
/// `Outcome::CannotRun`, the same answer as any other reason this check
/// cannot prove anything, without the path ever being opened. Containment is
/// checked twice: `resolve_within_root` refuses a `..`/absolute/root-anchored
/// path lexically, and `escapes_root` then refuses a path that is lexically
/// clean but reads, through a symlink or junction planted inside `root`, to
/// somewhere outside it - see that function's own doc comment.
///
/// `Check::Forbidden` carries no path at all, so none of the above ever runs
/// for it: `named_path` returns `None`, and this function returns
/// `Outcome::Holds` immediately, touching neither `root` nor the
/// filesystem - proven by `a_forbidden_check_holds_even_against_a_root_that_
/// does_not_exist` below, which hands it a root that was never created.
///
/// That answer is not a shortcut or a default standing in for "unknown" -
/// it is the only answer that could ever be correct here. `Forbidden` names
/// a standing prohibition with nothing external backing it ("never write an
/// em dash" depends on no file, no directory, nothing that could change out
/// from under it), so there is no state to inspect that could make it
/// `Fails`, and no failure mode (a missing file, an escape attempt, a read
/// error) that could make it `CannotRun` either - both of those outcomes
/// exist to report on an attempt to look at something, and `Forbidden` gives
/// this function nothing to look at. `Holds`, unconditionally, is simply
/// what "the check's condition is true right now" (`Outcome::Holds`'s own
/// doc comment) means for a condition with no external state left to be
/// false about.
pub fn run(check: &Check, root: &Path) -> Outcome {
    let Some(path) = named_path(check) else {
        return Outcome::Holds;
    };
    let Some(resolved) = resolve_within_root(root, path) else {
        return Outcome::CannotRun;
    };
    if escapes_root(root, &resolved) {
        return Outcome::CannotRun;
    }
    match check {
        Check::PathExists { .. } => {
            // Existence is exactly what this variant measures: a path that
            // is not there is a real, decided "no" - `Fails`, never
            // `CannotRun`. This is different from `Contains`/`Absent`
            // below, where a missing file means the literal's presence was
            // never actually looked at.
            if resolved.exists() {
                Outcome::Holds
            } else {
                Outcome::Fails
            }
        }
        Check::Contains { literal, .. } => match read_bounded_utf8(&resolved) {
            Some(content) => {
                if content.contains(literal.as_str()) {
                    Outcome::Holds
                } else {
                    Outcome::Fails
                }
            }
            None => Outcome::CannotRun,
        },
        Check::Absent { literal, .. } => match read_bounded_utf8(&resolved) {
            Some(content) => {
                if content.contains(literal.as_str()) {
                    Outcome::Fails
                } else {
                    Outcome::Holds
                }
            }
            None => Outcome::CannotRun,
        },
        Check::AbsentAll { literals, .. } => match read_bounded_utf8(&resolved) {
            // Set form of Absent immediately above: Holds only when NONE of
            // the literals is present, Fails as soon as any single one is -
            // same CannotRun conditions either way, since both forms share
            // the identical read_bounded_utf8 step.
            Some(content) => {
                if literals.iter().any(|literal| content.contains(literal.as_str())) {
                    Outcome::Fails
                } else {
                    Outcome::Holds
                }
            }
            None => Outcome::CannotRun,
        },
        // Unreachable in practice - the early return above already answers
        // for Forbidden, since `named_path` gives it no path to resolve -
        // but a real, correct arm rather than a panic: exhaustive by
        // construction, and still the right answer even if some future
        // change to the guard above ever let a Forbidden value reach here.
        Check::Forbidden { .. } => Outcome::Holds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // -------------------------------------------------------- PathExists

    #[test]
    fn a_path_that_exists_holds() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("here.txt"), b"anything").unwrap();
        let check = Check::PathExists { path: "here.txt".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::Holds);
    }

    #[test]
    fn a_path_that_is_missing_fails_it_is_not_cannot_run() {
        // THE DEFECT THIS PREVENTS: existence is exactly what PathExists
        // measures, so a missing path is a decided "no" - collapsing it
        // into CannotRun would hide a real, useful answer behind "could not
        // tell".
        let dir = tempfile::tempdir().unwrap();
        let check = Check::PathExists { path: "never-written.txt".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::Fails);
    }

    #[test]
    fn path_exists_holds_for_a_directory_too() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        let check = Check::PathExists { path: "subdir".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::Holds);
    }

    // ---------------------------------------------------------- Contains

    #[test]
    fn a_file_containing_the_literal_holds_for_contains() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("LICENSE"), b"Licensed under GPLv3, see COPYING").unwrap();
        let check = Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::Holds);
    }

    #[test]
    fn a_file_missing_the_literal_fails_for_contains() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("LICENSE"), b"Licensed under MIT").unwrap();
        let check = Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::Fails);
    }

    #[test]
    fn a_missing_file_cannot_run_a_contains_check() {
        // THE DEFECT THIS PREVENTS: a missing file is not "does not contain
        // the literal" (that would be Holds for Absent / Fails for
        // Contains) - it was never read, so it proves nothing either way.
        let dir = tempfile::tempdir().unwrap();
        let check = Check::Contains { path: "never-written.txt".to_string(), literal: "x".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    // ------------------------------------------------------------ Absent

    #[test]
    fn a_file_without_the_literal_holds_for_absent() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), b"fn main() {}").unwrap();
        let check = Check::Absent { path: "main.rs".to_string(), literal: "TODO".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::Holds);
    }

    #[test]
    fn a_file_with_the_literal_fails_for_absent() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), b"fn main() { /* TODO */ }").unwrap();
        let check = Check::Absent { path: "main.rs".to_string(), literal: "TODO".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::Fails);
    }

    #[test]
    fn a_missing_file_cannot_run_an_absent_check() {
        let dir = tempfile::tempdir().unwrap();
        let check = Check::Absent { path: "never-written.txt".to_string(), literal: "TODO".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    // --------------------------------------------------------- AbsentAll
    //
    // The set form of Absent: holds only when NONE of the literals is
    // present, fails as soon as ANY single one is. Each test below is named
    // after the exact failure mode it pins shut.

    #[test]
    fn a_file_containing_none_of_the_literals_holds_for_absent_all() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("STYLE.md"), b"plain ascii hyphens only").unwrap();
        let check = Check::AbsentAll {
            path: "STYLE.md".to_string(),
            literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "...".to_string()],
        };
        assert_eq!(run(&check, dir.path()), Outcome::Holds);
    }

    #[test]
    fn a_file_containing_the_first_literal_fails_for_absent_all() {
        // THE DEFECT THIS PREVENTS: a naive implementation that only ever
        // checks the LAST literal in the list (a copy-paste of the Absent
        // arm that forgot to loop) would miss a violation sitting in an
        // earlier position.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("STYLE.md"), "an em dash \u{2014} sneaks in here".as_bytes()).unwrap();
        let check = Check::AbsentAll {
            path: "STYLE.md".to_string(),
            literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "...".to_string()],
        };
        assert_eq!(run(&check, dir.path()), Outcome::Fails);
    }

    #[test]
    fn a_file_containing_a_middle_literal_fails_for_absent_all() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("STYLE.md"), "an en dash \u{2013} sneaks in here".as_bytes()).unwrap();
        let check = Check::AbsentAll {
            path: "STYLE.md".to_string(),
            literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "...".to_string()],
        };
        assert_eq!(run(&check, dir.path()), Outcome::Fails);
    }

    #[test]
    fn a_file_containing_the_last_literal_fails_for_absent_all() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("STYLE.md"), "an ellipsis ... sneaks in here".as_bytes()).unwrap();
        let check = Check::AbsentAll {
            path: "STYLE.md".to_string(),
            literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "...".to_string()],
        };
        assert_eq!(run(&check, dir.path()), Outcome::Fails);
    }

    #[test]
    fn a_missing_file_cannot_run_an_absent_all_check() {
        // Same CannotRun/Fails distinction Absent itself makes: a file that
        // was never read proves nothing about which literals it holds, so
        // this must not read back as Holds just because nothing was found.
        let dir = tempfile::tempdir().unwrap();
        let check = Check::AbsentAll {
            path: "never-written.txt".to_string(),
            literals: vec!["TODO".to_string()],
        };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    #[test]
    fn an_absent_all_check_escaping_the_root_with_dot_dot_cannot_run() {
        // Mirrors `a_contains_check_escaping_the_root_with_dot_dot_cannot_
        // run`: the shared root-containment guard in `run` sits BEFORE the
        // per-variant match, so it protects AbsentAll exactly like every
        // other form, never a second implementation to keep in sync.
        let outer = tempfile::tempdir().unwrap();
        fs::write(outer.path().join("secret.txt"), b"outside the root").unwrap();
        let root = outer.path().join("root");
        fs::create_dir(&root).unwrap();

        let check = Check::AbsentAll { path: "../secret.txt".to_string(), literals: vec!["outside".to_string()] };
        assert_eq!(run(&check, &root), Outcome::CannotRun);
    }

    // ---------------------------------------------------- IO / encoding / size

    #[test]
    fn non_utf8_content_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        // Lone 0xFF byte: not valid UTF-8 on its own or in any continuation.
        fs::write(dir.path().join("bin.dat"), [0xFFu8, 0x00, 0xFF]).unwrap();
        let check = Check::Contains { path: "bin.dat".to_string(), literal: "x".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    #[test]
    fn a_file_over_the_size_limit_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let oversized = vec![b'a'; (MAX_CHECK_FILE_BYTES + 1) as usize];
        fs::write(dir.path().join("big.txt"), &oversized).unwrap();
        let check = Check::Contains { path: "big.txt".to_string(), literal: "a".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    #[test]
    fn a_file_at_exactly_the_size_limit_can_still_run() {
        let dir = tempfile::tempdir().unwrap();
        let exact = vec![b'a'; MAX_CHECK_FILE_BYTES as usize];
        fs::write(dir.path().join("exact.txt"), &exact).unwrap();
        let check = Check::Contains { path: "exact.txt".to_string(), literal: "a".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::Holds);
    }

    #[test]
    fn a_check_against_a_directory_instead_of_a_file_cannot_run_and_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        let check = Check::Contains { path: "subdir".to_string(), literal: "x".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    // ------------------------------------------------------- root escape
    //
    // THE DEFECT THESE PREVENT: a check whose path climbs out of the root it
    // was handed could read (or prove the presence/absence of a literal in)
    // any file the process can see, far beyond whatever the item was meant
    // to anchor. Every escape attempt must come back CannotRun WITHOUT the
    // path ever being opened - proven here by pointing the escape at a real
    // file that exists just outside the root and confirming it is never
    // read.

    #[test]
    fn a_path_exists_check_escaping_the_root_with_dot_dot_cannot_run() {
        let outer = tempfile::tempdir().unwrap();
        fs::write(outer.path().join("secret.txt"), b"outside the root").unwrap();
        let root = outer.path().join("root");
        fs::create_dir(&root).unwrap();

        let check = Check::PathExists { path: "../secret.txt".to_string() };
        // Sanity check: the escaped file really is there, so a CannotRun
        // here can only mean the guard fired - not that PathExists
        // correctly reported a missing file.
        assert!(outer.path().join("secret.txt").exists());
        assert_eq!(run(&check, &root), Outcome::CannotRun);
    }

    #[test]
    fn a_contains_check_escaping_the_root_with_dot_dot_cannot_run() {
        // Same guard, proven again on a form that would otherwise read file
        // content - confirms the escape is refused before any read is
        // attempted, not just for the variant that only checks existence.
        let outer = tempfile::tempdir().unwrap();
        fs::write(outer.path().join("secret.txt"), b"outside the root").unwrap();
        let root = outer.path().join("root");
        fs::create_dir(&root).unwrap();

        let check = Check::Contains { path: "../secret.txt".to_string(), literal: "outside".to_string() };
        assert_eq!(run(&check, &root), Outcome::CannotRun);
    }

    #[test]
    fn an_absolute_path_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let absolute = outside.path().join("secret.txt");
        fs::write(&absolute, b"outside the root").unwrap();

        let check = Check::PathExists { path: absolute.to_string_lossy().into_owned() };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    #[cfg(windows)]
    #[test]
    fn a_root_anchored_path_without_a_drive_prefix_cannot_run() {
        // Windows-specific footgun: `\temp` is NOT `Path::is_absolute()`
        // (it has a root but no drive prefix), yet plain `Path::join` still
        // lets it replace everything but `root`'s own drive prefix. Proven
        // separately from `an_absolute_path_cannot_run` because it exercises
        // a different `Component` (`RootDir` without `Prefix`), which a
        // guard that only checked `is_absolute()` would miss entirely.
        let dir = tempfile::tempdir().unwrap();
        let naive_join = dir.path().join("\\temp");
        assert_ne!(
            naive_join, dir.path().join("temp"),
            "this test's premise: plain Path::join does not treat \\temp as a normal relative segment"
        );

        let check = Check::PathExists { path: "\\temp".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    #[test]
    fn an_empty_path_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let check = Check::PathExists { path: "".to_string() };
        assert_eq!(run(&check, dir.path()), Outcome::CannotRun);
    }

    // ------------------------------------------------ root escape via a link
    //
    // THE GAP THESE CLOSE: every escape proven above is caught lexically, by
    // `resolve_within_root`, before the filesystem is ever touched - none of
    // them prove anything about a path that is lexically clean but reads,
    // through a reparse point PLANTED INSIDE the root, to somewhere outside
    // it (`root/link -> C:\Windows`, then a check path of
    // `link/whatever` never contains `..` or a drive prefix at all).
    // `escapes_root` is the guard that closes this, by actually resolving
    // the path with `Path::canonicalize` and checking containment for real.
    //
    // A true NTFS symlink needs an elevated process or Developer Mode to
    // CREATE (confirmed by hand in this environment: `New-Item -ItemType
    // SymbolicLink` fails with "Administrator privilege required for this
    // operation") - but not to FOLLOW, which is all `escapes_root` ever
    // does. An NTFS junction is a different reparse point that any user may
    // create, and Windows resolves it through the exact same
    // `GetFinalPathNameByHandle` call `Path::canonicalize` makes for a
    // symlink - proven identical below, by canonicalising both sides of a
    // junction and asserting the results are equal, before trusting it as
    // this guard's test double for a symlink.

    #[cfg(windows)]
    fn make_junction(link: &Path, target: &Path) {
        let out = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "mklink /J must succeed for this test's premise to hold: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_resolves_through_canonicalize_exactly_like_a_symlink_would() {
        // This test's own premise, checked before trusting any test below
        // it: `Path::canonicalize` really does follow a junction, all the
        // way to the same canonical path as the real target directory.
        let outer = tempfile::tempdir().unwrap();
        let outside = outer.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let link = outer.path().join("link");
        make_junction(&link, &outside);

        assert_eq!(
            link.canonicalize().unwrap(),
            outside.canonicalize().unwrap(),
            "a junction must canonicalise to the same path as its real target"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_path_exists_check_through_a_junction_that_escapes_the_root_cannot_run() {
        let outer = tempfile::tempdir().unwrap();
        let outside = outer.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"outside the root").unwrap();

        let root = outer.path().join("root");
        fs::create_dir(&root).unwrap();
        let junction = root.join("escape-junction");
        make_junction(&junction, &outside);

        // Sanity check: the escaped file really is reachable through the
        // junction on disk, so a CannotRun here can only mean the guard
        // fired - not that the path never resolved to anything.
        assert!(junction.join("secret.txt").exists());

        let check = Check::PathExists { path: "escape-junction/secret.txt".to_string() };
        assert_eq!(run(&check, &root), Outcome::CannotRun);
    }

    #[cfg(windows)]
    #[test]
    fn a_contains_check_through_a_junction_that_escapes_the_root_cannot_run() {
        // Same guard, proven again on a form that would otherwise read file
        // content, mirroring `a_contains_check_escaping_the_root_with_dot_
        // dot_cannot_run` above - confirms this is refused before any read
        // is attempted, not just for the variant that only checks existence.
        let outer = tempfile::tempdir().unwrap();
        let outside = outer.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"outside the root").unwrap();

        let root = outer.path().join("root");
        fs::create_dir(&root).unwrap();
        let junction = root.join("escape-junction");
        make_junction(&junction, &outside);

        let check = Check::Contains { path: "escape-junction/secret.txt".to_string(), literal: "outside".to_string() };
        assert_eq!(run(&check, &root), Outcome::CannotRun);
    }

    #[cfg(windows)]
    #[test]
    fn a_missing_leaf_past_an_escaping_junction_cannot_run_not_fails() {
        // The other half of `escapes_root`'s own doc comment: the escape can
        // sit on an existing ANCESTOR even when the leaf itself is missing -
        // proves the walk-up-to-the-nearest-existing-ancestor loop, not just
        // the all-exists case above. Must come back CannotRun, not the
        // ordinary Fails a ContainsCheck a merely-missing file gets
        // (`a_missing_file_cannot_run_a_contains_check`) - the leaf being
        // missing is incidental here; the escaping ancestor is the reason.
        let outer = tempfile::tempdir().unwrap();
        let outside = outer.path().join("outside");
        fs::create_dir(&outside).unwrap();

        let root = outer.path().join("root");
        fs::create_dir(&root).unwrap();
        let junction = root.join("escape-junction");
        make_junction(&junction, &outside);

        assert!(!junction.join("never-written.txt").exists(), "sanity: the leaf really is missing");

        let check = Check::PathExists { path: "escape-junction/never-written.txt".to_string() };
        assert_eq!(run(&check, &root), Outcome::CannotRun);
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_that_stays_inside_the_root_does_not_false_positive() {
        // The guard must not be so broad that it refuses every reparse
        // point - only ones that actually escape `root`. A junction planted
        // inside root, pointing at another directory also inside root, must
        // still run exactly as if there were no junction at all.
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("root");
        fs::create_dir(&root).unwrap();
        let inside = root.join("inside");
        fs::create_dir(&inside).unwrap();
        fs::write(inside.join("here.txt"), b"anything").unwrap();

        let junction = root.join("link-to-inside");
        make_junction(&junction, &inside);

        let check = Check::PathExists { path: "link-to-inside/here.txt".to_string() };
        assert_eq!(
            run(&check, &root),
            Outcome::Holds,
            "a reparse point that stays inside root must not be refused"
        );
    }

    // ------------------------------------------------------------ Forbidden
    //
    // Unlike every form above, Forbidden carries no path - there is no file
    // whose current content could ever make it Fails or CannotRun, so it
    // always Holds, and it never touches the filesystem to get there. Each
    // test below is named after the exact property it pins shut.

    #[test]
    fn a_forbidden_check_holds() {
        let dir = tempfile::tempdir().unwrap();
        let check =
            Check::Forbidden { literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "...".to_string()] };
        assert_eq!(run(&check, dir.path()), Outcome::Holds);
    }

    #[test]
    fn a_forbidden_check_with_a_single_literal_holds() {
        // A list of one is an ordinary, if unusual, forbidden check - not a
        // reason to answer any differently than a longer list would.
        let dir = tempfile::tempdir().unwrap();
        let check = Check::Forbidden { literals: vec!["TODO".to_string()] };
        assert_eq!(run(&check, dir.path()), Outcome::Holds);
    }

    #[test]
    fn a_forbidden_check_holds_even_against_a_root_that_does_not_exist() {
        // THE DEFECT THIS PREVENTS: every other form's containment guard
        // (`escapes_root`) fails closed - CannotRun - the instant `root`
        // itself cannot be canonicalised (see that function's own doc
        // comment). If Forbidden's early return in `run` were ever removed
        // or bypassed, it would fall into that same guard and start
        // reporting CannotRun for a condition that has nothing to do with
        // any root at all. Proven here by pointing `root` at a directory
        // that was never created - every path-carrying form would answer
        // CannotRun against this same root; Forbidden must not.
        let outer = tempfile::tempdir().unwrap();
        let missing_root = outer.path().join("this-directory-was-never-created");
        assert!(!missing_root.exists(), "sanity: the root really does not exist");

        let check = Check::Forbidden { literals: vec!["TODO".to_string()] };
        assert_eq!(run(&check, &missing_root), Outcome::Holds);
    }
}
