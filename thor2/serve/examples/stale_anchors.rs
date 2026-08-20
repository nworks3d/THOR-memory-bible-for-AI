//! Read-only census: of every live Rule/Orientation that carries at least one
//! Path or Dir Target binding, how many have every one of those anchors still
//! resolving on disk, and how many have at least one that resolves to
//! nothing. A census of the live store found 2703 of 2974 fireable items have
//! never once fired (see `serve::status`/`serve::audit`); part of that is
//! items still waiting for their moment, part is items anchored to a file
//! that no longer exists. Those are not waiting, they are demonstrably stale.
//! This tool measures the second group.
//!
//! Read-only. STALE_ANCHOR_DB must point at a COPY of a store, never the live
//! one. This never writes, creates, migrates or repairs anything in the store
//! it opens: it opens through `EventStore::open_existing`, the same
//! constructor the inspection commands (doctor, fsck, status) use precisely
//! because it does no schema work and no FTS heal on open (see that
//! constructor's own doc comment in `core/src/event_store.rs`) - the closest
//! thing this crate exposes to a read-only open. Every store read after that
//! (`serve::live::live_items`) is a SELECT-only reader too.
//!
//! STALE_ANCHOR_ROOT must point at the filesystem root anchor values should
//! be resolved against - normally a checkout, or a copy of one, of the
//! repository the store's Path/Dir anchors were originally written against.
//! Nothing under this root is ever written to either: only `std::fs::read_dir`
//! is used, to build a listing of what exists.
//!
//! Neither variable falls back to a default path. Either one missing and this
//! prints why and exits before opening anything (no store open, no directory
//! walk).
//!
//! MATCHING - read this before trusting a number below. An anchor value is
//! often a partial path ("model/src/gate.rs") or a bare distinctive filename
//! ("gate.rs"), never guaranteed to be a file's full path, so this never
//! compares by equality. Both the anchor and every real file/directory found
//! under the root are run through the crate's own single normalisation
//! function, `model::normalize::normalize_target` (trim, backslash to
//! forward slash, lowercase - see `model/src/normalize.rs`), then split into
//! path components. An anchor of kind Path resolves when some real FILE under
//! the root has a component sequence whose tail equals the anchor's own
//! components exactly; an anchor of kind Dir resolves the same way against
//! real DIRECTORIES instead. The real path must be at least as long as the
//! anchor - a real path shorter than the anchor is never truncated to fit.
//!
//! WHERE THIS CAN BE WRONG, both directions:
//!   - False "resolves": a short or bare-filename anchor (e.g. "gate.rs")
//!     matches ANY file with that exact tail anywhere under the root. If the
//!     file the rule actually meant was deleted and an unrelated file with
//!     the same name exists elsewhere in the tree, this reports the anchor as
//!     still resolving. Bare filenames are the anchors this is least able to
//!     be precise about; a longer anchor ("model/src/gate.rs") narrows this a
//!     lot but never to zero.
//!   - False "does not resolve": an anchor stored as a longer path than any
//!     real file's path relative to this root - most plausibly a full
//!     absolute path captured on a different machine, drive letter or user
//!     account (e.g. "C:/Users/other/dev/thor2/model/src/gate.rs") - can
//!     never match, because the real candidate is shorter than the anchor and
//!     this never trims the anchor down to the real candidate's length. Every
//!     such anchor is reported as non-resolving even when the file is right
//!     there under the root. If a meaningful share of anchors were ever
//!     captured as absolute paths rather than authored as partial ones, this
//!     is the more likely systematic error of the two, and it overcounts
//!     staleness.
//!   - The comparison is case-insensitive (normalize_target lowercases both
//!     sides), which can only ever cause an OVER-match between two distinctly
//!     cased files that coexist on a case-sensitive filesystem - not possible
//!     on the Windows filesystem this ships against, noted for completeness.
//!   - The root's own directory itself is never a match candidate, only what
//!     is found WALKING the root is indexed - a Dir anchor that names the
//!     checkout root by its own folder name is reported as non-resolving even
//!     though the root plainly exists. Deliberate: STALE_ANCHOR_ROOT is very
//!     likely a differently-named copy of the original repository (see
//!     above), so matching against ITS incidental folder name would be a
//!     coincidence, not a real answer.
//!   - Symlinks, junctions and other reparse points are handled however
//!     `DirEntry::file_type()` reports them on this platform - not specially
//!     detected, not specially excluded, and not verified against a real
//!     symlinked tree. If a target is reachable only through such a link and
//!     `file_type()` reports it as neither a file nor a directory here, it is
//!     simply not indexed and reads as non-resolving.
//!   - A `read_dir` failure on any one directory (permissions, a path past
//!     Windows' legacy MAX_PATH) is skipped rather than reported: that
//!     subtree is treated as empty rather than aborting the whole run. This
//!     can only ever produce MORE "does not resolve" results, never fewer.
//!
//! CLASSIFICATION - a single "N do not resolve" number is worse than no
//! number: most of what fails to resolve here was never going to resolve
//! against ANY Windows development root, and lumping that together with a
//! genuinely missing relative path makes a healthy store look like it is
//! rotting. So every anchor that does not resolve is sorted into exactly one
//! of seven classes below, decided purely from the anchor's own characters -
//! never by asking the filesystem anything, and never from the normalised
//! form `resolves()` above compares with (normalising would erase the very
//! backslash that makes a value UNC). The classes are checked in this exact
//! order and the first match wins, so a value that could fit more than one
//! description (a Windows absolute path with an environment variable inside
//! it, say) always lands in the earlier one:
//!   1. `unc` - starts with two backslashes, e.g. `\\fileserver\web\site\`.
//!   2. `posix-absolute` - starts with a single forward slash and has at
//!      least one further separator after it, e.g. `/srv/x`,
//!      `/etc/crontab`.
//!   3. `windows-absolute` - an ASCII letter followed by a colon, e.g.
//!      `C:\Users\other\dev\thor2\model\src\gate.rs`.
//!   4. `env-template` - contains a `%NAME%` or `$NAME` / `${NAME}` style
//!      variable placeholder anywhere in the value, e.g.
//!      `%LOCALAPPDATA%\thor\guard-rulebook.json`.
//!   5. `route-like` - starts with a single forward slash and, having
//!      already failed class 2 above, has no further separator, e.g.
//!      `/admin`.
//!   6. `not-a-path` - no file extension, and either no separator anywhere
//!      (a bare word, e.g. `gate`) or nothing but two or more short (six
//!      characters or fewer) all-upper-case tokens joined by forward
//!      slashes - the shape a list of stock tickers takes when it ends up as
//!      the value of a Path binding, e.g. `AAPL/MSFT/GOOG`.
//!   7. `relative-missing` - everything left over: a plain relative path
//!      that simply did not resolve.
//!
//! SHARED VS LOCAL - class 4 (`env-template`), class 5 (`route-like`) and
//! the bare-word half of class 6 (`not-a-path` when there is no separator at
//! all) are the exact three shapes `model::anchor_shape::unmatchable` owns
//! for the write gate's own ground 16 (see `model/src/gate.rs`, `model/src/
//! anchor_shape.rs`) - `classify`/`is_not_a_path` below call that one shared
//! function for those three decisions now, rather than keeping a second copy
//! of the same lexical rule (this workspace has a test that fails when a
//! decision is defined in two places - `model/tests/single_can_fire_
//! definition.rs`/`model/tests/single_normalization.rs` enforce it for
//! `Kind::can_fire`/`normalize_target`, and `anchor_shape`'s own doc comment
//! names this exact file as the intended second caller). Classes 1-3
//! (`unc`/`posix-absolute`/`windows-absolute`), the ticker-list half of
//! class 6, and class 7 (`relative-missing`) are NOT covered by
//! `anchor_shape` at all - it never makes a positive "this IS a real
//! absolute path" decision, and its own doc comment says plainly it does not
//! port the ticker-list heuristic - and stay fully local to this file.
//!
//! Classes 1-3 are outside the supplied root by construction and are never a
//! sign of rot: no STALE_ANCHOR_ROOT could ever contain a UNC share, a POSIX
//! absolute path, or a different machine's drive letter. Classes 4-7 are the
//! ones actually worth a human's time.
//!
//! WHERE THE CLASSIFIER ITSELF CAN BE WRONG (a separate question from where
//! `resolves()` can be wrong, above):
//!   - `not-a-path` requires NO file extension, so a bare filename WITH one
//!     and no directory in front of it (`gate.rs` on its own) is reported as
//!     `relative-missing`, even though it may never have lived under the
//!     supplied root at all - this scheme cannot tell "a bare filename that
//!     never had a directory" apart from "a bare filename whose directory
//!     got stripped before it was stored".
//!   - the ticker-list case inside `not-a-path` only ever looks at forward
//!     slashes, matching the one example this was built from; the same list
//!     joined with backslashes instead falls through to `relative-missing`.
//!   - `env-template` fires on the placeholder syntax appearing ANYWHERE in
//!     the value, so an otherwise plain relative path with one `%VAR%`
//!     segment in the middle is `env-template`, not `relative-missing`, even
//!     if the rest of that path is also genuinely gone.
//!   - classification reads the raw anchor value with only leading/trailing
//!     whitespace trimmed - no case-folding, no slash-direction
//!     normalisation, by design, since `unc` depends on a real backslash -
//!     so the same target spelled with the other slash direction can sort
//!     into a different class than the one a person would expect.
//!
//! STALE_ANCHOR_CLASS (optional, no effect on any of the above) - unset,
//! this prints the same "sample of at most 15" mixed across the four
//! actionable classes it always has. Set to one of the seven labels the
//! classification above prints (`unc`, `posix-absolute`, `windows-absolute`,
//! `env-template`, `route-like`, `not-a-path`, `relative-missing` - matched
//! case-sensitively, exactly as spelled) and the sample section is replaced
//! with EVERY non-resolving anchor sorted into that one class (actionable or
//! not), together with a count of how many were printed - the complete list
//! a person repairing one class at a time actually needs, rather than
//! whatever of it happened to survive a mixed cap of 15. The per-class
//! counts printed above the sample are unaffected either way: they always
//! cover all seven classes, filter or no. A name that matches none of the
//! seven is refused with the list of valid names spelled out, and that check
//! runs before STALE_ANCHOR_DB or STALE_ANCHOR_ROOT is even read, since
//! naming a bad class needs neither - a typo is reported without first
//! requiring a store and a root to already be correctly set up, and nothing
//! is ever opened on that path.
//!
//! Run (from the `thor2` workspace root):
//!   STALE_ANCHOR_DB=<path to a COPY of a store> \
//!   STALE_ANCHOR_ROOT=<path to a checkout the anchors were written against> \
//!   cargo run -p serve --example stale_anchors
//!
//! ...or, for the complete list of one class instead of the mixed sample of
//! 15:
//!   STALE_ANCHOR_DB=<path to a COPY of a store> \
//!   STALE_ANCHOR_ROOT=<path to a checkout the anchors were written against> \
//!   STALE_ANCHOR_CLASS=route-like \
//!   cargo run -p serve --example stale_anchors

