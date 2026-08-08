//! Read-only census: of every live Rule/Orientation carrying a machine-
//! runnable `Check::Contains { path, literal }`, how many of those checks are
//! HOLLOW - built so they can never actually catch a violation, which makes
//! them worse than no check at all, because a hollow check still reads as
//! "provable" on `doctor`'s own `provable rules` line (see `ops::health::
//! proof_line`) while blocking nothing. Measured on a real repository: of 127
//! checks none failed and no path was dead, but 20 were hollow - 11 where the
//! literal is a word lifted straight from the FILENAME (a `path_exists` in
//! disguise), 5 where the literal is six characters or fewer, 3 where the
//! literal is common enough to hit 25+ times, and 3 where the literal exists
//! ONLY inside a comment, never in the real code a person would actually
//! change. This tool answers the same question for a whole store at once.
//!
//! SCOPE - only `Check::Contains`. `PathExists`, `Absent`, `AbsentAll` and
//! `Forbidden` are different shapes of proof with different failure modes
//! (see `model::item::Check`'s own doc comment) and are not this tool's
//! business. `serve/examples/check_census.rs` already answers Holds/Fails/
//! CannotRun for every check kind at once; this tool never repeats that
//! question - see "WHAT THIS DOES NOT MEASURE" below for exactly how the two
//! relate. Only a live item whose kind can fire (`Kind::can_fire`: Rule or
//! Orientation) is ever counted - the same restriction `check_census` and
//! `stale_anchors` both place on which items matter here, since a Report/
//! Lookup/Chunk may never carry a check at all (`gate::declare`, ground 12).
//!
//! Read-only throughout. CHECK_LINT_DB must point at a COPY of a store, never
//! the live one: opened through `EventStore::open_existing`, the same
//! constructor `stale_anchors`/`dead_anchors`/`check_census` and the
//! inspection commands (doctor, fsck, status) all use, precisely because it
//! does no schema work and no FTS heal on open (see that constructor's own
//! doc comment in `core/src/event_store.rs`). Every store read after that
//! (`serve::live::live_items`) is a SELECT-only reader too. Every filesystem
//! read below is also read-only: this never writes a byte to any checkout,
//! and every file read is bounded by `model::check::MAX_CHECK_FILE_BYTES`
//! (1 MiB), the exact same bound `model::check::run` itself enforces.
//!
//! PROJECT RESOLUTION - CHECK_LINT_CHECKOUTS must point at a directory of
//! per-project checkouts, resolved EXACTLY like `ops::health::decay_line` and
//! `serve/examples/dead_anchors.rs`: every immediate subdirectory is handed
//! to `serve::project::resolve_project`, and whatever project key that
//! returns becomes the checkout a same-named item's relative paths resolve
//! against. This is deliberately the same resolution, reused rather than
//! reinvented - a second, slightly different definition of "which checkout
//! does this project key mean" is exactly the kind of drift this workspace's
//! own single-definition tests (`serve/tests/single_project_resolution.rs`)
//! exist to catch elsewhere, and this tool has no business inventing a
//! second copy of that same idea just because it lives in a different crate.
//! An item with no project at all, or whose project resolves to no checkout
//! under CHECK_LINT_CHECKOUTS, is reported as UNRESOLVED, never guessed at
//! and never silently dropped - see "UNRESOLVED" below. A directory that
//! cannot be read at all (a bad CHECK_LINT_CHECKOUTS path) degrades to an
//! empty set of checkouts rather than an error, the same fails-open stance
//! `dead_anchors.rs` takes on the identical read.
//!
//! Neither variable falls back to a default path. Either one missing and
//! this prints why and exits before opening anything - no store open, no
//! checkout walked, no file read.
//!
//! HOW A CHECK IS ACTUALLY READ - never a second path-escape guard. This
//! tool calls `model::check::run` first, on the item's real `Check::Contains`
//! value, against the resolved project checkout - the exact function that
//! already refuses a `..`, an absolute path, or a path that only escapes the
//! checkout through a symlink/junction planted inside it (see that
//! function's own doc comment, and its private `resolve_within_root`/
//! `escapes_root` helpers). Only `Outcome::CannotRun` is treated specially
//! here (folded into UNRESOLVED, below); `Holds` and `Fails` both mean the
//! file was read successfully, so this tool then reads the same bytes a
//! second time itself, bounded the identical way, to get the text its own
//! classifier needs. `resolve_within_root`/`escapes_root` are private to
//! `model::check` and out of this change's scope to make public - this file
//! creates nothing else and edits nothing else, so the extra bounded read is
//! the honest cost of reusing the security-relevant guard instead of copying
//! it a second time.
//!
//! WHAT THIS DOES NOT MEASURE. Whether a check currently `Holds` or `Fails`
//! (`check_census`'s own question) is a DIFFERENT question from whether it
//! COULD EVER fail, which is what hollowness means here - a check that is
//! failing right now is thereby proven NOT hollow (the strongest possible
//! evidence a check can produce), so this tool folds that case into
//! `healthy` without further comment; see HEALTHY below. This also never
//! repairs, revises, retracts or writes anything to the store - the JSON
//! file it writes is detail for a LATER repair pass to read, not a plan this
//! tool carries out itself.
//!
//! CLASSIFICATION - checked in this exact fixed order, first match wins, so
//! a check that happens to fit more than one weak shape (a literal that is
//! BOTH a filename token AND six characters or fewer, e.g. "BACKUP" against
//! BACKUP.md) is still counted exactly once, under whichever class is
//! checked first - the same "first match wins" discipline `stale_anchors.rs`
//! uses for its own seven anchor classes, and `model::anchor_shape::
//! unmatchable` for its three:
//!
//!   1. `filename-token` - the literal, compared case-insensitively, equals
//!      one WHOLE token of the target path's own file stem (extension
//!      stripped, then split on every non-alphanumeric character). "RELEASE"
//!      inside RELEASE-CHECKLIST.md, or "BACKUP" inside BACKUP.md: the check
//!      cannot ever fail while the file simply exists under that name, so it
//!      is a `PathExists` wearing a `Contains` costume.
//!   2. `too-short` - the literal is six characters or fewer, counted as
//!      Unicode scalar values, never bytes. Short enough that it is
//!      plausible almost anywhere in a real file by pure chance, proving
//!      little about the specific fact it claims to guard.
//!   3. `too-common` - the literal occurs 25 or more times in the file (a
//!      plain, non-overlapping substring count). A literal repeated that
//!      often is background noise, not a specific claim - it would keep
//!      matching through almost any unrelated edit to the file.
//!   4. `comment-only` - EVERY occurrence of the literal in the file sits
//!      inside something this tool's own textual heuristic considers a
//!      comment; see "COMMENT-ONLY DETECTION" below for exactly what that
//!      does and does not catch, in both directions. The most dangerous
//!      shape of the four: a check like this can hold indefinitely while the
//!      real implementation it claims to describe drifts arbitrarily far
//!      away, because the fact never once has to agree with working code.
//!   5. `healthy` - none of the four above applied. Not a certificate that
//!      the check is well-chosen, only that this tool's four specific
//!      hollowness patterns did not fire - see HEALTHY below for exactly
//!      what that does and does not promise.
//!
//! COMMENT-ONLY DETECTION - read this before trusting that number. This is
//! deliberately NOT a parser for any language: it never tokenises, never
//! understands a string literal, never knows which language a file is
//! written in. Two purely textual passes decide "is this byte offset inside
//! a comment", and an occurrence of the literal counts as commented only if
//! its start position falls inside a span EITHER pass produced:
//!
//!   - LINE COMMENTS: `//`, `#` and `--`, tried per physical line - the
//!     EARLIEST of the three that appears (if any) wins, and everything from
//!     there to the end of that line counts as commented.
//!   - BLOCK COMMENTS: `/* .. */` and `<!-- .. -->`, first opening marker to
//!     the next closing marker (or to end of file when no closing marker
//!     ever appears), scanned independently of line boundaries.
//!
//! WHERE THIS OVER-DETECTS (marks real, non-comment text as commented, which
//! can misclassify a genuinely healthy check as `comment-only`):
//!   - Neither pass knows about string literals. A `//` inside a URL
//!     (`"http://example.com"`), a `#` inside almost any string, or a plain
//!     double hyphen that is not a comment at all (a CLI flag like
//!     `--release`, `--features`, this very workspace's own convention)
//!     still starts a "comment" that swallows the rest of that physical
//!     line - including a real, non-comment occurrence of the literal
//!     sitting later on the same line.
//!   - `#` is treated as a line comment regardless of language. In Rust
//!     source specifically (most of what this workspace's own checks are
//!     likely to be pointed at) this is wrong for an attribute line -
//!     `#[derive(Debug)]`, `#[cfg(test)]` - and in C it is wrong for a
//!     preprocessor directive (`#define`, `#include`); neither is a comment,
//!     both get swept in as one anyway.
//!   - Nesting is not modelled. Rust block comments really do nest
//!     (`/* outer /* inner */ still outer */`); this scanner still closes at
//!     the FIRST `*/`, so it marks less than the true nested comment, not
//!     more - see the under-detection case directly below for what that
//!     costs the other way.
//!
//! WHERE THIS UNDER-DETECTS (fails to mark real comment text as commented,
//! which can let a genuinely hollow `comment-only` check read as `healthy`
//! or another class instead):
//!   - Any comment style outside the two forms above is invisible to this
//!     tool: a Python triple-quoted string used as a comment, a Lua
//!     `--[[ .. ]]` block, a PowerShell `<# .. #>` block, a Bash heredoc. A
//!     literal that appears ONLY inside one of these reads as ordinary code.
//!   - The nesting gap above cuts both ways: text between an inner `*/` and
//!     the real outer `*/` is lexically still inside the block comment, but
//!     this scanner reads it as plain code once the first `*/` closes the
//!     span early - an occurrence sitting in exactly that gap is treated as
//!     NOT commented, when a real parser would say it is.
//!
//! HEALTHY - the residual bucket, not a certificate. It includes a check
//! that is presently `Fails` (the literal is not in the file at all right
//! now), precisely because a check that can and does fail has already proven
//! it is not hollow. It does NOT mean this tool re-verified the check
//! currently `Holds` (see WHAT THIS DOES NOT MEASURE above, and run
//! `check_census` for that separate question), and it does not mean no OTHER
//! kind of hollowness exists beyond the four this tool specifically looks
//! for.
//!
//! UNRESOLVED - reported on its own, never folded into `healthy` and never
//! forced into one of the five classes above, mirroring `model::check::
//! Outcome::CannotRun`'s own doctrine that "could not be checked" is a real,
//! distinct answer from every other one. Three reasons, each counted
//! separately:
//!   - `no-project` - the item names no project at all, so there is no
//!     checkout to resolve its relative path against.
//!   - `no-checkout` - the item names a project, but no directory under
//!     CHECK_LINT_CHECKOUTS resolves to that same project key (see
//!     `ops::health::orphan_projects_line` for the standing inventory of
//!     exactly this situation).
//!   - `cannot-run` - a project and a checkout both resolved, but
//!     `model::check::run` still came back `CannotRun`: the file is missing
//!     under that checkout, is not valid UTF-8, exceeds
//!     `model::check::MAX_CHECK_FILE_BYTES`, or the path escapes the
//!     checkout entirely (see that function's own doc comment for the
//!     complete list).
//!
//! Writes the full detail - every checked item, healthy or not, plus every
//! unresolved one - to check-lint-report.json in the current working
//! directory, for a later repair pass to read; prints a summary count per
//! class plus the weak items (everything but `healthy`) to stdout, grouped
//! by class, for a person to act on right now.
//!
//! Run (from the `thor2` workspace root):
//!   CHECK_LINT_DB=<path to a COPY of a store> \
//!   CHECK_LINT_CHECKOUTS=<path to a directory of per-project checkouts> \
//!   cargo run --release -p serve --example check_lint

