//! The Absent-check guard: turns a fact's machine-runnable `Check::Absent`
//! (and its set form, `Check::AbsentAll` - see `model::item::Check`,
//! `model::check::run`) from something merely rendered as context into an
//! actual stop, for a `Write` or `Edit` tool call whose target file carries
//! one.
//!
//! This module carries a SECOND, independent prohibition too: a LOCATION
//! guard (`find_location_violation`), for a live Rule/Orientation that names
//! a path or directory as somewhere nothing may ever be written, whatever
//! the content - see this file's own "location" section below for the
//! three-condition rule that makes an item count as one. The two
//! prohibitions never share matching logic (see the "Matching" paragraph
//! below for why), but they DO share one thing on purpose: the marker file
//! (see the "At most one block" paragraph below), and the caller's own
//! ordering, which prefers a location block over a content one whenever
//! both would fire - see `serve/src/bin/serve.rs`'s `absent_guard_block`.
//!
//! Governing doctrine the CONTENT guard exists to prove:
//! - Only a rule that carries a runnable check may block. Prose alone (a
//!   `falsifier`, an item with no `check` at all) never blocks anything
//!   here, and neither does a check of the `PathExists`/`Contains` form -
//!   see `first_violation`'s, `first_dir_violation`'s and
//!   `first_command_violation`'s own filters, which all go through the
//!   shared `absent_literals` helper and so consider
//!   `Check::Absent` and its set form `Check::AbsentAll` only, ignoring
//!   every other rule entirely (a `PathExists` check is exactly what the
//!   SEPARATE location guard below requires instead - see
//!   `location_anchor`). `AbsentAll` blocks as soon as ANY one of its
//!   literals is present - see `absent_literals`'s own doc comment for how
//!   the two forms share one loop instead of `AbsentAll` getting a second,
//!   parallel implementation.
//! - A rule may only block if its currency is provable AT THAT INSTANT: the
//!   check's own anchored path must resolve on disk right now
//!   (`anchor_is_current`), or there is no block, no warning, nothing. The
//!   location guard, and this guard's own `Dir`-anchored half
//!   (`first_dir_violation`), reuse this SAME helper for their own currency
//!   proof - never a second one.
//! - Any error, anywhere, is silence: an unreadable marker file, a store
//!   read failure, a payload missing the field this guard needs, a check
//!   whose path escapes its root - every one of these allows, never blocks
//!   and never warns. This guard never emits a permission decision of
//!   "allow" either - only a real block, or nothing at all, mirroring every
//!   other guard's own decision shape in this crate (see `capture`'s own
//!   doc comment). The location guard follows the identical stance: no
//!   matching anchor, a stale one, or an unresolvable root all fall through
//!   to "nothing to say", never a block.
//!
//! Governing doctrine the LOCATION guard exists to prove (see
//! `location_anchor`'s own doc comment for the full three-condition rule):
//! a `Check::PathExists` is what proves the item is still CURRENT - the
//! protected path really is still there - and `severity: Irreversible` is
//! what turns that into an actual PROHIBITION rather than a mere
//! description. Neither one alone is enough: without the check, severity
//! alone would block on a claim nobody ever re-verified; without the
//! severity requirement, every ordinary rule that happens to describe a
//! file or directory that still exists (a `PathExists` check proves nothing
//! more than that) would block all editing of it, which is absurd - it
//! would make `PathExists` radioactive for every rule not meant to be a
//! hard stop. A third tie, easy to miss: the check's own path must name the
//! EXACT SAME location as the item's own anchor, or a currency proof for
//! one path could stand in for a block on an entirely different one.
//!
//! Matching for the CONTENT guard splits in two, by anchor shape, because
//! `rank::select` structurally cannot serve a `Dir`-bound item at all: its
//! own `target_matches` requires the input's target KIND to equal the
//! item's (`rank::target_matches`), and the file being touched is only ever
//! offered to that path as a `Path` target (`ServeInput::add_file` - the
//! ONLY place `serve/src/bin/serve.rs` adds a `Dir` target at all is the
//! SEPARATE `location_input` built for the location guard below). So a
//! `Dir` binding is filtered out long before `find_violation` ever sees it,
//! regardless of any path logic in this file - this is what
//! `find_dir_content_violation` exists to close.
//!
//! A `Path`-anchored rule (`find_violation`) reuses the EXISTING surface,
//! never a second definition: the caller (`serve/src/bin/serve.rs`) finds
//! the live items anchored to the file being touched exactly the way
//! surface 2 already does (`live::candidates_for` + `rank::select`), and
//! hands the ranked result to `find_violation` here - this half of the
//! module never re-derives which items apply to a file, only what to do
//! once it already knows.
//!
//! A `Dir`-anchored rule (`find_dir_content_violation`) instead takes the
//! SAME approach the location guard already does (see the "Matching for the
//! LOCATION guard" paragraph below): its own `Path`+`Dir`-kind-narrowed
//! candidate pool, fetched via `live::candidates_for` exactly like the
//! location guard's own - in practice the SAME pool a caller already holds
//! as `location_candidates` for that guard, never a second store read - and
//! `path_is_or_contains`, the SAME function `find_location_violation` itself
//! uses, never a second containment implementation, to decide whether the
//! file being touched sits inside the directory a rule names. The two
//! content functions never overlap and neither changes the other: this
//! file's own `dir_binding` only ever extracts a `Dir` binding, so a
//! `Path`-anchored item is invisible to `find_dir_content_violation`
//! exactly as an `Always`- or `Moment`-bound one would be, and
//! `find_violation` itself is untouched by any of this - a `Path`-anchored
//! rule still matches exactly as it did before this half of the module
//! existed.
//!
//! The LOCATION guard (`find_location_violation`) deliberately does NOT
//! reuse `rank::select`: that function's own `target_matches` is an
//! approximate, human-facing comparison (equal once normalised, OR equal by
//! last path segment - built for "did you mean this file/command"), not a
//! filesystem containment test. Reusing it here would let a `Dir` binding
//! named, say, "src" block a write anywhere under ANY directory named "src"
//! in the whole repository - a false positive with real consequences for a
//! guard whose entire job is an irreversible-severity hard stop. So this
//! half of the module gets its own candidate pool instead (the caller fetches
//! it via `live::candidates_for` with a `Path` AND a `Dir` placeholder
//! target - kind-only narrowing, see `live::candidates_for`'s own doc
//! comment for why the placeholder VALUE never matters) and decides
//! containment itself, via `path_is_or_contains`'s own exact, resolved-path
//! comparison.
//!
//! THE COMMAND guard (`find_command_violation`) is a THIRD anchor shape for
//! the same CONTENT prohibition above - nothing here is new doctrine, only a
//! new place the same two rules ("only a runnable Absent/AbsentAll check
//! blocks", "currency proved at that instant") apply: a live Rule/
//! Orientation bound to a `Command` target, carrying a still-current
//! `Check::Absent`/`Check::AbsentAll`, blocks a Bash-style tool call whose
//! own command string carries one of the check's forbidden literals. This is
//! the owner's own worked example: his typography rule cannot reach a commit
//! MESSAGE any other way, because that text travels inside `git commit -m
//! ...`, never through a file write the other two CONTENT arms could already
//! see.
//!
//! Matching (`command_anchor_matches`) is its OWN comparison, never
//! `rank::target_matches` - for the SAME underlying reason the LOCATION guard
//! just above already refuses that function, but a different symptom:
//! `target_matches` decides whole-string-or-last-path-segment equality,
//! built to answer "is this the same file or command AS A WHOLE" - a real,
//! argument-bearing Bash invocation is essentially never equal, as a whole
//! string, to a short anchor like "git commit", and (having no `/`) gets no
//! help from last-segment matching either, so reusing it here would mean
//! this arm almost never fires at all. `command_anchor_matches` instead
//! checks a WHITESPACE-TOKENIZED PREFIX, case-insensitively: "git commit"
//! matches "git commit -m ..." and "git commit --amend" (every real way of
//! running that subcommand) while still refusing "git push" (second word
//! differs) and "git commit-graph verify" (`commit-graph` is one whole word,
//! not `commit` followed by more text - why this compares WORDS, never
//! `command.contains(anchor)`). PREFIX, deliberately, never "anywhere in the
//! command": `echo "remember to git commit later"` must never match - that
//! command's own first word is "echo", not "git", and a prefix check is what
//! tells RUNNING a subcommand apart from merely mentioning it in passing
//! (inside an echo, a comment, a search pattern). This is the guard's main
//! defence against its own worst false-positive shape: the forbidden literal
//! CAN genuinely, innocently appear in an unrelated command for a good
//! reason, and this guard cannot read intent, only text - a narrow anchor (a
//! full subcommand, never a bare "git") is most of what keeps that residual
//! risk small, not this function.
//!
//! Currency is UNCHANGED: the check's own path resolves through the exact
//! same `anchor_is_current` every other arm already uses, via the SAME
//! `absent_literals` filter `first_violation`/`first_dir_violation` share -
//! completely independent of the `Command` binding's own value, mirroring
//! `dir_binding`'s own currency independence from its `Dir` binding (see its
//! own doc comment).
//!
//! The once-per-session marker for this arm lives in its OWN sidecar file
//! (`default_command_marker_path`), never the shared (session, file) space
//! below: that space is keyed on the TOUCHED FILE, a stable identity the
//! caller already holds before any matching ever runs. A raw command string
//! has no such identity - two commands that trip the SAME rule rarely share
//! the same text at all (a commit message differs on every retry by design).
//!
//! That observation used to be the reason this arm keyed on the matched ANCHOR
//! ("git commit") ALONE, and stood aside once per session per anchor. Both
//! halves are gone as of 2026-08-08. Nothing is suppressed any more: a repeat
//! is refused too, and the key decides only whether the wording says so. Which
//! is exactly why the key can now carry the command's own fingerprint beside
//! the anchor - the objection above was about suppression, and there is none
//! left to get wrong. The anchor is still read off the WINNING candidate after
//! matching, because this guard cannot know its key up front the way the file
//! arm can, which is handed the file path by the caller before any matching
//! runs. The
//! staleness ledger needs no new sidecar, by contrast: `StaleRecord`/
//! `StaleFinding` are already keyed by item id regardless of what "file"
//! happens to name, so `stale_in_command` folds into the SAME
//! `absent-guard-stale.json` sidecar the other two arms already write.
//!
//! At most one block per session per file: the SAME KIND of marker-file
//! mechanism Lane C's capture guard already uses for its own once-per-session
//! bookkeeping (a small JSON sidecar beside the store, session-keyed,
//! read-modify-write, fail-open on any error - see
//! `capture::default_marker_path`/`capture::read_markers`/
//! `capture::flag_marker_text`), reused here for THIS guard's own (session,
//! file) key space rather than C1's (session -> single flagged prompt) one:
//! the two mechanisms track different things, so they get their own sidecar
//! file, exactly like the Response Guard's rulebook and the capture guard's
//! rulebook already sit in two separate files beside the store rather than
//! sharing one. The content and location prohibitions share THIS ONE space
//! (never two): whichever of them blocks first marks the file for the
//! session, and the other must never fire on it afterward either.
//!
//! A SECOND, independent sidecar (`default_stale_path`/`StaleRecord`, see
//! this file's own "staleness sidecar" section near the bottom) records the
//! OTHER half of this module's own opening doctrine above: a rule that
//! matched the file being written, carried the one check kind its guard
//! would use to block, and could not prove its own currency right now is
//! never just silently allowed to pass - that silence is itself recorded.
//! This shares NOTHING with the once-per-session block marker above: it is
//! keyed by item id, never by session, grows every time the condition is met
//! rather than firing once per session, and - critically - is never read by
//! anything that decides whether to block. `stale_in_content`/
//! `stale_in_dir_content`/`stale_in_location` do the finding (one scan per
//! guard-and-anchor-shape, run independently of whatever `find_violation`/
//! `find_dir_content_violation`/`find_location_violation` themselves decide,
//! and never influencing it); `record_stale_text` does the folding.

use crate::live::LiveItem;
use crate::rank::RankedItem;
use model::item::{Binding, Check, Item, Severity, TargetKind};
use model::normalize::normalize_target;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// How many characters of surrounding context to quote on each side of the
/// offending fragment - "a little", not the whole file: enough for a reader
/// to see exactly where the literal sits without reproducing the file.
const CONTEXT_CHARS: usize = 20;

// ------------------------------------------------------------ marker file

/// Where the once-per-session-per-file marker lives - the same convention
/// every sidecar file in this crate follows (see
/// `capture::default_marker_path`): beside the store, never inside a project
/// directory a session could plant one in.
pub fn default_marker_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("absent-guard-blocked.json")
}

/// Parse the marker file: session id -> the set of file paths already
/// blocked once this session. Malformed JSON (or a file that never existed)
/// yields an empty map - fail-open, the same stance every marker file in
/// this workspace takes (see `capture::read_markers`).
pub fn read_blocked(text: &str) -> HashMap<String, HashSet<String>> {
    serde_json::from_str(text).unwrap_or_default()
}

/// Has this session already been blocked once for this exact file path?
/// The key one blocked ATTEMPT is remembered under: the file (or command
/// anchor) plus a fingerprint of the text that call was carrying.
///
/// THE DEFECT THIS CLOSES. The key used to be the file alone, so the first
/// block in a session disarmed this guard for that file entirely: every later
/// write to it passed unexamined, including one carrying a different and
/// genuinely forbidden change, and including under an Irreversible rule. The
/// allow path writes no event either, so nothing measured it. "The gate
/// refuses" was true for exactly one attempt.
///
/// Why not simply drop the marker. It is loop protection, and that need is
/// real: an agent that retries the IDENTICAL write after a block would be
/// blocked forever, and a gate that cannot be satisfied is a gate people
/// route around. Fingerprinting the carried text keeps precisely that
/// escape - and on 2026-08-08 even that stopped being an escape: a repeat is
/// now refused too, and the marker only decides whether the refusal says it
/// is a repeat (see [`escalated`]). A DIFFERENT attempt is a fresh decision
/// and gets its own plain verdict. Version 1 reached the same answer for a
/// sibling problem: its session dedup key carries a fingerprint of the
/// content for exactly this reason.
///
/// FNV-1a, not a cryptographic hash: this key lives in a session-scoped
/// sidecar and never crosses a trust boundary, so collision resistance buys
/// nothing here and a dependency would cost something.
/// The whole of what a call is attempting, as one string, for
/// [`attempt_key`] to fingerprint.
///
/// NOT [`proposed_content`], and the difference matters on an `Edit`. That
/// function returns `new_string` alone, because for "does this write
/// introduce a forbidden literal" the replacement text is exactly the right
/// question. But two edits that replace DIFFERENT text with the SAME
/// replacement are two different attempts, and keying both under one
/// fingerprint would call the second one a repeat of the first. Since a
/// repeat now escalates rather than passes, that mistake costs a wrong
/// message rather than a missed refusal - which is why this is a correction
/// and not an emergency, and it is still a thing the gate should not say.
pub fn attempt_text(tool_name: &str, tool_input: Option<&Value>) -> Option<String> {
    match tool_name {
        "Write" => tool_input?.get("content")?.as_str().map(str::to_string),
        "Edit" => {
            let input = tool_input?;
            let old = input.get("old_string")?.as_str()?;
            let new = input.get("new_string")?.as_str()?;
            // A NUL separator, so ("ab", "c") and ("a", "bc") never collide.
            Some(format!("{old}\u{0}{new}"))
        }
        _ => None,
    }
}

pub fn attempt_key(target: &str, carried: Option<&str>) -> String {
    let Some(text) = carried else {
        // No readable payload: fall back to the old behaviour rather than
        // inventing a key, so an unknown tool shape can never accidentally
        // re-arm the guard on every call.
        return target.to_string();
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{target}#{hash:016x}")
}

/// The wording a repeat of an ALREADY refused attempt gets.
///
/// WHY THIS EXISTS AT ALL. Until 2026-08-08 a repeat did not get wording, it
/// got through: the marker returned before any rule was read, so the second
/// send of a refused write landed. The fear behind that was a retry loop -
/// an agent resending the same bytes forever against a gate that says no
/// forever. The fear was reasonable and the cure was worse than the disease,
/// because the one moment a gate has to hold is the moment somebody insists.
///
/// So a repeat is still refused, and the only thing the marker changes now is
/// what the refusal SAYS. That is also the loop answer: the text differs from
/// the first refusal, so a caller that reads its input at all has new
/// information, and the new information is "stop resending this one".
pub fn escalated(reason: &str) -> String {
    // "already been refused", not "the SECOND time": this fires on the third
    // attempt and every one after it too, and the file arm is not the only
    // caller - the command arm shares it, where "write" would be wrong. Both
    // reviews flagged the old wording as cosmetically false.
    format!(
        "{reason}

This exact attempt has ALREADY been refused in this session, and resending it will be refused again. Change what it carries, or ask the user whether the rule itself is wrong - and if it is, correct the rule (revise or retract it, with a reason) rather than working around it."
    )
}

pub fn already_blocked(markers: &HashMap<String, HashSet<String>>, session_id: &str, file_path: &str) -> bool {
    markers.get(session_id).is_some_and(|files| files.contains(file_path))
}

/// Fold a new (session, file) block into whatever the marker file already
/// held - pure, mirrors `capture::flag_marker_text`'s own shape exactly: the
/// caller does the actual read/write, this only computes the new contents
/// from the old ones. `None` only when the result cannot be serialised at
/// all, which never happens for this shape in practice.
pub fn mark_blocked_text(existing_text: Option<&str>, session_id: &str, file_path: &str) -> Option<String> {
    let mut markers = existing_text.map(read_blocked).unwrap_or_default();
    markers.entry(session_id.to_string()).or_default().insert(file_path.to_string());
    serde_json::to_string(&markers).ok()
}

// --------------------------------------------------------- payload reading

/// The text this call CARRIES, read from the payload's own tool-specific
/// shape - never a guessed or unified field name. `Write`'s own input
/// carries it as `content`; `Edit`'s carries the replacement half of the
/// edit as `new_string`. Any other tool name, or a payload missing the
/// field or carrying a non-string value there, yields `None` - never a
/// guess, never a panic.
///
/// THIS IS NOT THE FILE'S CONTENT AFTER THE CALL, and the distinction is
/// load-bearing. For the three arms that ask whether a forbidden literal
/// APPEARS, the carried fragment is exactly the right thing to search:
/// anything this call inserts is in it, and nothing else can be. For the
/// one arm that asks whether a required literal REMAINS, it is the wrong
/// thing entirely - see [`content_after_write`], which exists because this
/// function's doc comment used to claim it answered both.
pub fn proposed_content<'a>(tool_name: &str, tool_input: Option<&'a Value>) -> Option<&'a str> {
    let field = match tool_name {
        "Write" => "content",
        "Edit" => "new_string",
        _ => return None,
    };
    tool_input?.get(field)?.as_str()
}