use model::anchor_shape::{self, UnmatchableAnchor};
use model::item::{Binding, TargetKind};
use model::normalize::normalize_target;
use serve::live::live_items;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

/// A normalised, non-empty path-component sequence - the unit both a real
/// file/directory and an anchor value are reduced to before anything is
/// compared. Built through `model::normalize::normalize_target`, the crate's
/// one normalisation function (see `model/tests/single_normalization.rs`), so
/// this can never disagree with how the rest of the workspace decides "the
/// same target".
fn components(value: &str) -> Vec<String> {
    normalize_target(value).split('/').filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// Every file and every directory under a walked root, as root-relative
/// component sequences, indexed by their final component so resolving many
/// anchors never re-walks the tree and never re-scans every entry for each
/// one.
struct Listing {
    files: Vec<Vec<String>>,
    files_by_last: HashMap<String, Vec<usize>>,
    dirs: Vec<Vec<String>>,
    dirs_by_last: HashMap<String, Vec<usize>>,
}

fn index_by_last(entries: &[Vec<String>]) -> HashMap<String, Vec<usize>> {
    let mut map: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, comps) in entries.iter().enumerate() {
        if let Some(last) = comps.last() {
            map.entry(last.clone()).or_default().push(i);
        }
    }
    map
}

/// Walk one directory's entries into `files`/`dirs`, recursing into real
/// subdirectories. `dir` is the real directory to read; `rel` is the path
/// walked so far, relative to the root `build_listing` started from. A
/// directory this cannot even read (permissions, a path past Windows' legacy
/// MAX_PATH) is treated as empty rather than aborting the run - the same
/// fails-open stance `serve::live::live_items` takes on a store it cannot
/// fully read.
fn walk(dir: &Path, rel: &Path, files: &mut Vec<Vec<String>>, dirs: &mut Vec<Vec<String>>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        let name = entry.file_name();
        let child_rel = if rel.as_os_str().is_empty() { PathBuf::from(&name) } else { rel.join(&name) };
        if file_type.is_dir() {
            dirs.push(components(&child_rel.to_string_lossy()));
            walk(&entry.path(), &child_rel, files, dirs);
        } else if file_type.is_file() {
            files.push(components(&child_rel.to_string_lossy()));
        }
        // Neither: a symlink/junction/reparse point (or something else
        // file_type() cannot classify) - see this file's own doc comment
        // ("WHERE THIS CAN BE WRONG"). Silently not indexed.
    }
}