use model::check::{run, Outcome, MAX_CHECK_FILE_BYTES};
use model::item::Check;
use serve::live::live_items;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

/// The five ways a `Check::Contains` literal can relate to the file it
/// claims to guard - see this file's own "CLASSIFICATION" doc-comment
/// section for the fixed order these are tried in and exactly what each one
/// means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LintClass {
    FilenameToken,
    TooShort,
    TooCommon,
    CommentOnly,
    Healthy,
}

impl LintClass {
    /// Every class, in the fixed order they are checked in and printed in -
    /// spelled out explicitly rather than derived from a HashMap, so output
    /// order never depends on hashing (the same reasoning `AnchorClass::ALL`
    /// in `stale_anchors.rs` gives for its own array).
    const ALL: [LintClass; 5] = [
        LintClass::FilenameToken,
        LintClass::TooShort,
        LintClass::TooCommon,
        LintClass::CommentOnly,
        LintClass::Healthy,
    ];

    fn label(self) -> &'static str {
        match self {
            LintClass::FilenameToken => "filename-token",
            LintClass::TooShort => "too-short",
            LintClass::TooCommon => "too-common",
            LintClass::CommentOnly => "comment-only",
            LintClass::Healthy => "healthy",
        }
    }
}

/// Why a `Check::Contains` could not be classified at all - see this file's
/// own "UNRESOLVED" doc-comment section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Unresolved {
    NoProject,
    NoCheckout,
    CannotRun,
}