/// What the file will actually HOLD after this call, as opposed to the
/// fragment the call carries ([`proposed_content`]).
///
/// THE DEFECT THIS CLOSES, caught in the act (2026-08-07). The "must
/// remain" arm below was fed `proposed_content`, so on an `Edit` it asked
/// whether the REPLACEMENT FRAGMENT contained the guarded literal. A
/// two-word correction to a comment in a guarded file was refused, because
/// a two-word fragment does not contain the guarded literal and never
/// could. Every small edit to every guarded file was refused the same way,
/// and the only ways out were to rewrite the whole file or to stop using
/// the guard. A guard that makes ordinary editing impossible is a guard
/// someone switches off, which costs more than the defect it prevents.
///
/// A `Write` carries the whole file, so its own `content` already is the
/// answer. An `Edit` is resolved against the file on disk, applying the
/// same replacement the tool would. Any failure - a tool this has no notion
/// of, a missing or non-string field, no root to resolve against, an
/// unreadable file, an `old_string` that is not in it - yields `None`, and
/// the caller then declines to decide rather than deciding wrongly. That is
/// the same "any error, anywhere, is silence" stance the rest of this
/// module takes: a guard that cannot see what would remain must not claim
/// something is about to disappear.
pub fn content_after_write(
    tool_name: &str,
    tool_input: Option<&Value>,
    root: Option<&Path>,
    file_path: &str,
) -> Option<String> {
    let input = tool_input?;
    match tool_name {
        "Write" => Some(input.get("content")?.as_str()?.to_string()),
        "Edit" => {
            let old = input.get("old_string")?.as_str()?;
            let new = input.get("new_string")?.as_str()?;
            let on_disk = std::fs::read_to_string(root?.join(file_path)).ok()?;
            if !on_disk.contains(old) {
                // The edit would not apply, so nothing here describes the
                // file that will exist. Say nothing rather than guess.
                return None;
            }
            let replace_all = input.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
            Some(if replace_all { on_disk.replace(old, new) } else { on_disk.replacen(old, new, 1) })
        }
        _ => None,
    }
}

/// The command about to run, read the SAME way `serve/src/bin/serve.rs`
/// already reads it when building a `ServeInput` for the informational
/// surface (`tool_input.get("command")`) - never a second, guessed field
/// name; that call site now calls this function too, so there is exactly one
/// definition of "how to read a command off a payload", the same R6 stance
/// `model::normalize`'s own doc comment names for target identity. Unlike
/// `proposed_content` above, this is NOT gated on the tool's own name: the
/// existing call site it mirrors applies to whichever tool call happens to
/// carry a "command" field (Claude Code's own Bash tool is the realistic
/// case, but nothing here hard-codes that name), and this guard must see the
/// exact same commands that call site does. A payload missing the field, or
/// carrying a non-string value there, yields `None` - never a guess, never a
/// panic, mirroring `proposed_content`'s own stance exactly.
pub fn proposed_command(tool_input: Option<&Value>) -> Option<&str> {
    tool_input?.get("command")?.as_str()
}

// -------------------------------------------------------------- currency

/// Whether `check_path` (a `Check::Absent`'s own relative path) resolves on
/// disk right now, relative to `root` - CURRENCY, proved at the instant this
/// runs, never assumed. Reuses `model::check::run`'s own root-escape-guarded
/// resolver via a synthetic `PathExists` check on the same path, rather than
/// re-implementing path resolution here (see `model::check`'s own doc
/// comment: it already carries this guard, tested against symlink/junction
/// escapes on Windows). Never reads the file's content for this - existence
/// only. A thin boolean wrapper over `anchor_staleness` below - the ONE place
/// this synthetic check actually runs - since a block decision only ever
/// needs "current or not", never which of the two negative outcomes it was.
fn anchor_is_current(check_path: &str, root: &Path) -> bool {
    anchor_staleness(check_path, root).is_none()
}

/// The same synthetic `Check::PathExists` currency proof `anchor_is_current`
/// above is built on (never a second one), but reporting WHICH negative
/// outcome it was instead of collapsing straight to a bare `false`: `None`
/// means the anchor IS current (`model::check::Outcome::Holds`) - nothing to
/// record; `Some` carries the distinction a block decision has no use for (it
/// does not care WHY currency failed, only that it did) but the staleness
/// sidecar does - see `StaleOutcome`'s own doc comment, in this file's
/// "staleness sidecar" section near the bottom.
fn anchor_staleness(check_path: &str, root: &Path) -> Option<StaleOutcome> {
    let existence = Check::PathExists { path: check_path.to_string() };
    match model::check::run(&existence, root) {
        model::check::Outcome::Holds => None,
        model::check::Outcome::Fails => Some(StaleOutcome::Failed),
        model::check::Outcome::CannotRun => Some(StaleOutcome::CouldNotRun),
    }
}

// ------------------------------------------------------------- the block

/// A little surrounding context around the first occurrence of `literal` in
/// `content` - never a bare, out-of-context literal, and never sliced across
/// a char boundary (the literal may be multi-byte UTF-8, e.g. an em dash).
/// Falls back to `literal` alone in the should-be-unreachable case that it
/// cannot be found at all - never panics either way.
fn quote_with_context(content: &str, literal: &str) -> String {
    let Some(byte_pos) = content.find(literal) else {
        return literal.to_string();
    };
    let match_char_idx = content[..byte_pos].chars().count();
    let literal_char_len = literal.chars().count();
    let chars: Vec<char> = content.chars().collect();
    let start = match_char_idx.saturating_sub(CONTEXT_CHARS);
    let end = (match_char_idx + literal_char_len + CONTEXT_CHARS).min(chars.len());
    chars[start..end].iter().collect()
}

/// The block message, in the exact required order: a marker making clear it
/// comes from THOR, the rule's id, the rule's own text as the reason, then
/// the offending fragment quoted with a little surrounding context.
/// The id of the rule a refusal names, read back out of the message.
///
/// Every refusal this module builds opens `[THOR] rule {id} ` - all four
/// builders, enforced by `every_refusal_message_names_a_readable_rule_id`.
/// Reading it back beats threading an id through five arms that all return a
/// String today, and it keeps the coupling inside the one module that owns
/// the wording. `None` for anything that is not one of our own messages,
/// which is what an escalated repeat still is not (it keeps the original
/// first line, so it still parses).
pub fn rule_id_of(message: &str) -> Option<&str> {
    message.strip_prefix("[THOR] rule ")?.split_whitespace().next()
}

fn block_message(rule_id: &str, rule_text: &str, content: &str, literal: &str) -> String {
    let quote = quote_with_context(content, literal);
    format!(
        "[THOR] rule {rule_id} forbids this text (\"{rule_text}\"); found in what is about to be written: \"{quote}\""
    )
}

/// The `(path, literals)` a `Check::Absent` or `Check::AbsentAll` forbids,
/// or `None` for every other form this guard never blocks on (`PathExists`,
/// `Contains`, `Forbidden` - see this module's own top-of-file doc comment).
/// `Absent` reads as a one-literal slice, so every caller below has exactly
/// ONE loop to write over "every literal this check forbids", regardless of
/// which of the two forms it is actually looking at - `AbsentAll` is never a
/// second, parallel implementation of the matching logic `Absent` already
/// has, only a longer list fed through the identical loop.
///
/// `Forbidden` (added to `model::item::Check` alongside this guard's own
/// Command-anchor work, by a separate change) names no file at all - it has
/// no `path` to hand this guard's own currency proof (`anchor_is_current`),
/// which every one of this guard's three arms requires unconditionally. It
/// is ignored here on exactly the same terms as `PathExists`/`Contains`
/// already are: a check form this guard was never asked to act on, never a
/// silent guess at what a currency-less check ought to mean for a block
/// decision built entirely around "the check's own path must resolve right
/// now".
fn absent_literals(check: &Check) -> Option<(&str, &[String])> {
    match check {
        Check::Absent { path, literal } => Some((path.as_str(), std::slice::from_ref(literal))),
        Check::AbsentAll { path, literals } => Some((path.as_str(), literals.as_slice())),
        Check::PathExists { .. } | Check::Contains { .. } | Check::Forbidden { .. } => None,
    }
}

/// The literals a check forbids in a COMMAND, and the file (if any) whose
/// existence still has to prove the rule current.
///
/// WHY THIS IS NOT JUST `absent_literals`. A command prohibition - never run
/// `gh repo edit --visibility public`, never run `thor import` - is not about
/// a file, and demanding one was a category error that kept the whole class
/// out of enforcement. `Absent`/`AbsentAll` name a file and must still prove
/// it is there; `Forbidden` names none, so its reach is its BINDING, and a
/// `Command` binding says exactly which command it reaches. There is nothing
/// to prove current: the rule is stale only when its owner changes their mind,
/// which is what the falsifier is for, never a check.
///
/// This is the same reasoning `find_forbidden_violation` already applies to an
/// `Always` binding on the file side. One form, two bindings, no new check
/// kind, and no file invented to satisfy a formality - which is precisely the
/// thing this project's own doctrine warns against.
fn command_literals(check: &Check) -> Option<(Option<&str>, &[String])> {
    match check {
        Check::Forbidden { literals } => Some((None, literals.as_slice())),
        _ => absent_literals(check).map(|(path, literals)| (Some(path), literals)),
    }
}

/// The first literal, in the check's own declared order, that is actually
/// present in `content` - never simply the first literal in the list
/// regardless of whether IT matched. For a single-literal `Absent` check
/// this is that one literal or nothing; for a set-carrying `AbsentAll` check
/// this is whichever literal in the set actually triggered the block, so the
/// reported reason quotes the one that matched, not an arbitrary member of
/// the set.
fn first_present_literal<'a>(literals: &'a [String], content: &str) -> Option<&'a str> {
    literals.iter().find(|literal| content.contains(literal.as_str())).map(String::as_str)
}

/// The first live, ranked candidate (worst-first, exactly as `rank::select`
/// already orders them) whose check is a still-current `Check::Absent`/
/// `Check::AbsentAll` (via `absent_literals`) and at least one of whose
/// literals appears in `content` - the PROPOSED content, never the file on
/// disk. `root` is where a check's own relative path is resolved for the
/// currency proof; `None` (no cwd could be resolved at all) means currency
/// can never be proven for anything, so nothing here can ever match.
fn first_violation<'a>(
    ranked: &'a [RankedItem],
    file_path: &str,
    content: &str,
    root: Option<&Path>,
) -> Option<(&'a RankedItem, &'a str)> {
    let root = root?;
    for candidate in ranked {
        let Some(check) = &candidate.item.check else { continue };
        let Some((check_path, literals)) = absent_literals(check) else { continue };
        if !anchor_is_current(check_path, root) {
            continue;
        }
        // THE FALSE BLOCK THIS PREVENTS. An item reaches this pool either
        // because a TARGET matched the file being written, or because a
        // MOMENT matched - and `ServeInput::add_file` derives moments from the
        // path, so a filename containing the word "prod" is enough. For a
        // target-matched item the binding already scopes the prohibition to
        // this file. For a moment-matched one, nothing did: the check's path
        // was treated as a currency proof only, so a rule whose check names
        // one file would refuse a write to any OTHER file whose name happened
        // to trigger the same moment. Measured 2026-08-08: `prod_data` fired
        // 47 times in one session on filenames alone.
        //
        // So a moment-only match must additionally land inside the file the
        // check itself names. An `Absent` check names a file; refusing a write
        // to a different file is not the rule doing its job.
        // "Reached through a target" is not "carries a target". A first pass
        // asked the second question and got the first one wrong: an item bound
        // [Moment(ProdData), Target{Dir,"config"}] reaches this pool ONLY
        // through the moment, because a Dir or Command binding cannot match
        // the Path target `add_file` builds - yet it carries a target, so the
        // scope test was skipped and the defect survived for exactly the
        // shapes most likely to have it.
        //
        // `RankedItem` does not record WHY it matched, so the honest question
        // is asked directly: does a PATH binding on this item actually name
        // the file being written? That is the only binding kind a file touch
        // can match, and `rank::select` used the same comparison to let this
        // item in.
        let reached_by_path = candidate.item.bindings.iter().any(|b| {
            matches!(
                b,
                model::item::Binding::Target { kind: model::item::TargetKind::Path, value }
                    if model::normalize::target_matches(
                        model::item::TargetKind::Path,
                        value,
                        model::item::TargetKind::Path,
                        file_path,
                    )
            )
        });
        if !reached_by_path && !path_is_or_contains(&root.join(check_path), &root.join(file_path)) {
            continue;
        }
        if let Some(literal) = first_present_literal(literals, content) {
            return Some((candidate, literal));
        }
    }
    None
}

/// The full block reason for the first item `first_violation` finds among
/// `ranked` - `None` when nothing currently violates (no candidate carries
/// an Absent check, none is current, or the literal is simply not present in
/// `content`). `ranked` is expected to already be the caller's own
/// `rank::select` output for the file being touched - this function never
/// re-derives which items apply, only what to do once it already knows.
pub fn find_violation(ranked: &[RankedItem], file_path: &str, content: &str, root: Option<&Path>) -> Option<String> {
    let (item, literal) = first_violation(ranked, file_path, content, root)?;
    Some(block_message(&item.id, &item.item.text, content, literal))
}

// ------------------------------------------------ text that must REMAIN

/// The `(path, literal)` a `Check::Contains` requires to stay present, or
/// `None` for every other form. This is the mirror image of
/// `absent_literals`: those forms name what may not appear, this one names
/// what may not disappear.
fn required_literal(check: &Check) -> Option<(&str, &str)> {
    match check {
        Check::Contains { path, literal } => Some((path.as_str(), literal.as_str())),
        Check::PathExists { .. } | Check::Absent { .. } | Check::AbsentAll { .. } | Check::Forbidden { .. } => None,
    }
}

/// The block reason for the first live item among `items` whose
/// `Check::Contains` names the very file being written and whose required
/// literal is about to vanish from it.
///
/// TWO THINGS THIS SERVES, and they are one mechanism, not two: a document
/// that mirrors this memory must not silently lose what it mirrors, and a
/// fact about code must be able to prove itself by naming something that has
/// to still be there. A CLAUDE.md losing an agreement and a source file
/// losing the function a fact describes are the same event.
///
/// IDENTITY IS THE CHECK'S OWN PATH, not the item's binding, and that is
/// deliberate: "this literal must remain in this file" names the file
/// intrinsically. The three arms above match by binding and use the check
/// path only to prove currency; here the check path IS the subject, so
/// matching on anything else would let a rule anchored to one file guard a
/// different one.
///
/// IT MUST HOLD RIGHT NOW OR THERE IS NOTHING TO PRESERVE. If the literal is
/// already missing from the file on disk, this blocks nothing: the mirror is
/// already broken, and refusing the write would only trap whoever is trying
/// to fix it. That case belongs to the staleness ledger, not to a block.
pub fn find_missing_required(
    items: &[LiveItem],
    file_path: &str,
    content: &str,
    root: Option<&Path>,
) -> Option<String> {
    let root = root?;
    let file_abs = normalize_target(&root.join(file_path).to_string_lossy());
    for candidate in items {
        if !candidate.item.kind.can_fire() {
            continue;
        }
        let Some(check) = &candidate.item.check else { continue };
        let Some((check_path, literal)) = required_literal(check) else { continue };
        if normalize_target(&root.join(check_path).to_string_lossy()) != file_abs {
            continue;
        }
        if model::check::run(check, root) != model::check::Outcome::Holds {
            continue;
        }
        if !content.contains(literal) {
            return Some(format!(
                "[THOR] rule {} requires this to stay in {} (\"{}\"); it is missing from what is about to be written: \"{}\"",
                candidate.id, check_path, candidate.item.text, literal
            ));
        }
    }
    None
}

// ------------------------------------------------- the self-contained form

/// The literals a `Check::Forbidden` carries, or `None` for every other
/// form. Deliberately NOT folded into `absent_literals`: that one returns a
/// path because its callers must first prove the anchor is still there, and
/// this one has no path to return because there is nothing to prove. A rule
/// forbidding a character names nothing outside itself, so no file can move
/// out from under it and no value can change behind its back.
fn forbidden_literals(check: &Check) -> Option<&[String]> {
    match check {
        Check::Forbidden { literals } => Some(literals.as_slice()),
        Check::PathExists { .. } | Check::Contains { .. } | Check::Absent { .. } | Check::AbsentAll { .. } => None,
    }
}

/// The block reason for the first live item among `items` carrying a
/// self-contained `Forbidden` check one of whose literals appears in the
/// proposed `content` - `None` when none does.
///
/// Takes no `root` and runs no currency proof, and that is the point rather
/// than an omission: there is nothing to resolve. Every other arm here must
/// first show its anchor still exists, because a rule naming a file or a
/// value can rot. This form names neither, so making it point at something
/// would be a formality - and a formality that quietly narrows a universal
/// rule down to whichever single directory was picked to satisfy it.
///
/// `items` is expected to be the ALWAYS-bound pool
/// (`live::always_candidates`). That is what gives this form its reach: the
/// check carries no anchor, so the BINDING carries the scope, and `Always`
/// means every write to every file. A `Forbidden` check on an item bound to
/// a path or a directory is not reached here and blocks nothing - deciding
/// containment is what the two anchored arms are for, and repeating it here
/// would put one decision in two places.
pub fn find_forbidden_violation(items: &[LiveItem], content: &str) -> Option<String> {
    for candidate in items {
        if !candidate.item.kind.can_fire() {
            continue;
        }
        // The Always requirement lives HERE, not in the caller's choice of
        // pool, and that is the whole point of this line. A `Forbidden` check
        // carries no path, so its reach is its BINDING: Always means every
        // write to every file, and anything narrower means the rule was
        // written about a place this arm cannot honour.
        //
        // It used to rest on the caller passing `live::always_candidates`.
        // That function falls back to `live_items` - the entire store - on
        // three separate paths (a stale binding projection, or either
        // projected read failing), so in that state a target-bound
        // `Forbidden` check would refuse EVERY write, while `doctor` filed it
        // under "blocks nothing at all". A guarantee that holds only while an
        // unrelated projection is warm is not a guarantee; found by a review
        // reading this function instead of the comment that described it.
        if !candidate.item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always)) {
            continue;
        }
        let Some(check) = &candidate.item.check else { continue };
        let Some(literals) = forbidden_literals(check) else { continue };
        if let Some(literal) = first_present_literal(literals, content) {
            return Some(block_message(&candidate.id, &candidate.item.text, content, literal));
        }
    }
    None
}

// ----------------------------------------------------------- the location

/// Whether `item` is a LOCATION PROHIBITION, and if so, the one path or
/// directory it protects - never a bare boolean, so the caller has the
/// exact string in hand to resolve for both currency and containment,
/// rather than re-deriving it from the item's bindings a second time.
///
/// ALL THREE of the following must hold together (see this module's own doc
/// comment for the reasoning behind each; repeated here, briefly, because
/// none of the three is safe to skip on its own):
///   - a live `Rule`/`Orientation` (`Kind::can_fire`) - checked explicitly
///     here even though the write gate already refuses a `check` on any
///     other kind (`gate::declare`, ground 12): the SAME second lock on the
///     same door `rank::select`'s own `eligible` filter keeps for the
///     content guard above, needed here because this guard's own candidate
///     fetch does not run through `rank::select` (see this module's own doc
///     comment for why) and so never gets that filter for free.
///   - `severity: Irreversible` - a plain `!=` against `Option<Severity>`,
///     so a lighter severity (`Costly`, `HouseStyle`) or a missing one is
///     refused exactly alike, never treated as a lesser form of yes. This is
///     what turns a rule into a PROHIBITION rather than a description:
///     without it, every ordinary rule that happens to carry a `PathExists`
///     check (which proves only "the thing I am describing is still there",
///     nothing about whether writing to it is forbidden) would block all
///     editing of the file or directory it merely describes - which is
///     absurd, and would make `PathExists` radioactive for every rule that
///     is not meant to be a hard stop.
///   - a `Check::PathExists` whose OWN path is the exact same string as one
///     of the item's `Target(Path|Dir)` binding values - "for that same
///     path", compared by plain string equality, never
///     `model::normalize::normalize_target` (unlike the anchor-vs-file
///     comparison in `path_is_or_contains` below, both halves of THIS
///     comparison are authored by the same person at the same time - see
///     this module's own test fixtures, which reuse one literal for both -
///     so an exact match is the realistic, least-surprising bar). This is
///     what proves the rule is CURRENT: `PathExists` observed to hold, right
///     now, against the very path the binding anchors to - never a currency
///     proof for one path standing in for a block on a different one.
fn location_anchor(item: &Item) -> Option<&str> {
    if !item.kind.can_fire() {
        return None;
    }
    if item.severity != Some(Severity::Irreversible) {
        return None;
    }
    let Some(Check::PathExists { path: check_path }) = &item.check else { return None };
    let anchored = item.bindings.iter().any(|b| {
        matches!(b, Binding::Target { kind: TargetKind::Path | TargetKind::Dir, value } if value == check_path)
    });
    anchored.then_some(check_path.as_str())
}