fn build_listing(root: &Path) -> Listing {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    walk(root, Path::new(""), &mut files, &mut dirs);
    let files_by_last = index_by_last(&files);
    let dirs_by_last = index_by_last(&dirs);
    Listing { files, files_by_last, dirs, dirs_by_last }
}

/// Whether `anchor` (already normalised into components) is the exact tail of
/// at least one real file (`kind == Path`) or real directory (`kind == Dir`)
/// found under the walked root. See this file's own doc comment for exactly
/// what this does and does not catch.
fn resolves(listing: &Listing, kind: TargetKind, anchor: &[String]) -> bool {
    let Some(last) = anchor.last() else { return false };
    let (by_last, entries) = match kind {
        TargetKind::Path => (&listing.files_by_last, &listing.files),
        TargetKind::Dir => (&listing.dirs_by_last, &listing.dirs),
        _ => unreachable!("resolves() is only ever called with a Path or Dir anchor"),
    };
    let Some(candidates) = by_last.get(last) else { return false };
    candidates.iter().any(|&i| {
        let real = &entries[i];
        real.len() >= anchor.len() && real[real.len() - anchor.len()..] == *anchor
    })
}

/// The seven classes a non-resolving anchor's raw value is sorted into - see
/// this file's own "CLASSIFICATION" section above for the exact rule behind
/// each one, the fixed order they are checked in, and where this can be
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AnchorClass {
    Unc,
    PosixAbsolute,
    WindowsAbsolute,
    EnvTemplate,
    RouteLike,
    NotAPath,
    RelativeMissing,
}