impl Unresolved {
    /// Every reason, in the fixed order they are printed in - mirrors
    /// `LintClass::ALL` for the identical reason: deterministic output,
    /// never hashmap order.
    const ALL: [Unresolved; 3] = [Unresolved::NoProject, Unresolved::NoCheckout, Unresolved::CannotRun];

    fn label(self) -> &'static str {
        match self {
            Unresolved::NoProject => "no-project",
            Unresolved::NoCheckout => "no-checkout",
            Unresolved::CannotRun => "cannot-run",
        }
    }
}

/// A literal this short is plausible almost anywhere by pure chance - see
/// the "too-short" bullet in this file's own "CLASSIFICATION" doc comment.
/// Matches the measured example this tool was built from ("INSERT", six
/// characters, matched 7 times in a file including inside "INSERT OR
/// REPLACE" and guarded nothing).
const TOO_SHORT_MAX_CHARS: usize = 6;

/// A literal occurring at least this many times is background noise, not a
/// specific claim - see the "too-common" bullet above. Matches the measured
/// example ("setTimeout", 35 hits).
const TOO_COMMON_MIN_HITS: usize = 25;

/// `stem` split on every non-ASCII-alphanumeric character, uppercased - the
/// tokens a filename is considered to be "made of" for `is_filename_token`
/// below. `"RELEASE-CHECKLIST"` becomes `["RELEASE", "CHECKLIST"]`;
/// `"BACKUP"` becomes `["BACKUP"]`.
fn filename_tokens(stem: &str) -> Vec<String> {
    stem.split(|c: char| !c.is_ascii_alphanumeric()).filter(|token| !token.is_empty()).map(str::to_uppercase).collect()
}