/// Whether `file_abs` names exactly `anchor_abs`, or names something inside
/// it - requirement 1's two cases ("the file being written, or is a
/// directory containing it") collapse into this one comparison. Compared as
/// `model::normalize::normalize_target` strings (unifies `\`/`/` and case -
/// the ONE place this workspace normalises a path for comparison), never as
/// raw `Path` components, so a Windows separator or case difference between
/// the two can never produce a false miss. The trailing `/` appended to the
/// anchor before the prefix check is what keeps "foo/ba" from wrongly
/// containing "foo/bar/x.txt" - a bare `starts_with` on the two strings
/// alone would match that, silently widening "contains" into "shares a text
/// prefix with".
fn path_is_or_contains(anchor_abs: &Path, file_abs: &Path) -> bool {
    let anchor = normalize_target(&anchor_abs.to_string_lossy());
    let file = normalize_target(&file_abs.to_string_lossy());
    file == anchor || file.starts_with(&format!("{anchor}/"))
}

/// The first live location prohibition among `items` whose anchor is
/// CURRENT (reuses `anchor_is_current` above, never a second currency
/// check) and whose protected path is or contains `file_path`. `items` is
/// expected to already be the caller's own candidate fetch - see this
/// function's caller in `serve/src/bin/serve.rs`: `live::candidates_for`
/// with BOTH a `Path` and a `Dir` placeholder target, never `rank::select`
/// (see this module's own doc comment for why `rank::select`'s matching is
/// the wrong tool here). Worst-first ordering is moot among matches (every
/// one already carries the same `Irreversible` severity by definition), so
/// this simply takes the first match in whatever deterministic order the
/// caller's own read produced (`live::live_items`/`live::candidates_for` are
/// both id-ordered - never hashmap order). `root` is where BOTH the check's
/// own currency and the containment comparison are resolved from; `None`
/// means neither can ever be proven, so nothing here can ever match -
/// mirrors `first_violation`'s own `root: Option<&Path>`.
fn first_location_violation<'a>(
    items: &'a [LiveItem],
    file_path: &str,
    root: Option<&Path>,
) -> Option<(&'a LiveItem, &'a str)> {
    let root = root?;
    let file_abs = root.join(file_path);
    for candidate in items {
        let Some(anchor) = location_anchor(&candidate.item) else { continue };
        if !anchor_is_current(anchor, root) {
            continue;
        }
        let anchor_abs = root.join(anchor);
        if path_is_or_contains(&anchor_abs, &file_abs) {
            return Some((candidate, anchor));
        }
    }
    None
}

/// The location block message: the same shape as `block_message` above (the
/// THOR marker first, then the rule id, then the rule's own text as the
/// reason) but ending in a plain sentence naming the protected path and the
/// refused file, in place of a quoted fragment - a location prohibition has
/// no offending text to quote, only two paths.
fn location_block_message(rule_id: &str, rule_text: &str, protected_path: &str, refused_file: &str) -> String {
    format!(
        "[THOR] rule {rule_id} forbids writing here (\"{rule_text}\"); {protected_path} is a protected location and {refused_file} may not be written."
    )
}

/// The full block reason for the first location prohibition
/// `first_location_violation` finds among `items` - `None` when nothing
/// currently prohibits `file_path` (no candidate anchors here at all, none
/// is current, or none is/contains this file). See `location_anchor`'s own
/// doc comment for the three-condition rule an item must satisfy before it
/// is even considered.
pub fn find_location_violation(items: &[LiveItem], file_path: &str, root: Option<&Path>) -> Option<String> {
    let (item, anchor) = first_location_violation(items, file_path, root)?;
    Some(location_block_message(&item.id, &item.item.text, anchor, file_path))
}

// ------------------------------------------------- the block (dir-anchored)
//
// The CONTENT guard's other half (see this module's own top-of-file doc
// comment for why `find_violation` above can never reach a `Dir`-bound
// item): a rule whose `Target` binding is a directory, whose currency is
// still provable, and whose forbidden literal appears in the content about
// to be written to a file somewhere inside that directory. Structured as a
// close mirror of "the location" section just above - same candidate-pool
// shape (`&[LiveItem]`, never `rank::select`'s output), same reuse of
// `path_is_or_contains` for containment - but deciding a CONTENT question
// (`Check::Absent` + a literal found in the proposed write), never a
// location one, so it carries none of `location_anchor`'s extra
// severity/check-path ties.

/// The one `Dir`-kind target binding's own value on `item`, if any - never a
/// bare boolean, so the caller has the exact string in hand to resolve for
/// both currency and containment, mirroring `location_anchor`'s own shape.
/// Unlike `location_anchor`, this carries NO severity requirement and NO
/// check-path-equals-binding-value tie: a content prohibition is not a
/// location prohibition (see this module's own "Governing doctrine the
/// CONTENT guard exists to prove" above - only a runnable `Check::Absent`/
/// `Check::AbsentAll` decides whether an item may ever block here, exactly
/// the bar `first_violation` already holds a `Path`-anchored item to), and currency,
/// decided separately in `first_dir_violation` below from the check's OWN
/// `path` field, has never been required to name the same string as the
/// binding that got an item this far - the same looseness `first_violation`
/// already has for its `Path`-anchored items, whose currency anchor is
/// likewise the check's own path, never the binding value.
///
/// The `Kind::can_fire` gate is repeated here for the SAME reason
/// `location_anchor` repeats it rather than trusting `rank::select`'s own
/// `eligible` filter: this function's own candidate pool never goes through
/// `rank::select` either (see this module's own top-of-file doc comment), so
/// nothing else would stop a `Report`/`Chunk`/`Lookup` from reaching this
/// far.
fn dir_binding(item: &Item) -> Option<&str> {
    if !item.kind.can_fire() {
        return None;
    }
    item.bindings.iter().find_map(|b| match b {
        Binding::Target { kind: TargetKind::Dir, value } => Some(value.as_str()),
        _ => None,
    })
}

/// The first live candidate among `items` whose `Dir` binding is current
/// (`anchor_is_current`, the SAME helper `first_violation` and
/// `first_location_violation` already use) and CONTAINS `file_path` (via
/// `path_is_or_contains` - the SAME comparison `first_location_violation`
/// uses, never a second containment implementation), and whose forbidden
/// literal appears in `content` - the PROPOSED content, never the file on
/// disk, mirroring `first_violation`'s own final step exactly.
///
/// `items` is expected to be a `Path`+`Dir`-kind-narrowed candidate pool
/// fetched the same way the location guard's own is (`live::candidates_for`
/// with a `Path` and a `Dir` placeholder target - see this module's own
/// top-of-file doc comment) - in practice the SAME pool the caller already
/// fetches for `first_location_violation`, never a second store read. A
/// `Path`-bound candidate in that pool is harmless here: `dir_binding`
/// returns `None` for it, so it is skipped, exactly like an `Always`- or
/// `Moment`-bound one would be.
fn first_dir_violation<'a>(
    items: &'a [LiveItem],
    file_path: &str,
    content: &str,
    root: Option<&Path>,
) -> Option<(&'a LiveItem, &'a str)> {
    let root = root?;
    let file_abs = root.join(file_path);
    for candidate in items {
        let Some(dir_value) = dir_binding(&candidate.item) else { continue };
        let Some(check) = &candidate.item.check else { continue };
        let Some((check_path, literals)) = absent_literals(check) else { continue };
        if !anchor_is_current(check_path, root) {
            continue;
        }
        let dir_abs = root.join(dir_value);
        if !path_is_or_contains(&dir_abs, &file_abs) {
            continue;
        }
        if let Some(literal) = first_present_literal(literals, content) {
            return Some((candidate, literal));
        }
    }
    None
}

/// The full block reason for the first `Dir`-anchored content violation
/// `first_dir_violation` finds among `items` - `None` when nothing currently
/// violates (no candidate carries a `Dir` binding, none is current, none
/// contains `file_path`, or the literal is simply not present in `content`).
/// Reuses `block_message` - the SAME message shape `find_violation` already
/// produces, since a reader has no reason to see a differently-shaped reason
/// depending on which anchor shape happened to catch it.
///
/// The `Path`-anchored counterpart, `find_violation` above, is left
/// completely untouched by this function's existence: a caller wanting full
/// CONTENT-guard coverage calls both and takes whichever fires (mirrors
/// `serve/src/bin/serve.rs`'s own `absent_guard_block`, which already
/// combines `find_location_violation` and `find_violation` the identical
/// way, via `Option::or_else`) - never merged into one function, so the
/// pre-existing `Path` behaviour stays provably exactly what it was.
pub fn find_dir_content_violation(items: &[LiveItem], file_path: &str, content: &str, root: Option<&Path>) -> Option<String> {
    let (item, literal) = first_dir_violation(items, file_path, content, root)?;
    Some(block_message(&item.id, &item.item.text, content, literal))
}

// -------------------------------------------- the block (command-anchored)
//
// The CONTENT guard's THIRD anchor shape (see this module's own top-of-file
// doc comment, "THE COMMAND guard", for the full doctrine): a rule whose
// `Target` binding is a `Command`, whose currency is still provable, and
// whose forbidden literal appears in the COMMAND STRING itself, never a
// file's proposed content - there is none here. Structured as a close
// mirror of "the block (dir-anchored)" section just above (same
// `&[LiveItem]` candidate-pool shape, never `rank::select`'s output, same
// reuse of `absent_literals`/`anchor_is_current`/`first_present_literal`),
// but matching by `command_anchor_matches` instead of `path_is_or_contains` -
// a command has no filesystem containment to test.

/// The one `Command`-kind target binding's own value on `item`, if any -
/// mirrors `dir_binding`'s own shape exactly, including its reasoning: this
/// guard's own candidate pool never goes through `rank::select` either (see
/// this module's own top-of-file doc comment), so nothing else would stop a
/// `Report`/`Chunk`/`Lookup` from reaching this far without the
/// `Kind::can_fire` gate repeated here.
fn command_binding(item: &Item) -> Option<&str> {
    if !item.kind.can_fire() {
        return None;
    }
    item.bindings.iter().find_map(|b| match b {
        Binding::Target { kind: TargetKind::Command, value } => Some(value.as_str()),
        _ => None,
    })
}

/// EVERY command anchor an item carries, not just the first.
///
/// `command_binding` above returns one, which was fine while nothing declared
/// two - and silently wrong the moment something did. Measured on the owner's
/// own store: a rule given two spellings of the same command only ever
/// considered the first, so the spelling actually typed went unguarded while
/// the second binding read as protection that was not there.
fn command_bindings(item: &Item) -> Vec<&str> {
    if !item.kind.can_fire() {
        return Vec::new();
    }
    item.bindings
        .iter()
        .filter_map(|b| match b {
            Binding::Target { kind: TargetKind::Command, value } => Some(value.as_str()),
            _ => None,
        })
        .collect()
}

/// The command as the RULE means it, with the wrapping that shell syntax adds
/// taken off: a leading `sudo`, leading `VAR=value` assignments, the directory
/// part of the executable, and a `.exe` suffix.
///
/// WHY THIS IS NOT A WIDENING. Running a program under `sudo`, by absolute
/// path, or by its Windows spelling is the same act; only the typing differs.
/// A matcher that treats them as different commands is not being strict, it is
/// mistaking shell syntax for intent - and it fails in the one direction that
/// matters, because the dangerous form is usually the one typed with a full
/// path or under sudo. The prohibition still lives entirely in the literal;
/// this only decides which rules get to look.
///
/// KNOWN GAP, stated rather than left to be rediscovered: a command run
/// THROUGH another one, such as a container exec, still does not match an
/// anchor naming the inner command, because the anchor would have to match in
/// the middle rather than at the front. Making an anchor match anywhere is a
/// real widening, and this project has twice measured what unvalidated
/// widening costs, so it needs a blind hold-out first, not an argument.
fn strip_invocation_wrapper(command: &str) -> String {
    let mut words: Vec<&str> = command.split_whitespace().collect();
    while let Some(first) = words.first() {
        if *first == "sudo" || (first.contains('=') && !first.starts_with('-')) {
            words.remove(0);
        } else {
            break;
        }
    }
    if let Some(first) = words.first_mut() {
        let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
        *first = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(base);
    }
    words.join(" ")
}

/// Whether `anchor` (a rule's own declared `Command` target value, e.g. "git
/// commit") names the command actually about to run (`command`, the raw
/// string read off the tool payload) - deliberately its OWN comparison,
/// never `rank::target_matches` (see this module's own top-of-file doc
/// comment, "THE COMMAND guard", for why that function's whole-string-or-
/// last-path-segment equality was built to answer a different question and
/// would in practice almost never match a real, argument-bearing
/// invocation).
///
/// A WHITESPACE-TOKENIZED PREFIX, case-insensitively: holds when `command`'s
/// own leading words are EXACTLY `anchor`'s words, in order. "git commit"
/// then matches "git commit -m ..." and "git commit --amend" (every real way
/// of running that subcommand) while still refusing "git push" (the second
/// word differs) and "git commit-graph verify" (`commit-graph` is one whole
/// word, not the word `commit` followed by more text - the reason this
/// compares WORDS, never `command.contains(anchor)`). PREFIX, deliberately,
/// never "anywhere in the command": `echo "remember to git commit later"`
/// must never match an anchor of "git commit" - it only MENTIONS the
/// subcommand, it does not run it, and a prefix check is what tells the
/// difference (that command's own first word is "echo"). An empty anchor
/// never matches anything (mirrors `ServeInput::add_command`'s own refusal
/// to treat an empty string as a command at all, so this is defence in depth
/// rather than a reachable case for a live item - the write gate already
/// refuses an empty Target value).
fn command_anchor_matches(anchor: &str, command: &str) -> bool {
    let anchor_words: Vec<String> = anchor.split_whitespace().map(str::to_lowercase).collect();
    if anchor_words.is_empty() {
        return false;
    }
    let command_words: Vec<String> =
        command.split_whitespace().take(anchor_words.len()).map(str::to_lowercase).collect();
    command_words == anchor_words
}

/// The first candidate among `items` (in whatever deterministic order the
/// caller's own candidate fetch produced - `live::candidates_for`/
/// `live::live_items` are both id-ordered, never hashmap order, mirroring
/// `first_location_violation`'s own ordering note) whose `Command` binding
/// (`command_binding`) matches the command about to run
/// (`command_anchor_matches`), whose check is a still-current
/// `Check::Absent`/`Check::AbsentAll` (`absent_literals` + `anchor_is_current`
/// - the SAME two steps `first_violation`/`first_dir_violation` already use),
/// and at least one of whose literals is present in `command` ITSELF - never
/// a file's proposed content, there is none here. Returns the matched item,
/// its own anchor value (the caller needs this for the once-per-session
/// marker key - see this module's own top-of-file doc comment for why) and
/// the literal that matched.
fn first_command_violation<'a>(
    items: &'a [LiveItem],
    command: &str,
    root: Option<&Path>,
) -> Option<(&'a LiveItem, &'a str, &'a str)> {
    let root = root?;
    let normalised = strip_invocation_wrapper(command);
    for candidate in items {
        // EVERY anchor the item carries, and the command with its shell
        // wrapping taken off - see `command_bindings` and
        // `strip_invocation_wrapper` for what each of those two fixed.
        let Some(anchor) = command_bindings(&candidate.item)
            .into_iter()
            .find(|a| command_anchor_matches(a, command) || command_anchor_matches(a, &normalised))
        else {
            continue;
        };
        let Some(check) = &candidate.item.check else { continue };
        let Some((check_path, literals)) = command_literals(check) else { continue };
        // A path, when the check names one, still has to be there. A
        // `Forbidden` check names none and needs none: see `command_literals`.
        if let Some(check_path) = check_path {
            if !anchor_is_current(check_path, root) {
                continue;
            }
        }
        if let Some(literal) = first_present_literal(literals, command) {
            return Some((candidate, anchor, literal));
        }
    }
    None
}

/// The full block reason for the first command-anchored violation
/// `first_command_violation` finds among `items`, paired with the matched
/// item's own anchor value - `None` when nothing currently violates (no
/// candidate's `Command` binding matches `command`, none is current, or the
/// literal is simply not present in `command` itself). Reuses `block_message`
/// - the SAME message shape `find_violation`/`find_dir_content_violation`
/// already produce - quoted against the command string itself rather than a
/// file's proposed content. Returns the anchor ALONGSIDE the reason (unlike
/// its two siblings, which return only the reason) because the caller cannot
/// know the once-per-session marker key up front the way it can for a file
/// path - see this module's own top-of-file doc comment.
pub fn find_command_violation(items: &[LiveItem], command: &str, root: Option<&Path>) -> Option<(String, String)> {
    let (item, anchor, literal) = first_command_violation(items, command, root)?;
    Some((block_message(&item.id, &item.item.text, command, literal), anchor.to_string()))
}

/// Where the command guard's OWN once-per-session marker lives - never the
/// content/location guard's shared `default_marker_path` above: that space
/// is keyed by TOUCHED FILE, a stable identity known before any matching
/// runs; a command has no such identity of its own (see this module's own
/// top-of-file doc comment, "THE COMMAND guard", for the full reasoning), so
/// this guard gets its own sidecar - the same convention `default_stale_path`
/// below already follows for a differently-keyed space (beside the store,
/// never inside a project directory a session could plant one in).
pub fn default_command_marker_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("absent-guard-command-blocked.json")
}

// ----------------------------------------------------- staleness sidecar
//
// The second half of this module's own opening doctrine (see the top of
// this file): a rule that matched the file being written, carried the one
// check kind its guard would use to block, and could not prove its own
// currency right now is never just silently allowed to pass - that silence
// is itself recorded, in a THIRD sidecar file beside the store, completely
// separate from the once-per-session block marker above (never touched by
// anything below). This sidecar is a one-way ledger: nothing here ever
// reads it, and nothing here ever decides a block from it -
// `stale_in_content`/`stale_in_location` are called purely for their return
// value, which the caller (`serve/src/bin/serve.rs`) folds into the file
// with `record_stale_text` ALONGSIDE the real block decision
// (`find_violation`/`find_location_violation`), never in a way that can
// change it.

/// Why a rule's own currency proof came back negative, when it would
/// otherwise have mattered. Kept as two distinct values, never collapsed
/// into one "stale" bit, because they mean different things to whoever
/// triages this list afterward:
///   - `Failed` - the check actually RAN and decided false: the anchored
///     path is definitely not there any more, right now
///     (`model::check::Outcome::Fails`). The rule is provably out of date.
///   - `CouldNotRun` - the check could not even be attempted: an escaping
///     path, an empty one, or no resolvable root at all
///     (`model::check::Outcome::CannotRun`). The rule is not necessarily
///     wrong, but nothing here could prove it right either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleOutcome {
    Failed,
    CouldNotRun,
}