impl AnchorClass {
    /// Every class, in the fixed order they are checked in and printed in -
    /// spelled out explicitly rather than derived from a HashMap, so output
    /// order never depends on hashing.
    const ALL: [AnchorClass; 7] = [
        AnchorClass::Unc,
        AnchorClass::PosixAbsolute,
        AnchorClass::WindowsAbsolute,
        AnchorClass::EnvTemplate,
        AnchorClass::RouteLike,
        AnchorClass::NotAPath,
        AnchorClass::RelativeMissing,
    ];

    fn label(self) -> &'static str {
        match self {
            AnchorClass::Unc => "unc",
            AnchorClass::PosixAbsolute => "posix-absolute",
            AnchorClass::WindowsAbsolute => "windows-absolute",
            AnchorClass::EnvTemplate => "env-template",
            AnchorClass::RouteLike => "route-like",
            AnchorClass::NotAPath => "not-a-path",
            AnchorClass::RelativeMissing => "relative-missing",
        }
    }

    /// The inverse of `label()`, for interpreting `STALE_ANCHOR_CLASS`:
    /// matched case-sensitively against the exact strings `label()` returns,
    /// so a class name is either spelled exactly as printed or not
    /// recognised at all - no case-folding, no fuzzy matching. Built from
    /// `ALL` rather than its own duplicated match arm, so this can never
    /// accept a name that `label()`/`ALL` do not also agree is one of the
    /// seven classes.
    fn from_label(s: &str) -> Option<AnchorClass> {
        AnchorClass::ALL.into_iter().find(|c| c.label() == s)
    }

    /// env-template, route-like, not-a-path and relative-missing are worth a
    /// human's time; unc, posix-absolute and windows-absolute are outside
    /// the supplied root by construction and never a sign of rot. See the
    /// "CLASSIFICATION" section in this file's doc comment.
    fn is_actionable(self) -> bool {
        matches!(
            self,
            AnchorClass::EnvTemplate
                | AnchorClass::RouteLike
                | AnchorClass::NotAPath
                | AnchorClass::RelativeMissing
        )
    }
}