/// Whether `literal`, compared case-insensitively, equals one WHOLE token of
/// `path`'s own file stem (its file name with the last extension removed) -
/// never a substring match, only an exact token match, so "RELEASE" matches
/// the stem "RELEASE-CHECKLIST" but does not match a stem "PRERELEASE-NOTES"
/// (see this file's own "WHERE THIS CAN BE WRONG" note below).
///
/// WHERE THIS CAN BE WRONG, both directions: a literal that is only a
/// SUBSTRING of a filename token (not equal to one) is never caught here -
/// under-detection, a genuinely filename-derived check can read as another
/// class instead. A literal that happens to equal a filename token purely by
/// coincidence, with no real causal link to the filename (a literal "TERM"
/// checked inside GLOSSARY-TERM.md, where "TERM" is meaningful prose, not
/// merely present because it is the filename) is still flagged here -
/// over-detection, since this heuristic has no way to tell "the literal is
/// there because it is the filename" apart from "the literal is there for
/// its own reason and happens to equal a filename token".
fn is_filename_token(path: &str, literal: &str) -> bool {
    let needle = literal.trim().to_uppercase();
    if needle.is_empty() {
        return false;
    }
    let file_name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path);
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or(file_name);
    filename_tokens(stem).iter().any(|token| *token == needle)
}

/// A half-open byte range `[start, end)` into a file's content that this
/// heuristic considers "inside a comment". See this file's own "COMMENT-ONLY
/// DETECTION" doc-comment section for exactly what these mean and where they
/// are wrong, in both directions.
type Span = (usize, usize);

/// `/* .. */` and `<!-- .. -->` spans: first opening marker to the next
/// closing marker, or to end of file when no closing marker ever appears.
/// Scanned independently for each marker pair and independently of line
/// boundaries. Does not model nesting - the first closing marker ends the
/// span, even inside a real nested Rust block comment (see "WHERE THIS
/// OVER-DETECTS"/"WHERE THIS UNDER-DETECTS" in this file's own doc comment).
fn block_comment_spans(content: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    for (open, close) in [("/*", "*/"), ("<!--", "-->")] {
        let mut search_from = 0usize;
        while let Some(rel_start) = content[search_from..].find(open) {
            let start = search_from + rel_start;
            let after_open = start + open.len();
            match content[after_open..].find(close) {
                Some(rel_end) => {
                    let end = after_open + rel_end + close.len();
                    spans.push((start, end));
                    search_from = end;
                }
                None => {
                    spans.push((start, content.len()));
                    break;
                }
            }
        }
    }
    spans
}

/// `//`, `#` and `--` spans: for each physical line, the EARLIEST of the
/// three markers that appears (if any) starts a span running to the end of
/// that line. Neither a string literal nor the target language's own syntax
/// is understood - see this file's own "WHERE THIS OVER-DETECTS" doc-comment
/// section for exactly what that misreads, and in which direction.
fn line_comment_spans(content: &str) -> Vec<Span> {
    const MARKERS: [&str; 3] = ["//", "#", "--"];
    let mut spans = Vec::new();
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        let earliest = MARKERS.iter().filter_map(|marker| line.find(marker)).min();
        if let Some(rel) = earliest {
            spans.push((offset + rel, offset + line.len()));
        }
        offset += line.len();
    }
    spans
}

/// Every span either pass considers a comment, unmerged and in no particular
/// order - `occurs_in_comment` below only ever asks "is this one offset
/// covered by any span", so nothing here needs the spans sorted or merged.
fn comment_spans(content: &str) -> Vec<Span> {
    let mut spans = block_comment_spans(content);
    spans.extend(line_comment_spans(content));
    spans
}

fn occurs_in_comment(spans: &[Span], start: usize) -> bool {
    spans.iter().any(|&(s, e)| start >= s && start < e)
}