/// One item's own staleness record, keyed by item id in the sidecar (see
/// `read_stale`) - never written for an item whose currency proof HOLDS:
/// `stale_in_content`/`stale_in_location` only ever produce a `StaleFinding`
/// in the first place when it does not (see their own doc comments).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StaleRecord {
    pub outcome: StaleOutcome,
    /// The item's own `check` field, in a short readable form (`{:?}` on
    /// `model::item::Check` - already one line, already names the exact
    /// path and literal, so this never needs a bespoke formatter).
    pub check: String,
    pub file: String,
    pub count: u64,
    /// The event store's own seq at the moment this record was last folded
    /// in by `record_stale_text` (that function's own `now_seq` parameter -
    /// the caller's current max seq, saturating at 0 for an empty store, the
    /// exact computation `capture_flag` already uses for its unrelated
    /// `seq_at_flag`). Never the check's own currency - only "how far the
    /// log had gotten" at record time, so a later reader can tell whether
    /// the item has since been revised or retracted (see
    /// `serve::stale_guard::item_settled`, which is built on exactly that
    /// question). `#[serde(default)]` so a sidecar written before this field
    /// existed still parses: a missing value reads as 0 ("since the
    /// beginning of the log"), which can only ever make an old entry MORE
    /// likely to already read as settled, never less - the same fail-open
    /// bias every sidecar in this crate already takes on a field it cannot
    /// recover.
    #[serde(default)]
    pub seq_at_record: i64,
}

/// Where the staleness sidecar lives - the same convention every sidecar
/// file in this crate follows (see `default_marker_path` above,
/// `capture::default_marker_path`): beside the store, never inside a project
/// directory a session could plant one in.
pub fn default_stale_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("absent-guard-stale.json")
}

/// Parse the staleness sidecar: item id -> its own `StaleRecord`. Malformed
/// JSON (or a file that never existed) yields an empty map - fail-open, the
/// same stance every sidecar in this workspace takes.
pub fn read_stale(text: &str) -> HashMap<String, StaleRecord> {
    serde_json::from_str(text).unwrap_or_default()
}

/// One matching-and-checked candidate that could not prove its own currency,
/// found during a single guard pass - owned, never borrowed, so the caller
/// can collect findings from BOTH scans below into one plain `Vec` and hand
/// them to `record_stale_text` together, in one read-modify-write, exactly
/// like every other sidecar in this crate.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleFinding {
    pub id: String,
    pub outcome: StaleOutcome,
    pub check: String,
    pub file: String,
}

/// Every candidate among `ranked` that matches the file being written (by
/// construction: `ranked` is the caller's own `rank::select` output for this
/// exact file - the SAME list `find_violation` itself is handed), carries
/// one of the two check kinds the CONTENT guard would ever use to block
/// (`Check::Absent`/`Check::AbsentAll`, via the shared `absent_literals`
/// filter `first_violation` itself applies, never a second definition of
/// "matches"), and could not prove its own currency right now. Never just
/// the first: unlike a block, which only ever needs the single worst-first
/// violation, observation has nothing to gain from stopping early - every
/// stale candidate in the same pass is equally worth recording. `root: None`
/// means currency can never be proven for anything (mirrors
/// `first_violation`'s own root handling), so nothing here can ever be
/// observed either - not because it is known to be fine, but because
/// nothing could even be tried.
pub fn stale_in_content(ranked: &[RankedItem], file_path: &str, root: Option<&Path>) -> Vec<StaleFinding> {
    let Some(root) = root else { return Vec::new() };
    let mut out = Vec::new();
    for candidate in ranked {
        let Some(check) = &candidate.item.check else { continue };
        let Some((check_path, _literals)) = absent_literals(check) else { continue };
        let Some(outcome) = anchor_staleness(check_path, root) else { continue };
        out.push(StaleFinding {
            id: candidate.id.clone(),
            outcome,
            check: format!("{check:?}"),
            file: file_path.to_string(),
        });
    }
    out
}

/// The location guard's own mirror of `stale_in_content` above, over `items`
/// (the caller's own `Path`+`Dir`-narrowed candidate pool - see
/// `bin/serve.rs`'s own comment on why that pool is fetched the way it is,
/// never through `rank::select`). "Matches the file being written" means
/// something different here than it does for the content guard above:
/// `items` is NOT already narrowed to this exact file (the placeholder
/// target's VALUE never mattered when it was fetched - see this module's own
/// top-of-file doc comment), so containment against `file_path` is decided
/// HERE, via the SAME `path_is_or_contains` comparison
/// `first_location_violation` itself uses - checked BEFORE currency,
/// deliberately the opposite order from that function: a location rule whose
/// protected directory was just deleted can no longer prove it is still
/// there, but its protected PATH (the text itself) still tells us whether
/// this particular write would have landed inside it. Skipping the
/// containment check here (the mistake this ordering exists to prevent)
/// would flag every Irreversible location rule in the whole store as stale
/// on every single write anywhere, which is not observation, it is noise.
pub fn stale_in_location(items: &[LiveItem], file_path: &str, root: Option<&Path>) -> Vec<StaleFinding> {
    let Some(root) = root else { return Vec::new() };
    let file_abs = root.join(file_path);
    let mut out = Vec::new();
    for candidate in items {
        let Some(anchor) = location_anchor(&candidate.item) else { continue };
        let anchor_abs = root.join(anchor);
        if !path_is_or_contains(&anchor_abs, &file_abs) {
            continue;
        }
        let Some(outcome) = anchor_staleness(anchor, root) else { continue };
        let Some(check) = &candidate.item.check else { continue };
        out.push(StaleFinding {
            id: candidate.id.clone(),
            outcome,
            check: format!("{check:?}"),
            file: file_path.to_string(),
        });
    }
    out
}

/// The `Dir`-anchored content guard's own mirror of `stale_in_content` above
/// (never `stale_in_location` - a `Dir`-anchored CONTENT rule is not a
/// location prohibition, so it carries none of `location_anchor`'s extra
/// severity/check-path ties; matching here is `dir_binding`, the SAME
/// extractor `first_dir_violation` itself uses), over `items` (the same
/// `Path`+`Dir`-narrowed pool `first_dir_violation` consumes - see that
/// function's own doc comment). Containment is checked BEFORE currency here,
/// the same deliberately-reversed order `stale_in_location` already uses and
/// for the identical reason (see its own doc comment): skipping it would
/// flag every `Dir`-anchored content rule in the whole store as stale on
/// every single write anywhere its own directory happens not to exist,
/// however unrelated the file actually being written - noise, not
/// observation.
pub fn stale_in_dir_content(items: &[LiveItem], file_path: &str, root: Option<&Path>) -> Vec<StaleFinding> {
    let Some(root) = root else { return Vec::new() };
    let file_abs = root.join(file_path);
    let mut out = Vec::new();
    for candidate in items {
        let Some(dir_value) = dir_binding(&candidate.item) else { continue };
        let dir_abs = root.join(dir_value);
        if !path_is_or_contains(&dir_abs, &file_abs) {
            continue;
        }
        let Some(check) = &candidate.item.check else { continue };
        let Some((check_path, _literals)) = absent_literals(check) else { continue };
        let Some(outcome) = anchor_staleness(check_path, root) else { continue };
        out.push(StaleFinding {
            id: candidate.id.clone(),
            outcome,
            check: format!("{check:?}"),
            file: file_path.to_string(),
        });
    }
    out
}

/// The command guard's own mirror of `stale_in_dir_content` above (matching
/// here is `command_binding`/`command_anchor_matches`, never `dir_binding`/
/// `path_is_or_contains` - a command has no filesystem containment to test).
/// Anchor-matching is checked BEFORE currency, the same deliberately-reversed
/// order `stale_in_location`/`stale_in_dir_content` already use and for the
/// identical reason (see their own doc comments): skipping it would flag
/// every Command-anchored rule in the whole store as stale on every single
/// command run, however unrelated - noise, not observation. `file` carries
/// the command string itself here (this struct's one field doing double duty
/// across every arm, exactly as it already does for the `Dir`-anchored arm
/// above) - folds into the SAME `absent-guard-stale.json` sidecar the other
/// two arms already write (via the caller's own `record_stale_text`):
/// `StaleRecord`/`StaleFinding` are keyed by item id, never by "file" vs
/// "command", so this needs no sidecar of its own the way the BLOCK marker
/// does (`default_command_marker_path` above).
pub fn stale_in_command(items: &[LiveItem], command: &str, root: Option<&Path>) -> Vec<StaleFinding> {
    let Some(root) = root else { return Vec::new() };
    let mut out = Vec::new();
    for candidate in items {
        let Some(anchor) = command_binding(&candidate.item) else { continue };
        if !command_anchor_matches(anchor, command) {
            continue;
        }
        let Some(check) = &candidate.item.check else { continue };
        let Some((check_path, _literals)) = absent_literals(check) else { continue };
        let Some(outcome) = anchor_staleness(check_path, root) else { continue };
        out.push(StaleFinding {
            id: candidate.id.clone(),
            outcome,
            check: format!("{check:?}"),
            file: command.to_string(),
        });
    }
    out
}

/// Fold every finding from THIS call's own scans into whatever the sidecar
/// already held - one read, one write, however many findings there are (see
/// `stale_in_content`/`stale_in_location`, whose combined output this is
/// meant to be handed straight through). Mirrors `mark_blocked_text`'s own
/// shape: the caller does the actual read/write, this only computes the new
/// contents from the old ones. Keyed by item id, one row per id -
/// re-recording an id already present REPLACES its outcome/check/file with
/// this occurrence's own (the most recent occurrence is the useful one to
/// show) and increments its count, rather than ever appending a duplicate
/// entry. `now_seq` (the caller's own current max seq - see
/// `StaleRecord::seq_at_record`'s own doc comment) is stamped onto every
/// record this call touches, inserted or updated alike, so it always
/// reflects the MOST RECENT occurrence's own position in the log, exactly
/// like `outcome`/`check`/`file` already do. `None` only when there is
/// nothing new to fold in at all (`findings` is empty), so a caller with
/// nothing to record never has to reason about "write back what I just
/// read, unchanged".
/// `scanned` is every id this pass actually looked at. An id that was looked at
/// and is NOT among the findings has recovered, and its record is dropped.
///
/// THE DEFECT THIS CLOSES, reported from a fresh session on 2026-08-10. This
/// function used to return early on an empty finding list and never removed
/// anything, so a proof that once came out false stayed on file forever. The
/// owner's own AGENTS.md rule was reported as broken for hours after the word
/// it forbids had already been taken out - the daily scan agreed it was fine,
/// and the sidecar went on saying otherwise. A guard that cries wolf about a
/// healthy rule is worse than one that says nothing: the next real finding
/// arrives to a reader who has learned to dismiss it.
///
/// Only ids in `scanned` are dropped. A record for an item this pass never
/// examined is left exactly as it was, because "not looked at" and "looked at
/// and healthy" are different answers and this file must not confuse them.

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn finding(id: &str) -> StaleFinding {
        StaleFinding {
            id: id.to_string(),
            outcome: StaleOutcome::Failed,
            check: "Absent { path: \"NOTES.md\", literal: \"TODO\" }".to_string(),
            file: "NOTES.md".to_string(),
        }
    }

    /// THE DEFECT, reported from a fresh session on 2026-08-10: a proof that
    /// once came out false was reported for hours after it had been fixed,
    /// because nothing ever took the record back out.
    #[test]
    fn a_proof_that_healed_is_dropped_when_the_scan_sees_it_pass() {
        let seen = vec!["r1".to_string()];
        let recorded = record_stale_text(None, &[finding("r1")], &seen, 5).unwrap();
        assert!(recorded.contains("r1"), "fixture sanity");

        let cleared = record_stale_text(Some(&recorded), &[], &seen, 9)
            .expect("a scan that clears something must write the file");
        assert!(!cleared.contains("r1"), "a healed proof must not stay on file: {cleared}");
    }

    /// "Not looked at" is not "healthy". A pass that never examined an item
    /// leaves its record alone, or a narrow scan would erase what a broad one
    /// found.
    #[test]
    fn an_item_this_pass_never_looked_at_keeps_its_record() {
        let recorded = record_stale_text(None, &[finding("r1")], &["r1".to_string()], 5).unwrap();
        let after = record_stale_text(Some(&recorded), &[], &["other".to_string()], 9);
        assert!(after.is_none(), "nothing to add and nothing to drop means nothing to write");
    }

    #[test]
    fn a_clean_scan_with_an_empty_file_still_writes_nothing() {
        assert!(record_stale_text(None, &[], &["r1".to_string()], 5).is_none());
    }
}

pub fn record_stale_text(
    existing_text: Option<&str>,
    findings: &[StaleFinding],
    scanned: &[String],
    now_seq: i64,
) -> Option<String> {
    let mut records = existing_text.map(read_stale).unwrap_or_default();
    let before = records.len();
    records.retain(|id, _| !scanned.contains(id) || findings.iter().any(|f| &f.id == id));
    let dropped = before - records.len();
    if findings.is_empty() && dropped == 0 {
        return None;
    }
    for finding in findings {
        match records.get_mut(&finding.id) {
            Some(existing) => {
                existing.outcome = finding.outcome;
                existing.check = finding.check.clone();
                existing.file = finding.file.clone();
                existing.count += 1;
                existing.seq_at_record = now_seq;
            }
            None => {
                records.insert(
                    finding.id.clone(),
                    StaleRecord {
                        outcome: finding.outcome,
                        check: finding.check.clone(),
                        file: finding.file.clone(),
                        count: 1,
                        seq_at_record: now_seq,
                    },
                );
            }
        }
    }
    serde_json::to_string(&records).ok()
}

#[cfg(test)]
mod tests {

    /// THE DEFECT THIS PREVENTS, and it went to the heart of what this system
    /// claims to be. The block marker used to be keyed on the file alone, so
    /// the first refusal in a session disarmed this guard for that file
    /// entirely: every later write to it passed unexamined, including one
    /// carrying a different and genuinely forbidden change, and including
    /// under an Irreversible rule. Nothing recorded the allow, so nothing
    /// measured it either.
    #[test]
    fn a_different_attempt_on_the_same_file_is_judged_again() {
        let first = attempt_key("src/config.rs", Some("a harmless line"));
        let second = attempt_key("src/config.rs", Some("a line carrying the forbidden literal"));
        assert_ne!(first, second, "a different write to the same file must get its own verdict");
    }

    /// The other half, and the reason the marker exists at all: an agent that
    /// retries the IDENTICAL write must not be blocked forever. A gate that
    /// cannot be satisfied is a gate people learn to route around.
    #[test]
    fn the_same_attempt_twice_still_passes_the_second_time() {
        let once = attempt_key("src/config.rs", Some("exactly the same text"));
        let twice = attempt_key("src/config.rs", Some("exactly the same text"));
        assert_eq!(once, twice, "an unchanged retry must keep its loop protection");
    }

    /// An unknown tool shape carries no readable text. Inventing a key from
    /// nothing would re-arm the guard on every single call and turn the loop
    /// protection off by accident, so it falls back to the old behaviour.
    #[test]
    fn an_unreadable_payload_falls_back_to_the_bare_target() {
        assert_eq!(attempt_key("src/config.rs", None), "src/config.rs");
    }

    use super::*;
    use model::item::{Binding, Item, Kind, Severity, TargetKind};

    fn item_with_check(id: &str, check_path: &str, literal: &str) -> RankedItem {
        RankedItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: keep this file clean"),
                bindings: vec![Binding::Target { kind: TargetKind::Path, value: check_path.to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check: Some(Check::Absent { path: check_path.to_string(), literal: literal.to_string() }),
            },
        }
    }

    /// The set-form counterpart to `item_with_check` above: a live Rule
    /// carrying a `Check::AbsentAll` forbidding every one of `literals`
    /// together, anchored to `check_path` via a `Path` target - same shape
    /// otherwise, so a test built from this differs from one built from
    /// `item_with_check` only in the check it carries.
    fn item_with_absent_all_check(id: &str, check_path: &str, literals: &[&str]) -> RankedItem {
        RankedItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: keep this file clean"),
                bindings: vec![Binding::Target { kind: TargetKind::Path, value: check_path.to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check: Some(Check::AbsentAll {
                    path: check_path.to_string(),
                    literals: literals.iter().map(|s| s.to_string()).collect(),
                }),
            },
        }
    }

    // --------------------------------------------------------------- block

    /// THE FALSE BLOCK THIS PREVENTS. An item reaches this pool either because
    /// a TARGET matched the file, or because a MOMENT matched - and moments are
    /// derived from the path, so a filename carrying the word "prod" is enough
    /// (`prod_data` fired 47 times in one session on filenames alone,
    /// 2026-08-08). The check's path was a currency proof only, so a rule whose
    /// check named ONE file would refuse a write to any OTHER file that
    /// happened to trigger the same moment.
    ///
    /// A moment-only match now has to land inside the file the check itself
    /// names. A target-matched item is unaffected: its binding already said
    /// this file.
    #[test]
    fn a_moment_bound_rule_does_not_refuse_a_write_to_a_file_its_check_never_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NOTES.md"), "# notes
").unwrap();
        std::fs::write(dir.path().join("OTHER.md"), "# other
").unwrap();

        // Absolute, exactly as the hook payload delivers a written path.
        let notes = dir.path().join("NOTES.md").to_string_lossy().to_string();
        let other = dir.path().join("OTHER.md").to_string_lossy().to_string();

        let mut moment_only = item_with_check("m1", "NOTES.md", "forbidden");
        moment_only.item.bindings = vec![Binding::Moment(intent::Action::ProdData)];
        let content = "this write carries the forbidden word";

        assert_eq!(
            find_violation(&[moment_only.clone()], &other, content, Some(dir.path())),
            None,
            "a rule whose check names NOTES.md must not refuse a write to OTHER.md"
        );
        assert!(
            find_violation(&[moment_only], &notes, content, Some(dir.path())).is_some(),
            "the file the check actually names must still be protected"
        );

        // THE MIXED-BINDING HOLE, which the first version of this test
        // cemented as intended behaviour. An item can carry a Target binding
        // AND reach this pool through a Moment - because a Dir or Command
        // target, or a Path target naming a DIFFERENT file, cannot match the
        // Path target a file touch builds. Asking "does it carry a target"
        // therefore let exactly those shapes skip the scope check. The question
        // now asked is whether a Path binding actually names THIS file.
        let anchored_elsewhere = item_with_check("t1", "NOTES.md", "forbidden");
        assert_eq!(
            find_violation(&[anchored_elsewhere], &other, content, Some(dir.path())),
            None,
            "a rule anchored at NOTES.md must not refuse a write to OTHER.md, however it got into the pool"
        );

        let mut moment_and_dir = item_with_check("t2", "NOTES.md", "forbidden");
        moment_and_dir.item.bindings = vec![
            Binding::Moment(intent::Action::ProdData),
            Binding::Target { kind: TargetKind::Dir, value: "config".to_string() },
        ];
        assert_eq!(
            find_violation(&[moment_and_dir], &other, content, Some(dir.path())),
            None,
            "carrying a Dir target does not scope a moment-reached rule to an unrelated file"
        );
    }

    /// THE DEFECT THIS PREVENTS: a write containing the forbidden literal
    /// into the anchored file goes through unblocked, or blocks with a
    /// message that cannot be traced back to which rule fired or where the
    /// text actually is.
    #[test]
    fn a_forbidden_literal_in_the_proposed_write_is_blocked_and_names_the_rule_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NOTES.md"), "# notes\n").unwrap();
        let ranked = vec![item_with_check("r1", "NOTES.md", "forbidden")];
        let content = "the quick brown fox jumps over the forbidden fence into the garden";

