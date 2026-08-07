//! The judge reentrancy guard (see `JUDGE-TRANSPORT.md`'s "Judge reentrancy"
//! section for the full design write-up).
//!
//! Invoking the judge as a headless `claude` CLI call starts a REAL Claude
//! Code session. That child session may load the very hooks THOR itself
//! installs - so without a guard, THOR's own Stop hook would spawn a judge,
//! whose child session's own hooks would spawn ANOTHER judge, unbounded, each
//! level spawning processes (and, with a real hosted model configured, each
//! burning real tokens). Even short of full recursion, an unguarded child
//! session firing SessionStart/PreToolUse would inject facts into the
//! judge's own classification context and record delivery events as if they
//! had actually been served to the owner - corrupting the store's own usage
//! statistics.
//!
//! Both defects are closed BY CONSTRUCTION, never by a configuration
//! convention the owner has to remember to set (this project's own standing
//! rule: behavior that only a convention protects is a design defect):
//!
//! - `THOR_JUDGE_DEPTH` is an ordinary environment variable - the one
//!   mechanism the OS itself guarantees is inherited by an entire child
//!   process tree, on every platform this workspace targets. It carries NO
//!   private data, just a small non-negative integer.
//! - `judge::run_judge_command` marks the ONE child it spawns (the
//!   configured judge command, e.g. `claude`) one level deeper than this
//!   process' own depth (`mark_child`, called exactly once, in exactly that
//!   one place). Every process that command goes on to start - the `claude`
//!   binary itself, any hook IT fires (including another `serve hook`
//!   invocation), any MCP server it registers - inherits that same
//!   environment variable, whether or not each of those children
//!   individually knows anything about THOR at all.
//! - `serve/src/bin/serve.rs`'s `cmd_hook` (the ONLY entry point Claude Code
//!   itself ever invokes, via the hooks THOR installs) checks this mark
//!   FIRST, before the store is opened or any surface runs: present at all
//!   (depth >= 1) means "this process is a descendant of a judge call", and
//!   the correct behavior is to do NOTHING - the same shape as every other
//!   fail-open path in this hook (exit 0, never speak, never write).
//! - `judge::run_judge_command` ALSO checks the mark on ITSELF, before
//!   spawning anything, as a second, independent line of defense: if this
//!   process is somehow already running under the mark (a future caller of
//!   `run_judge`/`run_judge_command` that is not `cmd_hook`, a bug in the
//!   first guard, anything at all), it refuses to spawn and returns `None` -
//!   which every existing caller already treats as "no verdict", the same
//!   fail-open fallback path as a judge command that cannot be spawned or a
//!   run that times out. It never blocks; a nested judge call is refused
//!   explicitly, not silently guessed at.
//!
//! A depth COUNTER, not a boolean, costs nothing here (one more environment
//! variable write, one more integer parse) and buys real observability: "how
//! deep are we" is a number, not a yes/no, and any second level - should the
//! first guard ever be bypassed - is refused for the same explicit reason
//! ("already marked, depth > 0"), at whatever depth it actually occurs at,
//! rather than the guard collapsing to one indistinguishable flag.
//!
//! Deliberately split into pure logic (`depth_from_env_value`,
//! `is_reentrant_at`, `child_depth_value`) and thin I/O wrappers
//! (`current_depth`, `is_reentrant`, `mark_child`) - same shape as
//! `judge::parse_judge_output` vs. `judge::run_judge_command` - so the actual
//! decision rule is unit-tested without any process spawning, and without
//! ever mutating this test binary's own shared process environment (`std::
//! env::set_var` is process-wide and races across parallel test threads;
//! nothing here does that).

use std::process::Command;

/// The mark's name. Present at all, and parsing to an integer >= 1, means
/// "this process is a descendant of a judge invocation". ABSENT (the
/// ordinary case - every real hook Claude Code fires directly, with no judge
/// anywhere in its ancestry) reads as depth 0, invisible to normal operation
/// by construction: a process that never sets this variable can never
/// accidentally look reentrant.
///
/// Never set this by hand, never add it to a real session's own environment,
/// `settings.json`, or shell profile, and never write it anywhere except
/// `mark_child` below. It is written EXACTLY ONCE per judge call, by
/// `judge::run_judge_command`, on the ONE child process it spawns (the
/// configured judge command), for that call alone.
pub const JUDGE_DEPTH_ENV: &str = "THOR_JUDGE_DEPTH";