/// How many times a literal occurs in a file's content, and whether every
/// single one of those occurrences sits inside a comment span. `all_in_
/// comments` is `false` when `hits` is zero - a literal that never occurs at
/// all is not "comment-only", it simply is not there right now (see this
/// file's own "HEALTHY" doc-comment section for what a zero-hit, currently-
/// failing check means for classification).
#[derive(Debug)]
struct Occurrences {
    hits: usize,
    all_in_comments: bool,
}

fn scan_occurrences(content: &str, literal: &str) -> Occurrences {
    if literal.is_empty() {
        return Occurrences { hits: 0, all_in_comments: false };
    }
    let spans = comment_spans(content);
    let mut hits = 0usize;
    let mut all_in_comments = true;
    for (start, _) in content.match_indices(literal) {
        hits += 1;
        if !occurs_in_comment(&spans, start) {
            all_in_comments = false;
        }
    }
    Occurrences { hits, all_in_comments: hits > 0 && all_in_comments }
}

/// Classify one `Check::Contains { path, literal }`, already confirmed
/// readable, against its own file content - see this file's own
/// "CLASSIFICATION" doc-comment section for the fixed order and exactly what
/// each class means.
fn classify(path: &str, literal: &str, occurrences: &Occurrences) -> LintClass {
    if is_filename_token(path, literal) {
        return LintClass::FilenameToken;
    }
    if literal.chars().count() <= TOO_SHORT_MAX_CHARS {
        return LintClass::TooShort;
    }
    if occurrences.hits >= TOO_COMMON_MIN_HITS {
        return LintClass::TooCommon;
    }
    if occurrences.all_in_comments {
        return LintClass::CommentOnly;
    }
    LintClass::Healthy
}

/// Read `path` as a UTF-8 string, provided it exists, is a regular file, and
/// is at most `MAX_CHECK_FILE_BYTES` - the identical bound `model::check::
/// run` enforces internally. `model::check::run` already proved (by
/// returning something other than `CannotRun`) that this exact file reads
/// cleanly under that same bound; this is a second, independent read of the
/// same bytes so this tool has the actual text to search, never a shortcut
/// that skips the bound a second time.
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

/// `s` with every newline and carriage return collapsed to a single space -
/// applied to a literal or an item's own text, neither of which this tool
/// controls the shape of, so either can never break the one-row-per-item
/// shape of this tool's output. An example cannot share code with another
/// example, only with the library crates, so this restates `check_census.
/// rs`'s own `one_line` rather than drifting from it silently.
fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// The first 80 characters of `text`, counted in Unicode scalar values
/// (never bytes), with embedded newlines collapsed the same way `one_line`
/// collapses them in a literal. Mirrors `check_census.rs`'s own `snippet`.
fn snippet(text: &str) -> String {
    one_line(&text.chars().take(80).collect::<String>())
}

/// One `Check::Contains` this tool successfully classified - held only long
/// enough to be printed and written to JSON at the end of `main`.
struct CheckedRow {
    id: String,
    project: String,
    path: String,
    literal: String,
    class: LintClass,
    hits: usize,
    text: String,
}

/// One `Check::Contains` this tool could not classify at all - see this
/// file's own "UNRESOLVED" doc-comment section.
struct UnresolvedRow {
    id: String,
    project: Option<String>,
    path: String,
    literal: String,
    reason: Unresolved,
}

fn checked_row_json(row: &CheckedRow) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "project": row.project,
        "path": row.path,
        "literal": row.literal,
        "class": row.class.label(),
        "hits": row.hits,
        "text": row.text,
    })
}

fn unresolved_row_json(row: &UnresolvedRow) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "project": row.project,
        "path": row.path,
        "literal": row.literal,
        "reason": row.reason.label(),
    })
}