/// Sort one non-resolving anchor's raw value into exactly one `AnchorClass`.
/// Deliberately independent of `normalize_target`/`components`: this cares
/// about the ORIGINAL characters (normalising turns a UNC path's leading
/// backslashes into forward slashes before this ever saw it), while
/// `resolves()` cares about the normalised tail match - two separate
/// questions asked of the same string. Checks run in a fixed order, first
/// match wins; see "CLASSIFICATION" above.
fn classify(value: &str) -> AnchorClass {
    let v = value.trim();

    if v.starts_with(r"\\") {
        return AnchorClass::Unc;
    }
    if is_posix_absolute(v) {
        return AnchorClass::PosixAbsolute;
    }
    if is_windows_absolute(v) {
        return AnchorClass::WindowsAbsolute;
    }
    // EnvTemplate and Route are two of the three shapes `model::anchor_shape`
    // owns (see this file's own "SHARED VS LOCAL" doc-comment paragraph
    // above) - computed once here and matched for both, rather than keeping
    // a second copy of the same lexical rule. Checked in the identical order
    // `anchor_shape::unmatchable` itself uses (env-template before route),
    // which is also the order this file already checked them in.
    match anchor_shape::unmatchable(v) {
        Some(UnmatchableAnchor::EnvTemplate) => return AnchorClass::EnvTemplate,
        // A single leading slash with no further separator: already known
        // not to have one, or the is_posix_absolute() check above would have
        // matched first.
        Some(UnmatchableAnchor::Route) => return AnchorClass::RouteLike,
        // Some(BareWord) and None both fall through to is_not_a_path below,
        // which makes its own, separate call for the BareWord half of class 6.
        Some(UnmatchableAnchor::BareWord) | None => {}
    }
    if is_not_a_path(v) {
        return AnchorClass::NotAPath;
    }
    AnchorClass::RelativeMissing
}

/// A single forward slash, then at least one further `/` or `\`, e.g.
/// `/srv/x`, `/etc/crontab`.
fn is_posix_absolute(v: &str) -> bool {
    match v.strip_prefix('/') {
        Some(rest) => rest.contains('/') || rest.contains('\\'),
        None => false,
    }
}