/// Parse a raw `THOR_JUDGE_DEPTH` value (as `std::env::var` would hand it
/// over) into a depth. Missing, empty, whitespace-only, negative, or
/// non-numeric all parse to 0 - fail-open in the "not reentrant" direction
/// for THIS function alone; the actual refusal decision is `is_reentrant_at`,
/// which only ever needs "zero vs. more than zero", so a malformed mark can
/// never itself cause a false BLOCK - at worst a malformed mark is read as
/// "not marked", which degrades to ordinary (non-reentrant) behavior, never
/// the other way around.
pub fn depth_from_env_value(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// Whether a process running at `depth` is a descendant of a judge
/// invocation. `true` for depth 1, 2, or any deeper value: nesting is
/// refused at every level, not just the first.
pub fn is_reentrant_at(depth: u32) -> bool {
    depth > 0
}

/// The value to write into the CHILD's environment when marking it one level
/// deeper than `current_depth` (this process' own depth).
pub fn child_depth_value(current_depth: u32) -> String {
    (current_depth + 1).to_string()
}

/// This process' own depth, read from its real environment.
pub fn current_depth() -> u32 {
    depth_from_env_value(std::env::var(JUDGE_DEPTH_ENV).ok().as_deref())
}

/// Whether THIS process is a descendant of a judge invocation - the one
/// question both guards actually gate on (`cmd_hook` in `serve/src/bin/
/// serve.rs`, and `judge::run_judge_command`).
pub fn is_reentrant() -> bool {
    is_reentrant_at(current_depth())
}

/// Mark `command` (the CHILD about to be spawned) as one level deeper than
/// this process' own depth - the one and only place `THOR_JUDGE_DEPTH` is
/// ever written. Called exactly once, by `judge::run_judge_command`, on the
/// configured judge command itself; every process that command goes on to
/// start inherits the marked environment automatically (the OS's own
/// child-process inheritance, not something this function has to arrange
/// any further).
pub fn mark_child(command: &mut Command) {
    command.env(JUDGE_DEPTH_ENV, child_depth_value(current_depth()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE DEFECT THIS PREVENTS: an ordinary hook invocation - no
    /// `THOR_JUDGE_DEPTH` anywhere in its environment at all - is ever read
    /// as reentrant. This must be the invisible, default case: absent parses
    /// to depth 0, and depth 0 is not reentrant.
    #[test]
    fn absent_is_depth_zero_and_not_reentrant() {
        assert_eq!(depth_from_env_value(None), 0);
        assert!(!is_reentrant_at(depth_from_env_value(None)));
    }

    /// A present, well-formed depth is read back exactly, at any level - "how
    /// deep are we" is observable, not collapsed to a single boolean.
    #[test]
    fn a_present_depth_is_read_back_exactly() {
        assert_eq!(depth_from_env_value(Some("1")), 1);
        assert_eq!(depth_from_env_value(Some("2")), 2);
        assert_eq!(depth_from_env_value(Some("7")), 7);
    }

    /// Malformed values (empty, whitespace, negative, non-numeric) all fail
    /// toward depth 0 - never toward some arbitrary "assume the worst"
    /// depth, and never a panic. This is deliberately the opposite failure
    /// direction from the marker/rulebook fail-open doctrine elsewhere in
    /// this crate (where an unreadable input never BLOCKS): here an
    /// unreadable MARK never REFUSES, since the whole point of the mark is
    /// to be trustworthy when THIS code itself writes it, never something a
    /// hostile or corrupted environment could spoof into a false negative
    /// that skips the guard - see `is_reentrant_at`'s own doc comment.
    #[test]
    fn a_malformed_value_yields_depth_zero_not_a_panic() {
        for bad in ["", "   ", "-1", "not-a-number", "1.5", "one"] {
            assert_eq!(depth_from_env_value(Some(bad)), 0, "expected depth 0 for {bad:?}");
        }
    }

    /// Any depth of 1 or more is reentrant - not just exactly 1. A future
    /// second level (should the first guard ever be bypassed) is refused
    /// explicitly at whatever depth it actually reaches, never silently
    /// let through because only depth == 1 was checked.
    #[test]
    fn any_depth_at_or_above_one_is_reentrant() {
        assert!(!is_reentrant_at(0));
        assert!(is_reentrant_at(1));
        assert!(is_reentrant_at(2));
        assert!(is_reentrant_at(100));
    }

    /// The value written for a child is always exactly one more than the
    /// current process' own depth - not a fixed "1" regardless of how deep
    /// the current process already is, so a (hypothetical, already-refused)
    /// deeper nesting would still be observable as a real number if it ever
    /// happened.
    #[test]
    fn child_depth_is_always_one_more_than_the_current_depth() {
        assert_eq!(child_depth_value(0), "1");
        assert_eq!(child_depth_value(1), "2");
        assert_eq!(child_depth_value(5), "6");
    }
}