fn main() -> anyhow::Result<()> {
    let db_var = std::env::var("CHECK_LINT_DB").ok();
    let checkouts_var = std::env::var("CHECK_LINT_CHECKOUTS").ok();
    let (Some(db), Some(checkouts)) = (db_var, checkouts_var) else {
        eprintln!(
            "check_lint requires two environment variables, neither with a default:\n\
             CHECK_LINT_DB         - path to a COPY of a store (never the live thor.db)\n\
             CHECK_LINT_CHECKOUTS  - path to a directory of per-project checkouts, resolved \
             exactly like ops::health::decay_line (see this file's own doc comment)\n\
             At least one is unset, so this exits now: no store has been opened, nothing has \
             been read."
        );
        std::process::exit(1);
    };
    let db = PathBuf::from(db);
    let checkouts = PathBuf::from(checkouts);

    let store = EventStore::open_existing(&db)?;

    // Same per-project checkout resolution as ops::health::decay_line and
    // serve/examples/dead_anchors.rs - see this file's own "PROJECT
    // RESOLUTION" doc comment. A checkouts directory that cannot be read at
    // all degrades to an empty map rather than an error, the same fails-open
    // stance dead_anchors.rs takes on this identical read.
    let mut roots: BTreeMap<String, PathBuf> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(&checkouts) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    roots.insert(key, path);
                }
            }
        }
    }

    let mut fireable_total = 0usize;
    let mut with_check_total = 0usize;
    let mut contains_total = 0usize;

    let mut class_counts: HashMap<LintClass, usize> = HashMap::new();
    let mut unresolved_counts: HashMap<Unresolved, usize> = HashMap::new();
    let mut checked: Vec<CheckedRow> = Vec::new();
    let mut unresolved: Vec<UnresolvedRow> = Vec::new();

    for live in live_items(&store) {
        let id = live.id;
        let item = live.item;
        if !item.kind.can_fire() {
            continue; // only Rule/Orientation can ever carry a check (gate::declare ground 12)
        }
        fireable_total += 1;
        let Some(check) = item.check else { continue };
        with_check_total += 1;
        let Check::Contains { path, literal } = &check else { continue };
        contains_total += 1;

        let Some(project) = item.project.clone() else {
            *unresolved_counts.entry(Unresolved::NoProject).or_insert(0) += 1;
            unresolved.push(UnresolvedRow {
                id,
                project: None,
                path: path.clone(),
                literal: literal.clone(),
                reason: Unresolved::NoProject,
            });
            continue;
        };
        let Some(base) = roots.get(&project) else {
            *unresolved_counts.entry(Unresolved::NoCheckout).or_insert(0) += 1;
            unresolved.push(UnresolvedRow {
                id,
                project: Some(project),
                path: path.clone(),
                literal: literal.clone(),
                reason: Unresolved::NoCheckout,
            });
            continue;
        };

        if run(&check, base) == Outcome::CannotRun {
            *unresolved_counts.entry(Unresolved::CannotRun).or_insert(0) += 1;
            unresolved.push(UnresolvedRow {
                id,
                project: Some(project),
                path: path.clone(),
                literal: literal.clone(),
                reason: Unresolved::CannotRun,
            });
            continue;
        }

        // model::check::run just proved this path resolves inside base and
        // reads cleanly - see this file's own "HOW A CHECK IS ACTUALLY READ"
        // doc comment for why this reads the bytes a second time itself
        // rather than reimplementing that function's own escape guard.
        let full = base.join(path.replace('\\', "/"));
        let Some(content) = read_bounded_utf8(&full) else {
            // Only a race (the file changed between the two reads) can land
            // here. Never guessed at: the same bucket as any other "could
            // not be read" reason.
            *unresolved_counts.entry(Unresolved::CannotRun).or_insert(0) += 1;
            unresolved.push(UnresolvedRow {
                id,
                project: Some(project),
                path: path.clone(),
                literal: literal.clone(),
                reason: Unresolved::CannotRun,
            });
            continue;
        };

        let occurrences = scan_occurrences(&content, literal);
        let class = classify(path, literal, &occurrences);
        *class_counts.entry(class).or_insert(0) += 1;
        checked.push(CheckedRow {
            id,
            project,
            path: path.clone(),
            literal: literal.clone(),
            class,
            hits: occurrences.hits,
            text: item.text.clone(),
        });
    }

    // ---------------------------------------------------------------- print

    println!("live fireable items: {fireable_total}");
    println!("carry a check: {with_check_total}");
    println!("carry a Contains check (this tool's scope): {contains_total}");
    println!(
        "other check kinds (path_exists/absent/absent_all/forbidden - out of this tool's scope): {}",
        with_check_total - contains_total
    );
    println!();

    println!("classified: {}", checked.len());
    println!("unresolved (could not be classified, never guessed at): {}", unresolved.len());
    for reason in Unresolved::ALL {
        println!("  {}: {}", reason.label(), unresolved_counts.get(&reason).copied().unwrap_or(0));
    }
    println!();

    for class in LintClass::ALL {
        println!("{}: {}", class.label(), class_counts.get(&class).copied().unwrap_or(0));
    }
    println!();
    println!(
        "healthy means none of the four hollow patterns above fired - not a certificate the \
         check is well-chosen, and not the same question check_census answers (whether the \
         check currently holds). See this file's own doc comment for exactly what each class \
         does and does not promise, especially COMMENT-ONLY DETECTION."
    );

    println!();
    println!("weak items (everything but healthy), grouped by class:");
    for class in LintClass::ALL {
        if class == LintClass::Healthy {
            continue;
        }
        let rows: Vec<&CheckedRow> = checked.iter().filter(|row| row.class == class).collect();
        println!();
        println!("=== {} ({}) ===", class.label(), rows.len());
        for row in rows {
            println!("id={} project={} path={}", row.id, row.project, row.path);
            println!("  literal: \"{}\"", one_line(&row.literal));
            println!("  hits: {}", row.hits);
            println!("  text:  {}", snippet(&row.text));
        }
    }

    // ----------------------------------------------------------------- json

    let checked_json: Vec<serde_json::Value> = checked.iter().map(checked_row_json).collect();
    let unresolved_json: Vec<serde_json::Value> = unresolved.iter().map(unresolved_row_json).collect();
    let report = serde_json::json!({
        "summary": {
            "fireable_total": fireable_total,
            "with_check_total": with_check_total,
            "contains_total": contains_total,
            "classified": {
                "filename_token": class_counts.get(&LintClass::FilenameToken).copied().unwrap_or(0),
                "too_short": class_counts.get(&LintClass::TooShort).copied().unwrap_or(0),
                "too_common": class_counts.get(&LintClass::TooCommon).copied().unwrap_or(0),
                "comment_only": class_counts.get(&LintClass::CommentOnly).copied().unwrap_or(0),
                "healthy": class_counts.get(&LintClass::Healthy).copied().unwrap_or(0),
            },
            "unresolved": {
                "no_project": unresolved_counts.get(&Unresolved::NoProject).copied().unwrap_or(0),
                "no_checkout": unresolved_counts.get(&Unresolved::NoCheckout).copied().unwrap_or(0),
                "cannot_run": unresolved_counts.get(&Unresolved::CannotRun).copied().unwrap_or(0),
            },
        },
        "checked": checked_json,
        "unresolved": unresolved_json,
    });
    std::fs::write("check-lint-report.json", serde_json::to_string_pretty(&report)?)?;
    println!();
    println!("wrote check-lint-report.json ({} checked, {} unresolved)", checked.len(), unresolved.len());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------- filename-token

    #[test]
    fn a_literal_equal_to_a_hyphenated_filename_token_is_a_filename_token() {
        assert!(is_filename_token("docs/RELEASE-CHECKLIST.md", "RELEASE"));
    }

    #[test]
    fn a_literal_equal_to_the_whole_stem_is_a_filename_token() {
        assert!(is_filename_token("BACKUP.md", "BACKUP"));
    }

    #[test]
    fn a_literal_that_is_only_a_substring_of_a_token_is_not_a_filename_token() {
        // Documented gap: "RELEASE" is a substring of "PRERELEASE" but not
        // equal to it - only a whole-token match counts, never a substring.
        assert!(!is_filename_token("PRERELEASE-NOTES.md", "RELEASE"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(is_filename_token("Backup.md", "backup"));
    }

    // ------------------------------------------------------------- classify

    #[test]
    fn a_filename_token_wins_even_when_it_is_also_short_enough_to_be_too_short() {
        // "BACKUP" is six characters, so it also fits too-short - the fixed
        // order in classify must still pick the more specific class first.
        let occurrences = Occurrences { hits: 1, all_in_comments: false };
        assert_eq!(classify("BACKUP.md", "BACKUP", &occurrences), LintClass::FilenameToken);
    }

    #[test]
    fn a_short_literal_is_too_short() {
        let occurrences = Occurrences { hits: 7, all_in_comments: false };
        assert_eq!(classify("src/main.rs", "INSERT", &occurrences), LintClass::TooShort);
    }

    #[test]
    fn a_seven_character_literal_is_not_too_short() {
        let occurrences = Occurrences { hits: 1, all_in_comments: false };
        assert_eq!(classify("src/main.rs", "INSERTS", &occurrences), LintClass::Healthy);
    }

    #[test]
    fn twenty_five_hits_is_too_common() {
        let occurrences = Occurrences { hits: 25, all_in_comments: false };
        assert_eq!(classify("src/main.rs", "setTimeout", &occurrences), LintClass::TooCommon);
    }

    #[test]
    fn twenty_four_hits_is_not_too_common() {
        let occurrences = Occurrences { hits: 24, all_in_comments: false };
        assert_eq!(classify("src/main.rs", "setTimeout", &occurrences), LintClass::Healthy);
    }

    #[test]
    fn every_occurrence_in_a_comment_is_comment_only() {
        let occurrences = Occurrences { hits: 1, all_in_comments: true };
        assert_eq!(classify("src/main.rs", "handleCreated", &occurrences), LintClass::CommentOnly);
    }

    #[test]
    fn a_check_with_no_hollow_pattern_is_healthy() {
        let occurrences = Occurrences { hits: 3, all_in_comments: false };
        assert_eq!(classify("src/main.rs", "handleCreated", &occurrences), LintClass::Healthy);
    }

    #[test]
    fn a_check_that_currently_fails_is_healthy_not_hollow() {
        // Zero hits: the literal is not in the file at all right now.
        // Proven NOT hollow by the strongest possible evidence - it fails -
        // so this lands in healthy, never comment-only (all_in_comments is
        // false for zero hits by construction, see scan_occurrences).
        let occurrences = scan_occurrences("nothing relevant in here", "MISSING-LITERAL");
        assert_eq!(occurrences.hits, 0);
        assert_eq!(classify("src/main.rs", "MISSING-LITERAL", &occurrences), LintClass::Healthy);
    }

    // -------------------------------------------------------- scan_occurrences

    #[test]
    fn a_literal_only_inside_a_line_comment_is_all_in_comments() {
        let content = "fn main() {}\n// calls handleCreated when done\n";
        let occurrences = scan_occurrences(content, "handleCreated");
        assert_eq!(occurrences.hits, 1);
        assert!(occurrences.all_in_comments);
    }

    #[test]
    fn a_literal_in_real_code_is_not_all_in_comments() {
        let content = "fn handleCreated() {}\n";
        let occurrences = scan_occurrences(content, "handleCreated");
        assert_eq!(occurrences.hits, 1);
        assert!(!occurrences.all_in_comments);
    }

    #[test]
    fn one_real_occurrence_among_several_commented_ones_is_not_all_in_comments() {
        let content = "// see handleCreated below\nfn handleCreated() {}\n";
        let occurrences = scan_occurrences(content, "handleCreated");
        assert_eq!(occurrences.hits, 2);
        assert!(!occurrences.all_in_comments);
    }

    #[test]
    fn a_literal_inside_a_block_comment_is_all_in_comments() {
        let content = "/* handleCreated is defined elsewhere */\nfn other() {}\n";
        let occurrences = scan_occurrences(content, "handleCreated");
        assert_eq!(occurrences.hits, 1);
        assert!(occurrences.all_in_comments);
    }

    #[test]
    fn a_hash_prefixed_rust_attribute_is_wrongly_read_as_a_comment() {
        // THE DOCUMENTED GAP: '#' is treated as a line comment regardless of
        // language, so a Rust attribute line is wrongly swept in - see this
        // file's own "WHERE THIS OVER-DETECTS" doc-comment section. Pinned
        // here so a future change to line_comment_spans has to notice it is
        // moving this exact, known boundary, not drift into it by accident.
        let content = "#[derive(handleCreated)]\nfn other() {}\n";
        let occurrences = scan_occurrences(content, "handleCreated");
        assert_eq!(occurrences.hits, 1);
        assert!(occurrences.all_in_comments, "a Rust attribute is not a comment, but this heuristic cannot tell the difference");
    }

    #[test]
    fn a_url_like_double_slash_inside_a_string_is_wrongly_read_as_a_comment() {
        // THE DOCUMENTED GAP: neither pass understands string literals, so a
        // "//" inside a URL still starts a "comment" that swallows the rest
        // of the line, including a real occurrence sitting after it.
        let content = "let handleCreated = \"http://example.com\";\n";
        let occurrences = scan_occurrences(content, "example.com");
        assert_eq!(occurrences.hits, 1);
        assert!(occurrences.all_in_comments, "the URL's // is not a comment marker, but this heuristic cannot tell the difference");
    }

    #[test]
    fn a_double_hyphen_cli_flag_is_wrongly_read_as_a_comment() {
        // THE DOCUMENTED GAP: '--' is treated as a line comment regardless
        // of language, so an ordinary CLI flag like "--release" (this very
        // workspace's own convention) wrongly starts a "comment" too.
        let content = "run with --release to get handleCreated optimised\n";
        let occurrences = scan_occurrences(content, "handleCreated");
        assert_eq!(occurrences.hits, 1);
        assert!(occurrences.all_in_comments, "-- is not a comment marker here, but this heuristic cannot tell the difference");
    }

    #[test]
    fn a_nested_block_comment_tail_is_wrongly_read_as_not_a_comment() {
        // THE DOCUMENTED GAP: nesting is not modelled - the first "*/"
        // closes the tracked span, so text between an inner close and the
        // true outer close reads as ordinary code even though a real Rust
        // parser would still call it a comment.
        let content = "/* outer /* inner */ handleCreated still-outer */\n";
        let occurrences = scan_occurrences(content, "handleCreated");
        assert_eq!(occurrences.hits, 1);
        assert!(
            !occurrences.all_in_comments,
            "lexically this is still inside the outer block comment, but this heuristic closes at the first */"
        );
    }

    // -------------------------------------------------------------- read-only

    #[test]
    fn read_bounded_utf8_returns_none_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_bounded_utf8(&dir.path().join("nope.txt")), None);
    }

    #[test]
    fn read_bounded_utf8_reads_a_small_real_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("here.txt"), b"hello").unwrap();
        assert_eq!(read_bounded_utf8(&dir.path().join("here.txt")), Some("hello".to_string()));
    }
}