/// An ASCII letter immediately followed by a colon, e.g. the `C:` in
/// `C:\Users\...`.
fn is_windows_absolute(v: &str) -> bool {
    let bytes = v.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// No file extension, and either no `/` or `\` anywhere (a bare word) or
/// nothing but a forward-slash-joined list of two or more short,
/// all-upper-case tokens (a ticker list handed to a Path binding). See
/// "CLASSIFICATION" above for what this narrow second case is for and what
/// it deliberately does not cover.
///
/// The bare-word half (no extension, no separator at all) is exactly
/// `model::anchor_shape::unmatchable`'s `BareWord` shape - shared, not
/// reimplemented here (see this file's own "SHARED VS LOCAL" paragraph). The
/// ticker-list half has no counterpart in `anchor_shape` at all (see that
/// module's own doc comment, "KNOWN, DELIBERATE GAPS") and stays local.
fn is_not_a_path(v: &str) -> bool {
    if has_extension(v) {
        return false;
    }
    let bare_word = anchor_shape::unmatchable(v) == Some(UnmatchableAnchor::BareWord);
    bare_word || is_short_uppercase_token_list(v)
}

/// The final `/`- or `\`-delimited segment contains a `.` that is neither
/// its first character (a dotfile like `.gitignore` is not "an extension")
/// nor its last (a trailing `.` with nothing after it is not one either).
fn has_extension(v: &str) -> bool {
    let last_segment = v.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(v);
    match last_segment.rfind('.') {
        Some(0) | None => false,
        Some(pos) => pos + 1 < last_segment.len(),
    }
}

/// Two or more `/`-separated tokens, every one of them 1-6 ASCII upper-case
/// letters, e.g. `AAPL/MSFT/GOOG`. Narrow on purpose: this exists only for
/// the ticker-list shape called out in the task this tool was built for, not
/// for any all-caps path segment in general.
fn is_short_uppercase_token_list(v: &str) -> bool {
    let tokens: Vec<&str> = v.split('/').filter(|t| !t.is_empty()).collect();
    tokens.len() >= 2
        && tokens
            .iter()
            .all(|t| t.len() <= 6 && t.chars().all(|c| c.is_ascii_uppercase()))
}

/// One (item, anchor) pair sampled for the printed output. A single live
/// item with two non-resolving anchors in different actionable classes
/// contributes up to two of these - one per anchor, each carrying only that
/// anchor's own value and class, never the item's other anchors. `kind` is
/// the anchor's own `TargetKind` (`Path` or `Dir` - the only two kinds this
/// tool ever looks at; see the `anchors` filter in `main`), carried
/// alongside the value so a per-class listing shows exactly which binding on
/// the item is the wrong one, not just that one of its bindings is.
struct SampleRow {
    class: AnchorClass,
    kind: TargetKind,
    id: String,
    anchor: String,
    text: String,
}

/// Interpret `STALE_ANCHOR_CLASS`. `None` (the variable is unset) means "no
/// filter, behave exactly as before" - the mixed sample of at most 15 across
/// the four actionable classes. `Some(raw)` is matched against
/// `AnchorClass::from_label`; a match selects that one class for the
/// complete per-class listing, and anything else is a usage error refused
/// right here, before `main` ever requires STALE_ANCHOR_DB/STALE_ANCHOR_ROOT
/// to be set, let alone opens a store - naming a bad class needs neither.
fn parse_class_filter(raw: Option<String>) -> anyhow::Result<Option<AnchorClass>> {
    let Some(raw) = raw else { return Ok(None) };
    match AnchorClass::from_label(&raw) {
        Some(class) => Ok(Some(class)),
        None => {
            let valid: Vec<&str> = AnchorClass::ALL.iter().map(|c| c.label()).collect();
            anyhow::bail!(
                "STALE_ANCHOR_CLASS={raw:?} is not a recognised class name. Valid class names: {}.",
                valid.join(", ")
            )
        }
    }
}

fn main() -> anyhow::Result<()> {
    // Cheap and local: no environment beyond its own variable, no
    // filesystem, no store. Checked before STALE_ANCHOR_DB/STALE_ANCHOR_ROOT
    // below so a typo'd STALE_ANCHOR_CLASS is reported on its own, without
    // also forcing those two to already be set correctly just to find that
    // out.
    let class_filter = parse_class_filter(std::env::var("STALE_ANCHOR_CLASS").ok())?;

    let db = PathBuf::from(std::env::var("STALE_ANCHOR_DB").expect(
        "set STALE_ANCHOR_DB to the path of a COPY of a store (never the live thor.db) - \
         this tool refuses to run without one and never falls back to a default path",
    ));
    let root = PathBuf::from(std::env::var("STALE_ANCHOR_ROOT").expect(
        "set STALE_ANCHOR_ROOT to the filesystem root anchor values should be resolved \
         against - this tool refuses to run without one and never falls back to a default path",
    ));

    let store = EventStore::open_existing(&db)?;
    let listing = build_listing(&root);

    let mut class_counts: HashMap<AnchorClass, usize> = HashMap::new();
    let mut sample: Vec<SampleRow> = Vec::new();

    for live in live_items(&store) {
        if !live.item.kind.can_fire() {
            continue; // only Rule/Orientation can ever carry an anchor that matters here
        }
        let anchors: Vec<(TargetKind, String)> = live
            .item
            .bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Target { kind, value } if matches!(kind, TargetKind::Path | TargetKind::Dir) => {
                    Some((*kind, value.clone()))
                }
                _ => None,
            })
            .collect();

        for (kind, value) in anchors {
            if resolves(&listing, kind, &components(&value)) {
                continue;
            }
            let class = classify(&value);
            *class_counts.entry(class).or_insert(0) += 1;
            let include = match class_filter {
                Some(target) => class == target,
                None => class.is_actionable() && sample.len() < 15,
            };
            if include {
                sample.push(SampleRow {
                    class,
                    kind,
                    id: live.id.clone(),
                    anchor: value,
                    text: live.item.text.clone(),
                });
            }
        }
    }

    let mut grand_total = 0usize;
    for class in AnchorClass::ALL {
        let n = class_counts.get(&class).copied().unwrap_or(0);
        grand_total += n;
        println!("{}: {n}", class.label());
    }

    let actionable_total: usize = AnchorClass::ALL
        .into_iter()
        .filter(|c| c.is_actionable())
        .map(|c| class_counts.get(&c).copied().unwrap_or(0))
        .sum();
    let outside_root_total = grand_total - actionable_total;

    println!();
    println!("total non-resolving anchors: {grand_total}");
    println!("actionable total: {actionable_total}");
    println!("outside supplied root total: {outside_root_total}");
    println!(
        "actionable (worth a look): env-template, route-like, not-a-path, relative-missing. \
         outside the supplied root (not a sign of rot): unc, posix-absolute, windows-absolute."
    );
    println!();
    match class_filter {
        Some(target) => {
            println!("{} item(s) printed for class {}:", sample.len(), target.label());
            for row in &sample {
                let snippet: String = row.text.chars().take(80).collect();
                println!("id={} kind={:?}", row.id, row.kind);
                println!("  anchor: {}", row.anchor);
                println!("  text:   {snippet}");
            }
        }
        None => {
            println!("sample of at most 15 non-resolving anchors from actionable classes:");
            for row in &sample {
                let snippet: String = row.text.chars().take(80).collect();
                println!("class={} id={}", row.class.label(), row.id);
                println!("  anchor: {}", row.anchor);
                println!("  text:   {snippet}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression coverage for the SHARED VS LOCAL split (see this file's own
    // "SHARED VS LOCAL" doc-comment paragraph): each case here pins a
    // classification `classify` already made before EnvTemplate/Route/
    // BareWord started delegating to `model::anchor_shape::unmatchable`,
    // proving the delegation is behaviourally identical, never merely
    // differently written.

    #[test]
    fn a_percent_env_template_still_classifies_as_env_template() {
        assert_eq!(classify(r"%LOCALAPPDATA%\thor\guard-rulebook.json"), AnchorClass::EnvTemplate);
    }

    #[test]
    fn a_dollar_env_template_still_classifies_as_env_template() {
        assert_eq!(classify("$HOME/.config/thor/settings.json"), AnchorClass::EnvTemplate);
    }

    #[test]
    fn a_route_like_value_still_classifies_as_route_like() {
        assert_eq!(classify("/admin"), AnchorClass::RouteLike);
    }

    #[test]
    fn env_template_still_takes_priority_over_route_like() {
        // Checked in fixed order, env-template first - mirrors
        // `model::anchor_shape::unmatchable`'s own
        // `an_env_template_takes_priority_over_a_route_like_shape` test.
        assert_eq!(classify("/%VAR%"), AnchorClass::EnvTemplate);
    }

    #[test]
    fn a_single_segment_posix_absolute_path_outside_the_known_roots_is_still_route_like() {
        // The boundary moved on 2026-08-19: the handful of directories that
        // exist on every unix-shaped machine are places now, because a rule
        // about the temp directory could not be anchored at the temp
        // directory (see `model::anchor_shape::is_a_root_directory`).
        // Everything else keeps the old reading - "/workspace" really is
        // lexically identical to a single-segment route.
        assert_ne!(classify("/etc"), AnchorClass::RouteLike);
        assert_eq!(classify("/workspace"), AnchorClass::RouteLike);
    }

    #[test]
    fn a_multi_segment_posix_absolute_path_is_unaffected_by_the_delegation() {
        // Decided by the LOCAL is_posix_absolute check, which still runs
        // BEFORE the shared EnvTemplate/Route decision - proves classify()'s
        // own fixed order survived the refactor unchanged.
        assert_eq!(classify("/etc/crontab"), AnchorClass::PosixAbsolute);
    }

    #[test]
    fn a_windows_absolute_path_is_unaffected_by_the_delegation() {
        assert_eq!(classify(r"C:\Users\other\dev\thor2\model\src\gate.rs"), AnchorClass::WindowsAbsolute);
    }

    #[test]
    fn a_unc_path_is_unaffected_by_the_delegation() {
        assert_eq!(classify(r"\\fileserver\web\site\index.html"), AnchorClass::Unc);
    }

    #[test]
    fn a_bare_word_still_classifies_as_not_a_path() {
        assert_eq!(classify("gate"), AnchorClass::NotAPath);
    }

    #[test]
    fn a_bare_extensionless_real_filename_still_classifies_as_not_a_path() {
        // Same known trade-off `model::anchor_shape::unmatchable` documents
        // for its own BareWord shape: LICENSE/Makefile are indistinguishable
        // here from a genuinely non-path bare word.
        assert_eq!(classify("LICENSE"), AnchorClass::NotAPath);
    }

    #[test]
    fn a_ticker_list_still_classifies_as_not_a_path_the_local_only_case() {
        // The ticker-list case is NOT one of anchor_shape's three shapes
        // (see that module's own "KNOWN, DELIBERATE GAPS" - it has a path
        // separator, so it fails BareWord) - stays local to
        // `is_short_uppercase_token_list`, proven still wired up correctly.
        assert_eq!(classify("AAPL/MSFT/GOOG"), AnchorClass::NotAPath);
    }

    #[test]
    fn a_relative_path_with_an_extension_still_classifies_as_relative_missing() {
        assert_eq!(classify("model/src/gate.rs"), AnchorClass::RelativeMissing);
    }

    #[test]
    fn a_bare_filename_with_an_extension_still_classifies_as_relative_missing() {
        assert_eq!(classify("gate.rs"), AnchorClass::RelativeMissing);
    }
}