        let reason = find_violation(&ranked, "NOTES.md", content, Some(dir.path())).expect("must block");

        let marker_idx = reason.find("[THOR]").expect("must carry the THOR marker");
        let id_idx = reason.find("r1").expect("must name the rule id");
        let text_idx = reason.find("keep this file clean").expect("must carry the rule's own text as the reason");
        let quote_idx = reason.find("forbidden").expect("must quote the offending fragment");
        assert!(marker_idx < id_idx, "marker must come before the id: {reason}");
        assert!(id_idx < text_idx, "id must come before the rule's own text: {reason}");
        assert!(text_idx < quote_idx, "the rule's own text must come before the quoted fragment: {reason}");
        assert!(
            reason.contains("fence") || reason.contains("jumps"),
            "expected real surrounding context around the fragment, not just the bare literal: {reason}"
        );
    }

    /// THE DEFECT THIS PREVENTS: the guard blocks whatever file the caller
    /// happens to be writing, regardless of which file the rule is actually
    /// anchored to - proven here by running the SAME matching pipeline
    /// (`live::candidates_for` + `rank::select`) surface 2 itself uses, for
    /// a file the item is not bound to at all.
    #[test]
    fn a_forbidden_literal_written_to_a_different_file_is_not_blocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NOTES.md"), "# notes\n").unwrap();
        let mut db = thor_core::event_store::EventStore::in_memory().unwrap();
        let item = item_with_check("r1", "NOTES.md", "forbidden").item;
        model::store::declare(&mut db, "s", "l", "a", &item).unwrap();

        let mut input = crate::input::ServeInput::default();
        input.add_file("OTHER.md");
        let candidates = crate::live::candidates_for(&db, &input);
        let ranked = crate::rank::select(&candidates, &input);
        assert!(ranked.is_empty(), "fixture sanity: an unrelated file must not even match the binding");

        let content = "this text contains the forbidden word too";
        assert_eq!(find_violation(&ranked, "NOTES.md", content, Some(dir.path())), None);
    }

    /// THE DEFECT THIS PREVENTS: an item with prose describing a rule but no
    /// machine-runnable check ever blocks anything, however well its text
    /// happens to match what is being written - only a real `Check::Absent`
    /// may ever cause a block.
    #[test]
    fn an_item_with_prose_but_no_check_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NOTES.md"), "# notes\n").unwrap();
        let mut ranked_item = item_with_check("r1", "NOTES.md", "forbidden");
        ranked_item.item.check = None;

        let content = "this text contains the forbidden word";
        assert_eq!(find_violation(&[ranked_item], "NOTES.md", content, Some(dir.path())), None);
    }

    /// THE DEFECT THIS PREVENTS: a rule anchored to a file that has since
    /// been deleted or renamed still blocks, on the strength of a currency
    /// claim that is no longer provable.
    #[test]
    fn a_rule_whose_anchored_path_no_longer_exists_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        // NOTES.md is deliberately never created.
        let ranked = vec![item_with_check("r1", "NOTES.md", "forbidden")];
        let content = "this text contains the forbidden word";
        assert_eq!(find_violation(&ranked, "NOTES.md", content, Some(dir.path())), None);
    }

    /// The other half of the currency requirement: with no root at all to
    /// resolve against (the caller could not determine a cwd), currency can
    /// never be proven for anything, so nothing can ever block.
    #[test]
    fn no_resolvable_root_means_currency_can_never_be_proven_so_nothing_blocks() {
        let ranked = vec![item_with_check("r1", "NOTES.md", "forbidden")];
        let content = "this text contains the forbidden word";
        assert_eq!(find_violation(&ranked, "NOTES.md", content, None), None);
    }

    /// THE DEFECT THIS PREVENTS (step 2's own scope rule): a check of the
    /// `Contains` or `PathExists` form is silently treated like an `Absent`
    /// check and blocks anyway - even though the literal genuinely is
    /// present in the proposed content, only the `Absent`/`AbsentAll` forms
    /// may ever cause a block; `Contains`/`PathExists` are ignored entirely.
    #[test]
    fn a_check_of_another_form_never_blocks_even_if_it_would_otherwise_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NOTES.md"), "# notes\n").unwrap();
        let mut ranked_item = item_with_check("r1", "NOTES.md", "forbidden");
        ranked_item.item.check =
            Some(Check::Contains { path: "NOTES.md".to_string(), literal: "forbidden".to_string() });

        let content = "this text contains the forbidden word";
        assert_eq!(find_violation(&[ranked_item], "NOTES.md", content, Some(dir.path())), None);
    }

    /// Multi-byte safety: the quoted context must never panic by slicing a
    /// UTF-8 string on a non-char boundary, and the offending character
    /// itself (an em dash - this is the realistic scenario this guard was
    /// built for) must still appear in the final message.
    #[test]
    fn quoting_context_around_a_multi_byte_character_never_panics_on_a_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NOTES.md"), "# notes\n").unwrap();
        let ranked = vec![item_with_check("r1", "NOTES.md", "\u{2014}")];
        let content = format!("some notes here {} more notes right after it", '\u{2014}');

        let reason = find_violation(&ranked, "NOTES.md", &content, Some(dir.path())).unwrap();
        assert!(reason.contains('\u{2014}'), "{reason}");
    }

    // ------------------------------------------------- block, AbsentAll (set form)
    //
    // The set-form counterpart to the plain-`Absent` block tests above, over
    // `item_with_absent_all_check`: a set-carrying rule blocks as soon as ANY
    // one of its literals is present, and the reported reason quotes the
    // literal that actually matched - never the first one in the list
    // regardless of whether that one is the one present. Each test below
    // pins one position (first, middle, last) so a naive implementation that
    // only ever checked `literals[0]` (or only the last literal - the
    // copy-paste mistake `model::check::run`'s own `a_file_containing_the_
    // first_literal_fails_for_absent_all` test guards against on the model
    // side) would be caught here on the guard side too.

    /// THE DEFECT THIS PREVENTS: a set-carrying rule never blocks at all (the
    /// set form is silently treated like the four ignored check kinds), or it
    /// blocks but reports the wrong literal when the FIRST one in the list is
    /// the one actually present.
    #[test]
    fn a_set_carrying_rule_blocks_on_the_first_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), b"# style\n").unwrap();
        let ranked =
            vec![item_with_absent_all_check("r10", "STYLE.md", &["forbidden-one", "forbidden-two", "forbidden-three"])];
        let content = "this text carries forbidden-one right here";

        let reason = find_violation(&ranked, "STYLE.md", content, Some(dir.path())).expect("must block");
        assert!(reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-two"), "{reason}");
        assert!(!reason.contains("forbidden-three"), "{reason}");
    }

    /// The middle position: catches an implementation that only ever checks
    /// the first or the last literal and skips whatever sits between them.
    #[test]
    fn a_set_carrying_rule_blocks_on_a_middle_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), b"# style\n").unwrap();
        let ranked =
            vec![item_with_absent_all_check("r11", "STYLE.md", &["forbidden-one", "forbidden-two", "forbidden-three"])];
        let content = "this text carries forbidden-two right here";

        let reason = find_violation(&ranked, "STYLE.md", content, Some(dir.path())).expect("must block");
        assert!(reason.contains("forbidden-two"), "{reason}");
        assert!(!reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-three"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS: a naive implementation that only ever checks
    /// the FIRST literal in the list (a copy-paste of the plain `Absent` arm
    /// that forgot to loop over the rest) would miss a violation sitting in
    /// the last position.
    #[test]
    fn a_set_carrying_rule_blocks_on_the_last_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), b"# style\n").unwrap();
        let ranked =
            vec![item_with_absent_all_check("r12", "STYLE.md", &["forbidden-one", "forbidden-two", "forbidden-three"])];
        let content = "this text carries forbidden-three right here";

        let reason = find_violation(&ranked, "STYLE.md", content, Some(dir.path())).expect("must block");
        assert!(reason.contains("forbidden-three"), "{reason}");
        assert!(!reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-two"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS: a set-carrying rule blocks even though NONE
    /// of its literals is actually present - the mirror image of the three
    /// tests above, proving this never over-fires.
    #[test]
    fn a_set_carrying_rule_whose_literals_are_all_absent_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), b"# style\n").unwrap();
        let ranked =
            vec![item_with_absent_all_check("r13", "STYLE.md", &["forbidden-one", "forbidden-two", "forbidden-three"])];
        let content = "this text carries none of the forbidden words";

        assert_eq!(find_violation(&ranked, "NOTES.md", content, Some(dir.path())), None);
    }

    // ----------------------------------------------------------- payload

    /// THE DEFECT THIS PREVENTS: a payload missing the field this guard
    /// needs (or carrying the wrong type there, or naming a tool this guard
    /// has no notion of) is treated as empty content, or panics, instead of
    /// simply allowing the call through in silence.
    #[test]
    fn an_unparseable_or_missing_payload_allows_silently() {
        let write_missing = serde_json::json!({ "file_path": "a.md" });
        assert_eq!(proposed_content("Write", Some(&write_missing)), None);

        let edit_missing = serde_json::json!({ "file_path": "a.md", "old_string": "x" });
        assert_eq!(proposed_content("Edit", Some(&edit_missing)), None);

        let wrong_type = serde_json::json!({ "content": 42 });
        assert_eq!(proposed_content("Write", Some(&wrong_type)), None);

        assert_eq!(proposed_content("Write", None), None);

        let unrelated_tool = serde_json::json!({ "command": "ls" });
        assert_eq!(proposed_content("Bash", Some(&unrelated_tool)), None);
    }

    #[test]
    fn write_reads_content_and_edit_reads_new_string() {
        let write_input = serde_json::json!({ "file_path": "a.md", "content": "hello" });
        assert_eq!(proposed_content("Write", Some(&write_input)), Some("hello"));

        let edit_input = serde_json::json!({ "file_path": "a.md", "old_string": "x", "new_string": "y" });
        assert_eq!(proposed_content("Edit", Some(&edit_input)), Some("y"));
    }

    // ------------------------------------------------ content_after_write

    /// THE DEFECT THIS PREVENTS (2026-08-07): the "must remain" arm was fed
    /// the replacement fragment, so a small edit to a guarded file could
    /// never contain the guarded literal and was always refused. Caught by
    /// the guard refusing a two-word comment correction in the very file it
    /// guards.
    #[test]
    fn an_edit_resolves_to_the_whole_file_not_to_its_replacement_fragment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn keep_me() {}\n// a typo here\n").unwrap();
        let input = serde_json::json!({ "old_string": "a typo", "new_string": "the typo" });

        let after = content_after_write("Edit", Some(&input), Some(dir.path()), "a.rs").unwrap();
        assert!(after.contains("keep_me"), "the untouched half of the file must still be there: {after:?}");
        assert!(after.contains("the typo"), "the replacement must have been applied: {after:?}");
        assert!(!after.contains("a typo"));
    }

    #[test]
    fn an_edit_that_really_removes_the_literal_still_shows_it_gone() {
        // The other direction, so the fix cannot be "always allow".
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn keep_me() {}\n").unwrap();
        let input = serde_json::json!({ "old_string": "fn keep_me() {}", "new_string": "fn gone() {}" });

        let after = content_after_write("Edit", Some(&input), Some(dir.path()), "a.rs").unwrap();
        assert!(!after.contains("keep_me"));
    }

    #[test]
    fn replace_all_is_honoured_and_a_single_edit_replaces_only_the_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x x x\n").unwrap();

        let one = serde_json::json!({ "old_string": "x", "new_string": "y" });
        assert_eq!(content_after_write("Edit", Some(&one), Some(dir.path()), "a.rs").unwrap(), "y x x\n");

        let all = serde_json::json!({ "old_string": "x", "new_string": "y", "replace_all": true });
        assert_eq!(content_after_write("Edit", Some(&all), Some(dir.path()), "a.rs").unwrap(), "y y y\n");
    }

    #[test]
    fn a_write_carries_the_whole_file_already() {
        let dir = tempfile::tempdir().unwrap();
        let input = serde_json::json!({ "content": "everything" });
        assert_eq!(
            content_after_write("Write", Some(&input), Some(dir.path()), "a.rs").as_deref(),
            Some("everything")
        );
    }

    /// Every failure is silence, so the caller declines to decide rather
    /// than deciding wrongly - the same stance the rest of this module
    /// takes. A guard that cannot see what would remain must never claim
    /// something is about to disappear.
    #[test]
    fn anything_it_cannot_resolve_is_none_never_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "hello\n").unwrap();
        let ok = serde_json::json!({ "old_string": "hello", "new_string": "bye" });

        // No root to resolve the path against.
        assert_eq!(content_after_write("Edit", Some(&ok), None, "a.rs"), None);
        // A file that is not there.
        assert_eq!(content_after_write("Edit", Some(&ok), Some(dir.path()), "missing.rs"), None);
        // An old_string the file does not contain: the edit would not apply.
        let wont_apply = serde_json::json!({ "old_string": "nowhere", "new_string": "x" });
        assert_eq!(content_after_write("Edit", Some(&wont_apply), Some(dir.path()), "a.rs"), None);
        // A tool this has no notion of, and a payload missing its fields.
        assert_eq!(content_after_write("Bash", Some(&ok), Some(dir.path()), "a.rs"), None);
        let half = serde_json::json!({ "old_string": "hello" });
        assert_eq!(content_after_write("Edit", Some(&half), Some(dir.path()), "a.rs"), None);
        assert_eq!(content_after_write("Edit", None, Some(dir.path()), "a.rs"), None);
    }

    // ------------------------------------------------------------- marker

    /// THE DEFECT THIS PREVENTS: a second write to the same file in the same
    /// session blocks again, turning one violation into a repeated nag - and
    /// the converse defect, a marker so coarse it suppresses an unrelated
    /// file or an unrelated session too.
    #[test]
    fn two_edits_with_the_same_replacement_but_different_targets_are_two_attempts() {
        // THE DEFECT THIS PREVENTS. The fingerprint used to be taken over
        // `new_string` alone, so replacing "alpha" with "x" and replacing
        // "beta" with "x" hashed identically and the second one was called a
        // repeat of the first. Different edits, one key.
        let one = serde_json::json!({"old_string": "alpha", "new_string": "x"});
        let two = serde_json::json!({"old_string": "beta", "new_string": "x"});
        let key_one = attempt_key("f.rs", attempt_text("Edit", Some(&one)).as_deref());
        let key_two = attempt_key("f.rs", attempt_text("Edit", Some(&two)).as_deref());
        assert_ne!(key_one, key_two, "two different edits must not share one attempt key");

        // And the same edit twice still is one attempt, which is the whole
        // point of having a key at all.
        let again = serde_json::json!({"old_string": "alpha", "new_string": "x"});
        assert_eq!(key_one, attempt_key("f.rs", attempt_text("Edit", Some(&again)).as_deref()));

        // The separator is load-bearing: without it ("ab","c") and ("a","bc")
        // would concatenate to the same string.
        let split_one = serde_json::json!({"old_string": "ab", "new_string": "c"});
        let split_two = serde_json::json!({"old_string": "a", "new_string": "bc"});
        assert_ne!(
            attempt_key("f.rs", attempt_text("Edit", Some(&split_one)).as_deref()),
            attempt_key("f.rs", attempt_text("Edit", Some(&split_two)).as_deref())
        );
    }

    #[test]
    fn every_refusal_message_names_a_readable_rule_id() {
        // The gate's telemetry reads the id back out of the wording, so the
        // wording is load-bearing. All four builders, plus an escalated
        // repeat, which must still parse.
        let content = block_message("a-1", "no dashes", "a - b", "-");
        let location = location_block_message("a-2", "keep out", "secrets/", "secrets/key.txt");
        let missing = format!(
            "[THOR] rule {} requires this to stay in {} (\"{}\"); it is missing from what is about to be written: \"{}\"",
            "a-3", "NOTES.md", "keep the header", "header"
        );
        for (message, expected) in [(&content, "a-1"), (&location, "a-2"), (&missing, "a-3")] {
            assert_eq!(rule_id_of(message), Some(expected), "{message}");
            assert_eq!(rule_id_of(&escalated(message)), Some(expected), "escalated: {message}");
        }
        assert_eq!(rule_id_of("something else entirely"), None);
    }

    #[test]
    fn an_escalated_repeat_keeps_the_original_reason_and_says_it_is_a_repeat() {
        // The point of the escalation is that a repeat is still REFUSED. It
        // must therefore still carry why, or the caller loses the one thing
        // it needs to fix the write.
        let first = block_message("a-1", "no dashes", "a - b", "-");
        let again = escalated(&first);
        assert!(again.starts_with(&first), "the original reason must survive verbatim: {again}");
        assert!(again.contains("ALREADY been refused"), "{again}");
        assert_ne!(again, first, "a repeat that reads identically teaches the caller nothing new");
    }

    #[test]
    fn a_session_that_already_blocked_a_file_is_not_blocked_again_for_that_file() {
        let text = mark_blocked_text(None, "s1", "NOTES.md").unwrap();
        let markers = read_blocked(&text);
        assert!(already_blocked(&markers, "s1", "NOTES.md"));
        assert!(!already_blocked(&markers, "s1", "OTHER.md"), "a different file in the same session must not be suppressed");
        assert!(!already_blocked(&markers, "s2", "NOTES.md"), "a different session must not be suppressed");
    }

    /// Fail-open: a marker file that is missing or malformed reads back as
    /// nobody-blocked-yet, never an error.
    #[test]
    fn a_missing_or_malformed_marker_file_reads_as_empty() {
        assert!(read_blocked("").is_empty());
        assert!(read_blocked("{ not json").is_empty());
        assert!(!already_blocked(&read_blocked(""), "s1", "NOTES.md"));
    }

    // -------------------------------------------------------- location guard

    /// A live Rule anchoring `anchor_path` (kind `target_kind`, `Path` or
    /// `Dir`), carrying a `Check::PathExists` for that SAME path and
    /// `severity: Irreversible` - the minimal shape `location_anchor`
    /// requires. Mirrors `item_with_check` above: one literal reused for
    /// both the binding value and the check path, never two independently
    /// chosen strings.
    fn location_item(id: &str, target_kind: TargetKind, anchor_path: &str) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: never write here"),
                bindings: vec![Binding::Target { kind: target_kind, value: anchor_path.to_string() }],
                severity: Some(Severity::Irreversible),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out to be safe to touch")),
                check: Some(Check::PathExists { path: anchor_path.to_string() }),
            },
        }
    }

    /// THE DEFECT THIS PREVENTS: a write into a file INSIDE a directory a
    /// rule protects goes through unblocked, or blocks with a message that
    /// does not name both the protected directory and the file that was
    /// actually refused.
    #[test]
    fn a_write_inside_a_protected_directory_is_blocked_and_names_both_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frozen")).unwrap();
        let items = vec![location_item("loc1", TargetKind::Dir, "frozen")];

        let reason = find_location_violation(&items, "frozen/sub/cli.rs", Some(dir.path())).expect("must block");

        assert!(reason.starts_with("[THOR]"), "{reason}");
        assert!(reason.contains("loc1"), "must name the rule id: {reason}");
        assert!(reason.contains("never write here"), "must carry the rule's own text: {reason}");
        assert!(reason.contains("frozen"), "must name the protected path: {reason}");
        assert!(reason.contains("frozen/sub/cli.rs"), "must name the refused file: {reason}");
    }

    /// THE DEFECT THIS PREVENTS: the guard blocks a write no protected
    /// directory actually contains, either by matching too broadly or by
    /// mishandling the containment comparison.
    #[test]
    fn a_write_outside_the_protected_directory_is_not_blocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frozen")).unwrap();
        let items = vec![location_item("loc1", TargetKind::Dir, "frozen")];

        assert_eq!(find_location_violation(&items, "elsewhere/cli.rs", Some(dir.path())), None);
    }

    /// A directory named "froze" must never count as containing a write
    /// under a DIFFERENT, merely similarly-prefixed directory "frozen-copy" -
    /// pins the trailing-`/` guard in `path_is_or_contains` that keeps
    /// "contains" from silently degrading into "shares a text prefix with".
    #[test]
    fn a_directory_that_only_shares_a_text_prefix_does_not_count_as_containing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frozen")).unwrap();
        std::fs::create_dir(dir.path().join("frozen-copy")).unwrap();
        let items = vec![location_item("loc1a", TargetKind::Dir, "frozen")];

        assert_eq!(find_location_violation(&items, "frozen-copy/cli.rs", Some(dir.path())), None);
    }

    /// The other half of requirement 1: the protected path can BE the file
    /// itself (a `Path` anchor), never only a `Dir` one.
    #[test]
    fn a_write_to_the_exact_protected_file_is_blocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secrets.env"), b"x").unwrap();
        let items = vec![location_item("loc2", TargetKind::Path, "secrets.env")];

        let reason = find_location_violation(&items, "secrets.env", Some(dir.path())).expect("must block");
        assert!(reason.contains("secrets.env"));
    }

    /// THE DEFECT THIS PREVENTS: severity is what turns a description into a
    /// prohibition (see `location_anchor`'s own doc comment) - a rule that
    /// merely happens to carry a `PathExists` check must never block when its
    /// severity is anything short of `Irreversible`.
    #[test]
    fn a_pathexists_check_without_irreversible_severity_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frozen")).unwrap();
        let mut item = location_item("loc3", TargetKind::Dir, "frozen");
        item.item.severity = Some(Severity::Costly);

        assert_eq!(find_location_violation(&[item], "frozen/cli.rs", Some(dir.path())), None);
    }

    /// The other half: `Irreversible` severity alone, with no check at all,
    /// must never block either - severity says how bad it would be, the
    /// check is what proves the rule is still current, and neither stands in
    /// for the other.
    #[test]
    fn irreversible_severity_without_a_check_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frozen")).unwrap();
        let mut item = location_item("loc4", TargetKind::Dir, "frozen");
        item.item.check = None;

        assert_eq!(find_location_violation(&[item], "frozen/cli.rs", Some(dir.path())), None);
    }

    /// THE DEFECT THIS PREVENTS: a check whose OWN path names something
    /// other than the item's actual anchor would let a currency proof for
    /// one path justify a block on a completely different one.
    #[test]
    fn a_check_pointing_at_a_different_path_than_the_binding_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frozen")).unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), b"x").unwrap();
        let mut item = location_item("loc5", TargetKind::Dir, "frozen");
        item.item.check = Some(Check::PathExists { path: "unrelated.txt".to_string() });

        assert_eq!(find_location_violation(&[item], "frozen/cli.rs", Some(dir.path())), None);
    }

    /// THE DEFECT THIS PREVENTS: a rule anchored to a directory that has
    /// since been deleted or renamed still blocks, on the strength of a
    /// currency claim that is no longer provable - the same guarantee
    /// `a_rule_whose_anchored_path_no_longer_exists_never_blocks` proves for
    /// the content guard, proven again here for the location guard's own
    /// (separate) matching path.
    #[test]
    fn a_rule_whose_protected_path_no_longer_exists_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        // "frozen" is deliberately never created.
        let items = vec![location_item("loc6", TargetKind::Dir, "frozen")];

        assert_eq!(find_location_violation(&items, "frozen/cli.rs", Some(dir.path())), None);
    }

    /// Defense in depth, mirroring `rank::select`'s own guarantee for the
    /// content-guard path (`a_report_can_never_reach_a_gate_even_bypassing_the_write_gate`):
    /// a kind that can never fire must never block here either, even if it
    /// somehow carries a matching check and severity - constructed directly,
    /// as if the write gate (which refuses a check on any such kind, ground
    /// 12) had never run, because this guard's own candidate fetch does not
    /// run through `rank::select`'s own `eligible` filter to get this for
    /// free.
    #[test]
    fn a_kind_that_can_never_fire_never_blocks_even_with_a_matching_check_and_severity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frozen")).unwrap();
        let mut item = location_item("loc7", TargetKind::Dir, "frozen");
        item.item.kind = Kind::Lookup;
        item.item.key = Some("loc7".to_string());

        assert_eq!(find_location_violation(&[item], "frozen/cli.rs", Some(dir.path())), None);
    }

    /// With no resolvable root, currency (and containment) can never be
    /// proven for anything - the same guarantee
    /// `no_resolvable_root_means_currency_can_never_be_proven_so_nothing_blocks`
    /// proves for the content guard, proven again here.
    #[test]
    fn no_resolvable_root_means_the_location_guard_never_blocks_either() {
        let items = vec![location_item("loc8", TargetKind::Dir, "frozen")];
        assert_eq!(find_location_violation(&items, "frozen/cli.rs", None), None);
    }

    // ------------------------------------------------ block, dir-anchored content

    /// A live Rule bound to `dir_path` via a `Dir` target, carrying a
    /// `Check::Absent` for that SAME path forbidding `literal` - the minimal
    /// shape `dir_binding`/`first_dir_violation` require. Mirrors
    /// `item_with_check` above (one literal reused for the binding's own
    /// value and the check's own path, never two independently chosen
    /// strings) and `location_item` above (a `LiveItem`, since this guard's
    /// own `Dir`-anchored half never goes through `rank::select` either).
    fn dir_item_with_check(id: &str, dir_path: &str, literal: &str) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: keep everything under this directory clean"),
                bindings: vec![Binding::Target { kind: TargetKind::Dir, value: dir_path.to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check: Some(Check::Absent { path: dir_path.to_string(), literal: literal.to_string() }),
            },
        }
    }

    /// A live Rule anchored to `file` via a `Path` target and carrying a
    /// `Check::Contains` requiring `literal` to stay in that same file - the
    /// shape a mirrored document or a self-verifying code fact takes.
    fn item_requiring(id: &str, file: &str, literal: &str) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: this file mirrors the memory, keep it"),
                bindings: vec![Binding::Target { kind: TargetKind::Path, value: file.to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check: Some(Check::Contains { path: file.to_string(), literal: literal.to_string() }),
            },
        }
    }

    /// THE DEFECT THIS PREVENTS: a document that mirrors this memory losing
    /// what it mirrors, in silence. Every other arm here watches for
    /// something appearing; nothing watched for something going away.
    #[test]
    fn a_write_that_drops_the_required_literal_is_blocked_and_names_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MIRROR.md"), "the agreed rule: never force-push main\n").unwrap();
        let items = vec![item_requiring("mirror", "MIRROR.md", "never force-push main")];
        let reason = find_missing_required(&items, "MIRROR.md", "a rewritten file with the rule gone", Some(dir.path()))
            .expect("dropping the mirrored line must block");
        assert!(reason.contains("mirror"), "{reason}");
        assert!(reason.contains("never force-push main"), "must name what went missing: {reason}");
    }

    /// THE DEFECT THIS PREVENTS: blocking an ordinary edit that keeps the
    /// mirrored text, which would make the guard unusable on the very files
    /// it is supposed to protect.
    #[test]
    fn a_write_that_keeps_the_required_literal_passes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MIRROR.md"), "the agreed rule: never force-push main\n").unwrap();
        let items = vec![item_requiring("mirror", "MIRROR.md", "never force-push main")];
        assert!(find_missing_required(
            &items,
            "MIRROR.md",
            "rewritten, but the agreed rule: never force-push main is still here",
            Some(dir.path())
        )
        .is_none());
    }

    /// THE DEFECT THIS PREVENTS: trapping the person trying to repair a
    /// mirror that is ALREADY broken. If the literal is not in the file now,
    /// there is nothing to preserve, and refusing the write would block the
    /// fix rather than the damage.
    #[test]
    fn a_required_literal_already_absent_from_the_file_blocks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MIRROR.md"), "someone already removed it\n").unwrap();
        let items = vec![item_requiring("mirror", "MIRROR.md", "never force-push main")];
        assert!(find_missing_required(&items, "MIRROR.md", "still without it", Some(dir.path())).is_none());
    }

    /// THE DEFECT THIS PREVENTS: a rule guarding a file it does not name.
    /// Identity here is the check's own path, so a rule about one file must
    /// never fire on a write to another.
    #[test]
    fn a_rule_naming_a_different_file_is_not_consulted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MIRROR.md"), "the agreed rule: never force-push main\n").unwrap();
        let items = vec![item_requiring("mirror", "MIRROR.md", "never force-push main")];
        assert!(find_missing_required(&items, "OTHER.md", "nothing of the sort here", Some(dir.path())).is_none());
    }

    /// A live Rule bound `Always`, carrying a self-contained
    /// `Check::Forbidden` over `literals` - no path anywhere in it, which is
    /// the whole point of the form. Deliberately carries NO target binding,
    /// so a test built from this cannot accidentally pass because some
    /// anchor happened to match.
    fn always_item_with_forbidden_check(id: &str, literals: &[&str]) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: never write these, anywhere"),
                bindings: vec![Binding::Always],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check: Some(Check::Forbidden {
                    literals: literals.iter().map(|l| l.to_string()).collect(),
                }),
            },
        }
    }

    /// THE DEFECT THIS PREVENTS: a rule that forbids something self-contained
    /// has no anchor to prove, and an earlier shape made it point at one
    /// directory as a formality - which quietly narrowed a universal rule
    /// down to that single directory and left every other project silently
    /// uncovered.
    #[test]
    fn an_always_bound_forbidden_rule_blocks_a_write_it_has_no_anchor_for() {
        let items = vec![always_item_with_forbidden_check("no-em-dash", &["\u{2014}"])];
        let reason = find_forbidden_violation(&items, "a line with an \u{2014} in it")
            .expect("an Always-bound Forbidden rule must block whatever file is being written");
        assert!(reason.contains("no-em-dash"), "{reason}");
        assert!(reason.contains('\u{2014}'), "the reason must quote the character itself: {reason}");
    }

    /// THE DEFECT THIS PREVENTS: reporting an arbitrary member of the set
    /// rather than the one that actually matched, which sends the reader
    /// looking for a character that is not there.
    #[test]
    fn a_forbidden_rule_quotes_the_literal_that_actually_matched() {
        let items = vec![always_item_with_forbidden_check("typography", &["\u{2014}", "\u{2026}", "\u{2019}"])];
        let reason = find_forbidden_violation(&items, "three dots like \u{2026} here")
            .expect("the middle literal must still block");
        assert!(reason.contains('\u{2026}'), "{reason}");
        assert!(!reason.contains('\u{2014}'), "must not quote a literal that did not match: {reason}");
    }

    /// THE DEFECT THIS PREVENTS: requiring a currency proof for a rule that
    /// names nothing outside itself. There is no path to resolve, so no root
    /// is needed, and demanding one would make the form unusable exactly
    /// where it is meant to work.
    #[test]
    fn a_forbidden_rule_needs_no_root_and_no_currency_proof() {
        let items = vec![always_item_with_forbidden_check("no-em-dash", &["\u{2014}"])];
        assert!(find_forbidden_violation(&items, "clean text with a plain - hyphen").is_none());
        assert!(find_forbidden_violation(&items, "dirty \u{2014} text").is_some());
    }

    /// THE DEFECT THIS PREVENTS: a check form this arm does not own being
    /// silently swept into it, which would block on an anchored rule without
    /// ever proving its anchor still exists.
    #[test]
    fn a_path_anchored_check_is_invisible_to_the_self_contained_arm() {
        let anchored = dir_item_with_check("anchored", "docs", "\u{2014}");
        assert!(find_forbidden_violation(&[anchored], "text with \u{2014} in it").is_none());
    }

    /// THE DEFECT THIS PREVENTS, and it is the one a review called worse than
    /// what it replaced. A `Forbidden` check carries no path, so its reach IS
    /// its binding: Always means every write to every file, and anything
    /// narrower is a rule written about a place this arm cannot honour.
    ///
    /// That requirement used to live only in the CALLER, which passes
    /// `live::always_candidates`. That function falls back to `live_items` -
    /// the whole store - on three separate paths, so with a cold binding
    /// projection a target-bound `Forbidden` check would refuse EVERY write,
    /// while `doctor` classified it as blocking nothing. The requirement is
    /// now local, so no pool can hand this arm something it must not act on.
    #[test]
    fn a_forbidden_check_without_an_always_binding_can_never_block_whatever_pool_it_arrives_in() {
        let mut pinned = always_item_with_forbidden_check("pinned", &["\u{2014}"]);
        pinned.item.bindings = vec![Binding::Always];
        assert!(
            find_forbidden_violation(&[pinned], "text with \u{2014} in it").is_some(),
            "an Always-bound Forbidden check is exactly what this arm exists for"
        );

        for bindings in [
            vec![Binding::Moment(intent::Action::Commit)],
            vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }],
            vec![Binding::Target { kind: TargetKind::Command, value: "git commit".to_string() }],
        ] {
            let mut narrower = always_item_with_forbidden_check("narrower", &["\u{2014}"]);
            narrower.item.bindings = bindings.clone();
            assert!(
                find_forbidden_violation(&[narrower], "text with \u{2014} in it").is_none(),
                "a Forbidden check on {bindings:?} must never block, even handed straight to this arm"
            );
        }
    }

    /// The set-form counterpart to `dir_item_with_check` above, mirroring how
    /// `item_with_absent_all_check` mirrors `item_with_check` for the
    /// `Path`-anchored half.
    fn dir_item_with_absent_all_check(id: &str, dir_path: &str, literals: &[&str]) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: keep everything under this directory clean"),
                bindings: vec![Binding::Target { kind: TargetKind::Dir, value: dir_path.to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check: Some(Check::AbsentAll {
                    path: dir_path.to_string(),
                    literals: literals.iter().map(|s| s.to_string()).collect(),
                }),
            },
        }
    }

    /// THE DEFECT THIS PREVENTS - the bug report's own reproduction: a
    /// content rule bound to a DIRECTORY never fired for a file directly
    /// inside it, because the file was only ever offered to `find_violation`
    /// as a `Path` target and a `Dir` binding can never match a `Path`-kind
    /// input through `rank::target_matches` (kind equality is required
    /// there) - see this module's own top-of-file doc comment.
    /// `path_is_or_contains` is what proves containment instead.
    #[test]
    fn a_dir_anchored_content_rule_blocks_a_file_directly_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        let items = vec![dir_item_with_check("d1", "thor2", "forbidden")];
        let content = "a line carrying the forbidden word right here";

        let reason = find_dir_content_violation(&items, "thor2/probe-scratch.md", content, Some(dir.path()))
            .expect("must block");
        assert!(reason.starts_with("[THOR]"), "{reason}");
        assert!(reason.contains("d1"), "must name the rule id: {reason}");
        assert!(reason.contains("forbidden"), "must quote the offending fragment: {reason}");
    }

    /// The same, at depth: containment must hold no matter how many path
    /// segments separate the file from the directory that protects it.
    #[test]
    fn a_dir_anchored_content_rule_blocks_a_file_several_levels_deep() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        let items = vec![dir_item_with_check("d2", "thor2", "forbidden")];
        let content = "a line carrying the forbidden word";

        let reason =
            find_dir_content_violation(&items, "thor2/serve/src/absent_guard.rs", content, Some(dir.path()))
                .expect("must block");
        assert!(reason.contains("d2"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS (the false-positive half): a directory that
    /// only shares a text PREFIX with the protected one must never count as
    /// contained - pins the same trailing-`/` guard in `path_is_or_contains`
    /// the location guard's own
    /// `a_directory_that_only_shares_a_text_prefix_does_not_count_as_containing`
    /// test already pins, proven again here for this guard's new consumer of
    /// that function.
    #[test]
    fn a_sibling_directory_that_only_shares_a_text_prefix_is_not_blocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        std::fs::create_dir(dir.path().join("thor2-copy")).unwrap();
        let items = vec![dir_item_with_check("d3", "thor2", "forbidden")];
        let content = "a line carrying the forbidden word";

        assert_eq!(
            find_dir_content_violation(&items, "thor2-copy/probe-scratch.md", content, Some(dir.path())),
            None
        );
    }

    /// THE DEFECT THIS PREVENTS (requirement 3, "unchanged"): adding
    /// `Dir`-anchor support must never change how a `Path`-anchored content
    /// rule behaves. `find_violation` itself is untouched by this change
    /// (see this module's own top-of-file doc comment); this proves the NEW
    /// function has no opinion on a `Path`-bound item either - `dir_binding`
    /// only ever extracts a `Dir` binding, so a `Path`-bound item is
    /// invisible to it, exactly as an `Always`- or `Moment`-bound one would
    /// be.
    #[test]
    fn a_path_anchored_item_is_invisible_to_the_dir_anchored_function() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NOTES.md"), "# notes\n").unwrap();
        let path_item = LiveItem {
            id: "p1".to_string(),
            item: Item {
                id: "p1".to_string(),
                kind: Kind::Rule,
                text: "p1: keep this file clean".to_string(),
                bindings: vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some("p1 turns out not to matter".to_string()),
                check: Some(Check::Absent { path: "NOTES.md".to_string(), literal: "forbidden".to_string() }),
            },
        };
        let content = "this text contains the forbidden word";

        assert_eq!(find_dir_content_violation(&[path_item], "NOTES.md", content, Some(dir.path())), None);
    }

    /// THE DEFECT THIS PREVENTS (requirement 5, currency): a `Dir`-anchored
    /// rule whose protected directory has since been deleted or renamed must
    /// never block - the same currency guarantee
    /// `a_rule_whose_anchored_path_no_longer_exists_never_blocks` already
    /// proves for the `Path`-anchored case - and the silence must still be
    /// recorded, mirroring
    /// `a_failing_currency_check_on_a_matching_content_rule_is_recorded_and_still_allows`.
    #[test]
    fn a_dir_anchored_rule_whose_directory_no_longer_exists_never_blocks_and_is_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        // "thor2" is deliberately never created.
        let items = vec![dir_item_with_check("d4", "thor2", "forbidden")];
        let content = "a line carrying the forbidden word";

        assert_eq!(
            find_dir_content_violation(&items, "thor2/probe-scratch.md", content, Some(dir.path())),
            None,
            "must still allow"
        );

        let findings = stale_in_dir_content(&items, "thor2/probe-scratch.md", Some(dir.path()));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "d4");
        assert_eq!(findings[0].outcome, StaleOutcome::Failed);
        assert_eq!(findings[0].file, "thor2/probe-scratch.md");
        assert!(findings[0].check.contains("Absent"), "{}", findings[0].check);
    }

    /// Defense in depth, mirroring
    /// `a_kind_that_can_never_fire_never_blocks_even_with_a_matching_check_and_severity`:
    /// a kind that can never fire must never block here either, even with an
    /// otherwise-matching `Dir` binding and `Check::Absent` - constructed
    /// directly, as if the write gate had never run, because this function's
    /// own candidate pool does not go through `rank::select`'s own
    /// `eligible` filter to get this for free.
    #[test]
    fn a_kind_that_can_never_fire_never_blocks_even_with_a_matching_dir_binding_and_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        let mut item = dir_item_with_check("d5", "thor2", "forbidden");
        item.item.kind = Kind::Lookup;
        item.item.key = Some("d5".to_string());
        let content = "a line carrying the forbidden word";

        assert_eq!(
            find_dir_content_violation(&[item], "thor2/probe-scratch.md", content, Some(dir.path())),
            None
        );
    }

    /// With no resolvable root, currency (and containment) can never be
    /// proven for anything - the same guarantee
    /// `no_resolvable_root_means_currency_can_never_be_proven_so_nothing_blocks`
    /// proves for the `Path`-anchored content guard, proven again here for
    /// its `Dir`-anchored sibling.
    #[test]
    fn no_resolvable_root_means_the_dir_anchored_function_never_blocks_either() {
        let items = vec![dir_item_with_check("d6", "thor2", "forbidden")];
        let content = "a line carrying the forbidden word";
        assert_eq!(find_dir_content_violation(&items, "thor2/probe-scratch.md", content, None), None);
    }

    // ------------------------------------------- block, AbsentAll (set form), dir-anchored
    //
    // The `Dir`-anchored mirror of the "block, AbsentAll (set form)" section
    // above, over `dir_item_with_absent_all_check` - same three positions
    // (first/middle/last) plus the all-absent negative, proven again here
    // for `find_dir_content_violation` since it never shares an
    // implementation with `find_violation` beyond the common
    // `absent_literals`/`first_present_literal` helpers.

    /// THE DEFECT THIS PREVENTS: a `Dir`-anchored set-carrying rule never
    /// blocks at all, or blocks but reports the wrong literal when the FIRST
    /// one in the list is the one actually present.
    #[test]
    fn a_dir_anchored_set_carrying_rule_blocks_on_the_first_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        let items =
            vec![dir_item_with_absent_all_check("d10", "thor2", &["forbidden-one", "forbidden-two", "forbidden-three"])];
        let content = "this text carries forbidden-one right here";

        let reason = find_dir_content_violation(&items, "thor2/probe-scratch.md", content, Some(dir.path()))
            .expect("must block");
        assert!(reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-two"), "{reason}");
        assert!(!reason.contains("forbidden-three"), "{reason}");
    }

    /// The middle position, `Dir`-anchored: catches an implementation that
    /// only ever checks the first or the last literal.
    #[test]
    fn a_dir_anchored_set_carrying_rule_blocks_on_a_middle_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        let items =
            vec![dir_item_with_absent_all_check("d11", "thor2", &["forbidden-one", "forbidden-two", "forbidden-three"])];
        let content = "this text carries forbidden-two right here";

        let reason = find_dir_content_violation(&items, "thor2/probe-scratch.md", content, Some(dir.path()))
            .expect("must block");
        assert!(reason.contains("forbidden-two"), "{reason}");
        assert!(!reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-three"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS: a naive `Dir`-anchored implementation that
    /// only ever checks the FIRST literal would miss a violation sitting in
    /// the last position.
    #[test]
    fn a_dir_anchored_set_carrying_rule_blocks_on_the_last_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        let items =
            vec![dir_item_with_absent_all_check("d12", "thor2", &["forbidden-one", "forbidden-two", "forbidden-three"])];
        let content = "this text carries forbidden-three right here";

        let reason = find_dir_content_violation(&items, "thor2/probe-scratch.md", content, Some(dir.path()))
            .expect("must block");
        assert!(reason.contains("forbidden-three"), "{reason}");
        assert!(!reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-two"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS: a `Dir`-anchored set-carrying rule blocks
    /// even though NONE of its literals is actually present.
    #[test]
    fn a_dir_anchored_set_carrying_rule_whose_literals_are_all_absent_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        let items = vec![dir_item_with_absent_all_check("d13", "thor2", &["forbidden-one", "forbidden-two"])];
        let content = "this text carries none of the forbidden words";

        assert_eq!(find_dir_content_violation(&items, "thor2/probe-scratch.md", content, Some(dir.path())), None);
    }

    /// The set-form counterpart to
    /// `a_dir_anchored_rule_whose_directory_no_longer_exists_never_blocks_
    /// and_is_recorded_as_stale`: a `Dir`-anchored set-carrying rule whose
    /// check cannot prove its own currency must never block, and the silence
    /// must still be recorded.
    #[test]
    fn a_dir_anchored_set_carrying_rule_whose_check_cannot_run_does_not_block_and_is_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        // "thor2" is deliberately never created.
        let items = vec![dir_item_with_absent_all_check("d14", "thor2", &["forbidden-one", "forbidden-two"])];
        let content = "this text carries forbidden-one";

        assert_eq!(
            find_dir_content_violation(&items, "thor2/probe-scratch.md", content, Some(dir.path())),
            None,
            "must still allow"
        );

        let findings = stale_in_dir_content(&items, "thor2/probe-scratch.md", Some(dir.path()));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "d14");
        assert_eq!(findings[0].outcome, StaleOutcome::Failed);
        assert!(findings[0].check.contains("AbsentAll"), "{}", findings[0].check);
    }

    // ---------------------------------------------- block, command-anchored

    /// A live Rule bound to `command_anchor` via a `Command` target, carrying
    /// a `Check::Absent` for `check_path` forbidding `literal` - the minimal
    /// shape `command_binding`/`first_command_violation` require. Unlike
    /// `item_with_check`/`dir_item_with_check` above, `check_path` is
    /// deliberately a DIFFERENT string from `command_anchor`, never one
    /// reused twice: a command anchor ("git commit") is not a file path, and
    /// this guard's own doc comment makes a point of currency being proved
    /// against the check's own path, completely independent of the Command
    /// binding's own value - reusing one string for both here would hide
    /// that distinction instead of demonstrating it. `check_path` is
    /// realistic on purpose: a policy document is what a real commit-message
    /// rule's currency would actually hang off.
    fn command_item_with_check(id: &str, command_anchor: &str, check_path: &str, literal: &str) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: keep this command's own text clean"),
                bindings: vec![Binding::Target { kind: TargetKind::Command, value: command_anchor.to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check: Some(Check::Absent { path: check_path.to_string(), literal: literal.to_string() }),
            },
        }
    }

    /// A command prohibition that names no file at all: the self-contained
    /// form, reaching exactly the command its binding names.
    fn command_item_with_forbidden_check(id: &str, command_anchor: &str, literals: &[&str]) -> LiveItem {
        let mut item = command_item_with_check(id, command_anchor, "unused", "unused");
        item.item.check =
            Some(Check::Forbidden { literals: literals.iter().map(|l| l.to_string()).collect() });
        item
    }

    /// THE CLASS THIS OPENS. A command prohibition - never run `gh repo edit
    /// --visibility public`, never run `thor import` - is not about a file, and
    /// the arm used to demand one anyway: `absent_literals` returned nothing
    /// for `Forbidden`, so the only way in was an `Absent` check pointed at
    /// some file picked to satisfy the currency proof. That is the formality
    /// this project's own doctrine warns against, and it kept the entire class
    /// out of enforcement.
    ///
    /// A `Forbidden` check names no path, so its reach is its BINDING - the
    /// same reasoning the file arm already applies to `Always`. Nothing here
    /// needs to prove itself current, because such a rule goes stale only when
    /// its owner changes their mind, which is what the falsifier is for.
    #[test]
    fn a_command_prohibition_needs_no_file_to_prove_itself_by() {
        let dir = tempfile::tempdir().unwrap();
        let items = vec![command_item_with_forbidden_check("no-public", "gh repo edit", &["--visibility public"])];

        let (reason, anchor) =
            find_command_violation(&items, "gh repo edit --visibility public", Some(dir.path()))
                .expect("a self-contained command prohibition must block with no file anywhere");
        assert!(reason.contains("no-public"), "{reason}");
        assert!(reason.contains("--visibility public"), "must quote what matched: {reason}");
        assert_eq!(anchor, "gh repo edit");

        // The same rule, a harmless invocation of the same command: no block.
        assert!(
            find_command_violation(&items, "gh repo edit --description x", Some(dir.path())).is_none(),
            "the anchor alone must never block; the literal has to be there"
        );

        // And a DIFFERENT command carrying the literal is not this rule's
        // business - the binding is what carries the reach.
        assert!(
            find_command_violation(&items, "echo --visibility public", Some(dir.path())).is_none(),
            "a Forbidden check on a Command binding reaches that command and nothing else"
        );
    }

    /// THE TWO DEFECTS THIS PINS, both found by probing the real gate rather
    /// than by reading it. A rule carrying TWO command anchors only ever used
    /// the first, so the spelling its owner actually types went unguarded. And
    /// the same program run under sudo, by absolute path, or with a .exe
    /// suffix read as a different command entirely - which fails in the one
    /// direction that matters, since the dangerous form is usually the one
    /// typed with a full path.
    #[test]
    fn a_command_prohibition_survives_sudo_a_full_path_and_an_exe_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let items = vec![command_item_with_forbidden_check("no-reset", "git reset", &["--hard"])];

        for command in [
            "git reset --hard origin/main",
            "sudo git reset --hard origin/main",
            "/usr/bin/git reset --hard origin/main",
            "GIT_DIR=/srv/x git reset --hard origin/main",
        ] {
            assert!(
                find_command_violation(&items, command, Some(dir.path())).is_some(),
                "the same act, differently typed, must still be refused: {command}"
            );
        }

        // A different subcommand of the same program is not this rule.
        assert!(find_command_violation(&items, "sudo git pull --ff-only", Some(dir.path())).is_none());

        // THE LIMIT, pinned rather than hidden: an executable path containing
        // a SPACE is split into two words before anything else runs, so the
        // first word is not the program at all and the anchor cannot match.
        // Handling it properly means parsing shell quoting, which is the one
        // thing both reviews of this guard said not to build - a false
        // positive machine aimed at exactly the place false positives teach
        // people to route around the gate. Stated here so the next reader
        // knows it is a decision, not an oversight.
        assert!(
            find_command_violation(&items, r"C:\Program Files\Git\git.exe reset --hard x", Some(dir.path()))
                .is_none(),
            "if this starts blocking, someone taught the matcher about quoting - update this test on purpose"
        );
    }

    /// Every anchor an item declares is considered, not just the first.
    #[test]
    fn a_second_command_anchor_is_not_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut item = command_item_with_forbidden_check("two-spellings", "alpha run", &["--wipe"]);
        item.item.bindings.push(Binding::Target { kind: TargetKind::Command, value: "beta run".to_string() });

        let one = vec![LiveItem { id: item.id.clone(), item: item.item.clone() }];
        let two = vec![LiveItem { id: item.id.clone(), item: item.item.clone() }];
        assert!(find_command_violation(&one, "alpha run --wipe", Some(dir.path())).is_some());
        assert!(
            find_command_violation(&two, "beta run --wipe", Some(dir.path())).is_some(),
            "the SECOND anchor must guard too, or it is protection that is not there"
        );
    }

    /// The separation that keeps the two arms honest: a command prohibition
    /// must never leak into the FILE arm, where its binding means nothing.
    #[test]
    fn a_command_bound_forbidden_check_never_blocks_a_file_write() {
        let items = vec![command_item_with_forbidden_check("no-public", "gh repo edit", &["--visibility public"])];
        assert!(
            find_forbidden_violation(&items, "a document that mentions --visibility public").is_none(),
            "only an Always binding reaches every file write"
        );
    }

    /// The set-form counterpart to `command_item_with_check` above, mirroring
    /// how `dir_item_with_absent_all_check` mirrors `dir_item_with_check`.
    fn command_item_with_absent_all_check(
        id: &str,
        command_anchor: &str,
        check_path: &str,
        literals: &[&str],
    ) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: keep this command's own text clean"),
                bindings: vec![Binding::Target { kind: TargetKind::Command, value: command_anchor.to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check: Some(Check::AbsentAll {
                    path: check_path.to_string(),
                    literals: literals.iter().map(|s| s.to_string()).collect(),
                }),
            },
        }
    }

    /// THE DEFECT THIS PREVENTS: a rule anchored to a `Command` target never
    /// blocks at all (the anchor shape this guard exists to add), or blocks
    /// with a message that cannot be traced back to which rule fired or
    /// which literal actually matched.
    #[test]
    fn a_forbidden_literal_in_a_matching_command_is_blocked_and_names_the_rule_and_quotes_the_literal_that_matched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("COMMIT-POLICY.md"), "# commit policy\n").unwrap();
        let items = vec![command_item_with_check("cmd1", "git commit", "COMMIT-POLICY.md", "forbidden")];
        let command = "git commit -m \"a message with a forbidden word in it\"";

        let (reason, anchor) = find_command_violation(&items, command, Some(dir.path())).expect("must block");

        assert!(reason.starts_with("[THOR]"), "{reason}");
        assert!(reason.contains("cmd1"), "must name the rule id: {reason}");
        assert!(reason.contains("keep this command's own text clean"), "must carry the rule's own text: {reason}");
        assert!(reason.contains("forbidden"), "must quote the offending literal: {reason}");
        assert_eq!(anchor, "git commit", "must report the matched item's own anchor, for the caller's marker key");
    }

    /// THE DEFECT THIS PREVENTS: the guard blocks whatever command the
    /// caller happens to be running, regardless of which command the rule is
    /// actually anchored to - the command-guard counterpart to
    /// `a_forbidden_literal_written_to_a_different_file_is_not_blocked`
    /// above. The literal is present in this command too (as one of its
    /// arguments), so this also proves matching gates the block, not merely
    /// literal presence.
    #[test]
    fn the_same_literal_in_a_non_matching_command_is_not_blocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("COMMIT-POLICY.md"), "# commit policy\n").unwrap();
        let items = vec![command_item_with_check("cmd2", "git commit", "COMMIT-POLICY.md", "forbidden")];
        let command = "git push origin main -- forbidden";

        assert_eq!(find_command_violation(&items, command, Some(dir.path())), None);
    }

    /// THE DEFECT THIS PREVENTS: an item with prose describing a rule but no
    /// machine-runnable check ever blocks a command, however well its own
    /// anchor matches what is actually running - only a real `Check::Absent`/
    /// `Check::AbsentAll` may ever cause a block, mirroring
    /// `an_item_with_prose_but_no_check_never_blocks` above for this arm.
    #[test]
    fn a_command_rule_with_prose_but_no_check_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("COMMIT-POLICY.md"), "# commit policy\n").unwrap();
        let mut item = command_item_with_check("cmd3", "git commit", "COMMIT-POLICY.md", "forbidden");
        item.item.check = None;
        let command = "git commit -m \"a forbidden word right here\"";

        assert_eq!(find_command_violation(&[item], command, Some(dir.path())), None);
    }

    /// THE DEFECT THIS PREVENTS: a rule whose check's own anchored file has
    /// since been deleted or renamed still blocks a matching command, on the
    /// strength of a currency claim that is no longer provable - mirrors
    /// `a_rule_whose_anchored_path_no_longer_exists_never_blocks` above, then
    /// proves the "and is recorded" half on top of it, mirroring
    /// `a_dir_anchored_rule_whose_directory_no_longer_exists_never_blocks_and_is_recorded_as_stale`.
    #[test]
    fn a_command_rule_whose_check_cannot_run_never_blocks_and_is_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        // COMMIT-POLICY.md is deliberately never created.
        let items = vec![command_item_with_check("cmd4", "git commit", "COMMIT-POLICY.md", "forbidden")];
        let command = "git commit -m \"a forbidden word right here\"";

        assert_eq!(find_command_violation(&items, command, Some(dir.path())), None, "must still allow");

        let findings = stale_in_command(&items, command, Some(dir.path()));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "cmd4");
        assert_eq!(findings[0].outcome, StaleOutcome::Failed);
        assert_eq!(findings[0].file, command, "the recorded 'file' field carries the command string for this arm");
        assert!(findings[0].check.contains("Absent"), "{}", findings[0].check);
    }

    /// The other half of the currency requirement: with no root at all to
    /// resolve against, currency can never be proven for anything, so
    /// nothing can ever block - mirrors
    /// `no_resolvable_root_means_currency_can_never_be_proven_so_nothing_blocks`
    /// above.
    #[test]
    fn no_resolvable_root_means_the_command_guard_never_blocks_either() {
        let items = vec![command_item_with_check("cmd5", "git commit", "COMMIT-POLICY.md", "forbidden")];
        let command = "git commit -m \"a forbidden word right here\"";
        assert_eq!(find_command_violation(&items, command, None), None);
    }

    /// THE DEFECT THIS PREVENTS (step 2's own scope rule): a check of the
    /// `Contains`/`PathExists` form is silently treated like an `Absent`
    /// check and blocks anyway - mirrors
    /// `a_check_of_another_form_never_blocks_even_if_it_would_otherwise_match`
    /// above for this arm.
    #[test]
    fn a_check_of_another_form_never_blocks_a_command_even_if_it_would_otherwise_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("COMMIT-POLICY.md"), "# commit policy\n").unwrap();
        let mut item = command_item_with_check("cmd6", "git commit", "COMMIT-POLICY.md", "forbidden");
        item.item.check =
            Some(Check::Contains { path: "COMMIT-POLICY.md".to_string(), literal: "forbidden".to_string() });
        let command = "git commit -m \"a forbidden word right here\"";

        assert_eq!(find_command_violation(&[item], command, Some(dir.path())), None);
    }

    /// Defense in depth, mirroring
    /// `a_kind_that_can_never_fire_never_blocks_even_with_a_matching_dir_binding_and_check`:
    /// a kind that can never fire must never block here either, even with an
    /// otherwise-matching `Command` binding and check - constructed directly,
    /// as if the write gate had never run, because this function's own
    /// candidate pool does not go through `rank::select`'s own `eligible`
    /// filter to get this for free.
    #[test]
    fn a_kind_that_can_never_fire_never_blocks_even_with_a_matching_command_binding_and_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("COMMIT-POLICY.md"), "# commit policy\n").unwrap();
        let mut item = command_item_with_check("cmd7", "git commit", "COMMIT-POLICY.md", "forbidden");
        item.item.kind = Kind::Lookup;
        item.item.key = Some("cmd7".to_string());
        let command = "git commit -m \"a forbidden word right here\"";

        assert_eq!(find_command_violation(&[item], command, Some(dir.path())), None);
    }

    /// A rule whose currency HOLDS (and therefore blocks) must never also
    /// show up in the staleness sidecar - mirrors
    /// `a_holding_content_check_that_blocks_is_not_recorded_as_stale` above.
    #[test]
    fn a_holding_command_check_that_blocks_is_not_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("COMMIT-POLICY.md"), "# commit policy\n").unwrap();
        let items = vec![command_item_with_check("cmd9", "git commit", "COMMIT-POLICY.md", "forbidden")];
        let command = "git commit -m \"a forbidden word right here\"";

        assert!(find_command_violation(&items, command, Some(dir.path())).is_some(), "sanity: must block");
        assert!(stale_in_command(&items, command, Some(dir.path())).is_empty());
    }

    /// THE DEFECT THIS PREVENTS: a rule whose `Command` anchor does not even
    /// match the command actually being run still ends up in the staleness
    /// sidecar - mirrors
    /// `a_dir_anchored_rule_that_does_not_contain_the_file_is_not_recorded_as_stale`:
    /// matching must be checked BEFORE currency, so an unrelated command
    /// never even reaches the (also-failing) currency check.
    #[test]
    fn a_command_rule_that_does_not_match_the_command_is_not_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        // COMMIT-POLICY.md deliberately never created either, so its
        // currency proof would fail too - but matching must be checked
        // FIRST, so an unrelated command must never even reach that check.
        let items = vec![command_item_with_check("cmd15", "git commit", "COMMIT-POLICY.md", "forbidden")];
        let command = "git push origin main";

        assert!(stale_in_command(&items, command, Some(dir.path())).is_empty());
    }

    // ------------------------------------------- command-anchor matching
    //
    // Direct, pure-function-level tests of `command_anchor_matches` - the
    // precision the CONTENT guard's other two arms get for free from
    // filesystem containment/full-path equality, but a command has neither,
    // so this comparison earns its own focused tests, mirroring how
    // `rank.rs`'s own `target_matches_is_the_only_definition_used_here` tests
    // `target_matches` directly, alongside `select`'s own higher-level tests.

    /// The realistic case, at realistic length: a heredoc-wrapped commit
    /// message (the exact shape this workspace's own commit instructions
    /// use) still matches a plain "git commit" anchor, because matching is a
    /// PREFIX of the command's own words, never an exact whole-string
    /// comparison.
    #[test]
    fn an_anchor_matches_a_long_heredoc_wrapped_commit_command() {
        let command = "git commit -m \"$(cat <<'EOF'\nSubject line with a forbidden word\n\nEOF\n)\"";
        assert!(command_anchor_matches("git commit", command));
    }

    #[test]
    fn an_anchor_matches_every_flag_variant_of_its_own_subcommand() {
        assert!(command_anchor_matches("git commit", "git commit -m \"fine\""));
        assert!(command_anchor_matches("git commit", "git commit --amend"));
        assert!(command_anchor_matches("git commit", "git commit"));
    }

    /// THE DEFECT THIS PREVENTS - the task's own worked concern: an anchor of
    /// "git commit" also fires on a DIFFERENT git subcommand, so a rule about
    /// committing fires on every git command.
    #[test]
    fn an_anchor_does_not_match_a_different_git_subcommand() {
        assert!(!command_anchor_matches("git commit", "git push origin main"));
        assert!(!command_anchor_matches("git commit", "git status"));
    }

    /// THE DEFECT THIS PREVENTS: a plain substring test
    /// (`command.contains(anchor)`) would wrongly treat "git commit-graph
    /// verify" as a match for the anchor "git commit" - `commit-graph` is one
    /// whole word, never the word `commit` followed by more text.
    #[test]
    fn an_anchor_does_not_match_a_lookalike_subcommand_sharing_a_text_prefix() {
        assert!(!command_anchor_matches("git commit", "git commit-graph verify"));
    }

    /// THE DEFECT THIS PREVENTS - the guard's own worst false-positive shape
    /// (see this module's own top-of-file doc comment, "THE COMMAND guard",
    /// and this crate's task report for the full reasoning): a command that
    /// merely MENTIONS a subcommand (inside an echo, a comment, a search
    /// pattern) must never match, only a command that actually RUNS it -
    /// matching is a PREFIX of the command's own words, never "the anchor's
    /// words appear anywhere in the command".
    #[test]
    fn an_anchor_does_not_match_when_its_words_appear_later_in_the_command_not_as_a_prefix() {
        assert!(!command_anchor_matches("git commit", "echo remember to git commit later"));
    }

    #[test]
    fn command_anchor_matching_is_case_insensitive() {
        assert!(command_anchor_matches("git commit", "GIT COMMIT -m \"fine\""));
        assert!(command_anchor_matches("GIT COMMIT", "git commit -m \"fine\""));
    }

    #[test]
    fn an_empty_anchor_never_matches_any_command() {
        assert!(!command_anchor_matches("", "git commit -m \"fine\""));
        assert!(!command_anchor_matches("", ""));
    }

    #[test]
    fn a_command_shorter_than_the_anchor_never_matches() {
        assert!(!command_anchor_matches("git commit", "git"));
        assert!(!command_anchor_matches("git commit", ""));
    }

    // ---------------------------- block, AbsentAll (set form), command-anchored

    /// THE DEFECT THIS PREVENTS: a set-carrying command-anchored rule never
    /// blocks at all, or blocks but reports the wrong literal when the FIRST
    /// one in the list is the one actually present.
    #[test]
    fn a_command_anchored_set_carrying_rule_blocks_on_the_first_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), b"# style\n").unwrap();
        let items = vec![command_item_with_absent_all_check(
            "cmd10",
            "git commit",
            "STYLE.md",
            &["forbidden-one", "forbidden-two", "forbidden-three"],
        )];
        let command = "git commit -m \"carries forbidden-one right here\"";

        let (reason, _anchor) = find_command_violation(&items, command, Some(dir.path())).expect("must block");
        assert!(reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-two"), "{reason}");
        assert!(!reason.contains("forbidden-three"), "{reason}");
    }

    /// The middle position: catches an implementation that only ever checks
    /// the first or the last literal.
    #[test]
    fn a_command_anchored_set_carrying_rule_blocks_on_a_middle_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), b"# style\n").unwrap();
        let items = vec![command_item_with_absent_all_check(
            "cmd11",
            "git commit",
            "STYLE.md",
            &["forbidden-one", "forbidden-two", "forbidden-three"],
        )];
        let command = "git commit -m \"carries forbidden-two right here\"";

        let (reason, _anchor) = find_command_violation(&items, command, Some(dir.path())).expect("must block");
        assert!(reason.contains("forbidden-two"), "{reason}");
        assert!(!reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-three"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS: a naive implementation that only ever checks
    /// the FIRST literal would miss a violation sitting in the last position.
    #[test]
    fn a_command_anchored_set_carrying_rule_blocks_on_the_last_literal_and_quotes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), b"# style\n").unwrap();
        let items = vec![command_item_with_absent_all_check(
            "cmd12",
            "git commit",
            "STYLE.md",
            &["forbidden-one", "forbidden-two", "forbidden-three"],
        )];
        let command = "git commit -m \"carries forbidden-three right here\"";

        let (reason, _anchor) = find_command_violation(&items, command, Some(dir.path())).expect("must block");
        assert!(reason.contains("forbidden-three"), "{reason}");
        assert!(!reason.contains("forbidden-one"), "{reason}");
        assert!(!reason.contains("forbidden-two"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS: a set-carrying command-anchored rule blocks
    /// even though NONE of its literals is actually present.
    #[test]
    fn a_command_anchored_set_carrying_rule_whose_literals_are_all_absent_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), b"# style\n").unwrap();
        let items = vec![command_item_with_absent_all_check(
            "cmd13",
            "git commit",
            "STYLE.md",
            &["forbidden-one", "forbidden-two"],
        )];
        let command = "git commit -m \"carries none of the forbidden words\"";

        assert_eq!(find_command_violation(&items, command, Some(dir.path())), None);
    }

    // ----------------------------------------------------- command payload

    /// THE DEFECT THIS PREVENTS: a tool call that never carried a "command"
    /// field at all (a Write, an Edit, any other ordinary tool) is treated as
    /// an empty command, or panics, instead of simply leaving the command
    /// guard with nothing to do - mirrors
    /// `an_unparseable_or_missing_payload_allows_silently` above for the
    /// content guard's own payload reading.
    #[test]
    fn a_tool_call_that_is_not_a_command_is_unaffected() {
        let write_input = serde_json::json!({ "file_path": "a.md", "content": "no command here" });
        assert_eq!(proposed_command(Some(&write_input)), None);

        assert_eq!(proposed_command(None), None);

        let wrong_type = serde_json::json!({ "command": 42 });
        assert_eq!(proposed_command(Some(&wrong_type)), None);
    }

    #[test]
    fn a_bash_style_payload_carries_its_command_through() {
        let bash_input = serde_json::json!({ "command": "git commit -m \"fine\"", "description": "commit" });
        assert_eq!(proposed_command(Some(&bash_input)), Some("git commit -m \"fine\""));
    }

    // -------------------------------------------------- staleness sidecar

    /// THE DEFECT THIS PREVENTS: a content rule that matched the file being
    /// written and carried a check whose currency could not be proven (the
    /// anchored file was deleted or renamed) walks away in total silence -
    /// the exact moment doctrine says is the most informative one to observe
    /// is lost forever instead of recorded. Mirrors
    /// `a_rule_whose_anchored_path_no_longer_exists_never_blocks` above for
    /// the "still allows" half, then proves the new "and is recorded" half
    /// on top of it.
    #[test]
    fn a_failing_currency_check_on_a_matching_content_rule_is_recorded_and_still_allows() {
        let dir = tempfile::tempdir().unwrap();
        // NOTES.md is deliberately never created, so the check's own anchor
        // cannot prove it is current.
        let ranked = vec![item_with_check("r1", "NOTES.md", "forbidden")];
        let content = "this text contains the forbidden word";

        assert_eq!(find_violation(&ranked, "NOTES.md", content, Some(dir.path())), None, "must still allow");

        let findings = stale_in_content(&ranked, "NOTES.md", Some(dir.path()));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "r1");
        assert_eq!(findings[0].outcome, StaleOutcome::Failed);
        assert_eq!(findings[0].file, "NOTES.md");
        assert!(findings[0].check.contains("Absent"), "{}", findings[0].check);
        assert!(findings[0].check.contains("NOTES.md"), "{}", findings[0].check);
    }

    /// The other negative outcome, kept distinct: a check whose own path
    /// escapes the root can never even be attempted, which is a different
    /// fact from "definitely not there any more" - both must still allow,
    /// but they must not collapse into the same recorded outcome.
    #[test]
    fn a_content_check_that_cannot_run_is_recorded_with_a_different_outcome_and_still_allows() {
        let dir = tempfile::tempdir().unwrap();
        let ranked = vec![item_with_check("r2", "../escape.txt", "forbidden")];
        let content = "this text contains the forbidden word";

        assert_eq!(find_violation(&ranked, "NOTES.md", content, Some(dir.path())), None, "must still allow");

        let findings = stale_in_content(&ranked, "whatever.md", Some(dir.path()));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "r2");
        assert_eq!(
            findings[0].outcome,
            StaleOutcome::CouldNotRun,
            "an escaping path proves nothing either way, which is a different outcome from Failed"
        );
    }

    /// The set-form counterpart to
    /// `a_failing_currency_check_on_a_matching_content_rule_is_recorded_and_
    /// still_allows`: a set-carrying rule whose check cannot prove its own
    /// currency must never block, and the silence must still be recorded -
    /// `absent_literals` (never a second, parallel currency path for
    /// `AbsentAll`) is what makes this fall through exactly like the plain
    /// `Absent` case does.
    #[test]
    fn a_set_carrying_rule_whose_check_cannot_run_does_not_block_and_is_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        // STYLE.md is deliberately never created, so the check's own anchor
        // cannot prove it is current.
        let ranked = vec![item_with_absent_all_check("r14", "STYLE.md", &["forbidden-one", "forbidden-two"])];
        let content = "this text carries forbidden-one";

        assert_eq!(find_violation(&ranked, "NOTES.md", content, Some(dir.path())), None, "must still allow");

        let findings = stale_in_content(&ranked, "STYLE.md", Some(dir.path()));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "r14");
        assert_eq!(findings[0].outcome, StaleOutcome::Failed);
        assert!(findings[0].check.contains("AbsentAll"), "{}", findings[0].check);
    }

    /// A rule whose currency HOLDS (and therefore blocks) must never also
    /// show up in the staleness sidecar - the two are mutually exclusive by
    /// construction, but worth pinning explicitly: a bug that recorded every
    /// candidate regardless of outcome would pass every test above
    /// undetected.
    #[test]
    fn a_holding_content_check_that_blocks_is_not_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NOTES.md"), "# notes\n").unwrap();
        let ranked = vec![item_with_check("r3", "NOTES.md", "forbidden")];
        let content = "the quick brown fox jumps over the forbidden fence";

        assert!(find_violation(&ranked, "NOTES.md", content, Some(dir.path())).is_some(), "sanity: this must actually block");
        assert!(stale_in_content(&ranked, "NOTES.md", Some(dir.path())).is_empty());
    }

    /// THE DEFECT THIS PREVENTS: a rule that is not even a candidate for the
    /// file actually being written still ends up in the staleness sidecar -
    /// proven, like `a_forbidden_literal_written_to_a_different_file_is_not_
    /// blocked` above, through the SAME real matching pipeline
    /// (`live::candidates_for` + `rank::select`) surface 2 itself uses, for a
    /// file the item is not bound to at all.
    #[test]
    fn a_content_rule_that_does_not_match_the_file_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        // NOTES.md deliberately never created either, so a bug that skipped
        // the matching step entirely (and jumped straight to a currency
        // check) would still be caught here.
        let mut db = thor_core::event_store::EventStore::in_memory().unwrap();
        let item = item_with_check("r4", "NOTES.md", "forbidden").item;
        model::store::declare(&mut db, "s", "l", "a", &item).unwrap();

        let mut input = crate::input::ServeInput::default();
        input.add_file("OTHER.md");
        let candidates = crate::live::candidates_for(&db, &input);
        let ranked = crate::rank::select(&candidates, &input);
        assert!(ranked.is_empty(), "fixture sanity: an unrelated file must not even match the binding");

        assert!(stale_in_content(&ranked, "OTHER.md", Some(dir.path())).is_empty());
    }

    /// THE DEFECT THIS PREVENTS: a rule that keeps failing to prove its own
    /// currency accumulates a new duplicate row every time instead of one
    /// row whose count grows - which would make the sidecar an ever-growing
    /// log instead of a small, readable ledger keyed by id.
    #[test]
    fn a_second_stale_occurrence_increments_the_count_instead_of_duplicating() {
        let finding = StaleFinding {
            id: "r1".to_string(),
            outcome: StaleOutcome::Failed,
            check: "Absent { path: \"NOTES.md\", literal: \"forbidden\" }".to_string(),
            file: "NOTES.md".to_string(),
        };
        let seen = vec![finding.id.clone()];
        let text1 = record_stale_text(None, std::slice::from_ref(&finding), &seen, 5).unwrap();
        let text2 = record_stale_text(Some(&text1), std::slice::from_ref(&finding), &seen, 9).unwrap();

        let records = read_stale(&text2);
        assert_eq!(records.len(), 1, "must stay one row, never a duplicate");
        let record = records.get("r1").unwrap();
        assert_eq!(record.count, 2);
        assert_eq!(record.outcome, StaleOutcome::Failed);
        assert_eq!(
            record.seq_at_record, 9,
            "seq_at_record must reflect the MOST RECENT occurrence, like outcome/check/file already do"
        );
    }

    /// Fail-open: a sidecar file that is missing or malformed reads back as
    /// an empty ledger, never an error - the same stance every marker file
    /// in this crate takes.
    #[test]
    fn a_missing_or_corrupt_stale_sidecar_reads_as_empty() {
        assert!(read_stale("").is_empty());
        assert!(read_stale("{ not json").is_empty());
    }

    /// THE DEFECT THIS PREVENTS: a sidecar written before `seq_at_record`
    /// existed fails to parse at all (or panics) once this field is added -
    /// backward compatibility for every existing on-disk sidecar. The
    /// missing field must read back as 0 ("since the beginning of the log"),
    /// never an error - see `StaleRecord::seq_at_record`'s own doc comment
    /// for why that default is the safe, fail-open direction.
    #[test]
    fn a_stale_record_written_before_seq_at_record_existed_still_parses_and_defaults_to_zero() {
        let legacy = r#"{"r1":{"outcome":"failed","check":"Absent","file":"NOTES.md","count":1}}"#;
        let records = read_stale(legacy);
        assert_eq!(records.get("r1").unwrap().seq_at_record, 0);
    }

    /// The location guard's own mirror of
    /// `a_failing_currency_check_on_a_matching_content_rule_is_recorded_and_still_allows`:
    /// a write landing inside a directory a rule protects, whose protected
    /// directory was itself deleted or renamed, is recorded - the exact
    /// moment `a_rule_whose_protected_path_no_longer_exists_never_blocks`
    /// above proves stays silent on the BLOCK side.
    #[test]
    fn a_failing_currency_check_on_a_matching_location_rule_is_recorded_and_still_allows() {
        let dir = tempfile::tempdir().unwrap();
        // "frozen" is deliberately never created.
        let items = vec![location_item("loc1", TargetKind::Dir, "frozen")];

        assert_eq!(find_location_violation(&items, "frozen/cli.rs", Some(dir.path())), None, "must still allow");

        let findings = stale_in_location(&items, "frozen/cli.rs", Some(dir.path()));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "loc1");
        assert_eq!(findings[0].outcome, StaleOutcome::Failed);
        assert_eq!(findings[0].file, "frozen/cli.rs");
        assert!(findings[0].check.contains("PathExists"), "{}", findings[0].check);
    }

    /// The location guard's own mirror of
    /// `a_holding_content_check_that_blocks_is_not_recorded_as_stale`.
    #[test]
    fn a_holding_location_check_that_blocks_is_not_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("frozen")).unwrap();
        let items = vec![location_item("loc2", TargetKind::Dir, "frozen")];

        assert!(find_location_violation(&items, "frozen/cli.rs", Some(dir.path())).is_some(), "sanity: must block");
        assert!(stale_in_location(&items, "frozen/cli.rs", Some(dir.path())).is_empty());
    }

    /// THE DEFECT THIS PREVENTS: the location guard's own candidate pool is
    /// fetched WITHOUT narrowing by the actual file value (see this module's
    /// own top-of-file doc comment - only the target KIND narrows it), so
    /// skipping the containment check inside `stale_in_location` would flag
    /// every stale, Irreversible location rule in the whole store on every
    /// single write anywhere, however unrelated - noise, not observation.
    #[test]
    fn a_location_rule_that_does_not_contain_the_file_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        // "frozen" is deliberately never created, so its currency proof
        // would fail too - but containment must be checked FIRST, so an
        // unrelated file must never even reach that check.
        let items = vec![location_item("loc3", TargetKind::Dir, "frozen")];

        assert!(stale_in_location(&items, "elsewhere/cli.rs", Some(dir.path())).is_empty());
    }

    /// The `Dir`-anchored content guard's own mirror of
    /// `a_holding_content_check_that_blocks_is_not_recorded_as_stale`/
    /// `a_holding_location_check_that_blocks_is_not_recorded_as_stale`: a
    /// rule whose currency HOLDS (and therefore blocks) must never also show
    /// up in the staleness sidecar.
    #[test]
    fn a_holding_dir_anchored_check_that_blocks_is_not_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("thor2")).unwrap();
        let items = vec![dir_item_with_check("d7", "thor2", "forbidden")];
        let content = "a line carrying the forbidden word";

        assert!(
            find_dir_content_violation(&items, "thor2/probe-scratch.md", content, Some(dir.path())).is_some(),
            "sanity: this must actually block"
        );
        assert!(stale_in_dir_content(&items, "thor2/probe-scratch.md", Some(dir.path())).is_empty());
    }

    /// THE DEFECT THIS PREVENTS: the staleness scan must check CONTAINMENT
    /// before CURRENCY (the same deliberately-reversed order
    /// `stale_in_location`'s own
    /// `a_location_rule_that_does_not_contain_the_file_is_not_recorded` test
    /// pins, mirrored here) - a file outside the protected directory must
    /// never be recorded, even though the directory's own currency proof
    /// would also fail.
    #[test]
    fn a_dir_anchored_rule_that_does_not_contain_the_file_is_not_recorded_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        // "thor2" is deliberately never created, so its currency proof
        // would fail too - but containment must be checked FIRST, so an
        // unrelated file must never even reach that check.
        let items = vec![dir_item_with_check("d8", "thor2", "forbidden")];

        assert!(stale_in_dir_content(&items, "elsewhere/probe-scratch.md", Some(dir.path())).is_empty());
    }
}
