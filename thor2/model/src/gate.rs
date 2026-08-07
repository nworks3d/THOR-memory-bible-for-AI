//! The write gate: refuses a declaration that cannot be honoured, with a
//! reason and with what to do instead. Never silent (R5 of the workspace
//! design note: write and declare is the failure policy that must never be
//! quiet). Every refusal ground below has a test named after the defect it
//! prevents; a refusal ground with no test does not exist.

use crate::anchor_shape::{self, UnmatchableAnchor};
use crate::item::{Binding, Check, Item, Kind, TargetKind};
use crate::normalize::normalize_target;
use intent::Action;
use std::fmt;

/// A rule/orientation whose text is unreadable at a glance is not readable
/// at the moment of action either. Measured on the real 301-line corpus:
/// median 146 chars, longest 197 - this limit refuses zero of them.
pub const MAX_TEXT_CHARS: usize = 300;

/// A declaration or revise that the gate rejects. `problem` names what is
/// wrong; `fix` names what the writer should do instead. A refusal that only
/// says "invalid" is itself a defect (see the workspace design note) - every
/// refusal below carries both halves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub problem: String,
    pub fix: String,
}

impl Refusal {
    fn new(problem: impl Into<String>, fix: impl Into<String>) -> Self {
        Self { problem: problem.into(), fix: fix.into() }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} - {}", self.problem, self.fix)
    }
}

impl std::error::Error for Refusal {}

const BARE_ROLE_NAMES: &[&str] = &["main", "mod", "lib", "index", "test", "util"];

/// A bare (unqualified - no directory component) file name whose stem is a
/// generic role every project has one of. "src/main.rs" is not bare (the
/// directory makes it distinct); a lone "main.rs" is, because it matches
/// every project's entry point.
fn is_bare_role_name(normalized_value: &str) -> bool {
    if normalized_value.contains('/') {
        return false;
    }
    let stem = normalized_value.split('.').next().unwrap_or(normalized_value);
    BARE_ROLE_NAMES.contains(&stem)
}

/// The `Host` counterpart to `BARE_ROLE_NAMES`: every machine resolves
/// "localhost" to itself, and "0.0.0.0" is the networking convention for
/// "every interface" - both name no machine in particular, so a binding on
/// either matches every environment the same way a bare role name matches
/// every project.
const BARE_HOST_NAMES: &[&str] = &["localhost", "0.0.0.0"];

fn is_bare_host_name(normalized_value: &str) -> bool {
    BARE_HOST_NAMES.contains(&normalized_value)
}

/// Parse a raw action name into a `Moment` binding. Refuses an unknown name
/// with the full closed vocabulary to use instead (refusal ground 5).
pub fn parse_moment(name: &str) -> Result<Binding, Refusal> {
    match name.parse::<Action>() {
        Ok(action) => Ok(Binding::Moment(action)),
        Err(_) => Err(Refusal::new(
            format!("'{name}' is not a known action"),
            format!("use one of the known actions: {}", Action::known_names_sorted().join(", ")),
        )),
    }
}

fn check_target_binding(kind: TargetKind, value: &str) -> Result<(), Refusal> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Refusal::new(
            "a Target binding has an empty or whitespace-only value",
            "give the binding the exact value it must match, or drop the binding entirely",
        ));
    }
    // '?' is a glob wildcard for a path/symbol/command/project, but ordinary
    // syntax for a Route (a query string, e.g. "/admin/orders/:id?preview=1")
    // - measured on the real 55 http-route items this migration parked: 2 of
    // them carry a literal '?' and neither is a wildcard. '*' keeps meaning
    // "matches everything" for every kind, Route included (an Express-style
    // catch-all route really is spelled with one).
    if trimmed.contains('*') || (kind != TargetKind::Route && trimmed.contains('?')) {
        return Err(Refusal::new(
            format!("the Target value '{trimmed}' contains a glob wildcard, so it matches everything"),
            "name the exact path, symbol, command or project instead of a glob pattern",
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(Refusal::new(
            format!("the Target value '{trimmed}' names the current or parent directory, so it matches everything"),
            "name the exact path this binding must match, not '.' or '..'",
        ));
    }
    let normalized_value = normalize_target(trimmed);
    if kind == TargetKind::Path && is_bare_role_name(&normalized_value) {
        return Err(Refusal::new(
            format!(
                "'{trimmed}' is a bare role name (main, mod, lib, index, test, util) - every project has one, so it matches everything"
            ),
            "qualify it with the directory that makes this file distinct from every other project's main/index/lib",
        ));
    }
    // GROUND 11 (CONTINUED): the same rot ground 11 refuses in an item's
    // text is worse in a Target value, because the anchor is what decides
    // whether the item ever fires at all - a Path/Dir value carrying a line
    // number can never match a real file path, so the item would stay
    // silent forever. Measured on the real store: an item anchored to
    // "thor/src/courier.rs:716". Reuses `find_line_number_reference` (never
    // a second recogniser) on `trimmed`, not `normalized_value`:
    // `normalize_target` lowercases the value, which would silently break
    // the "L93" form's capital-L match - exactly why the text-level check
    // above also reads `item.text` directly rather than a normalised copy
    // of it. Symbol, Command, Project, Route and Host are deliberately left
    // alone: a colon-and-digits value is ordinary syntax there (a host:port
    // or a port-bearing command, for instance), never a line number.
    if matches!(kind, TargetKind::Path | TargetKind::Dir) {
        if let Some(reference) = find_line_number_reference(trimmed) {
            return Err(Refusal::new(
                format!(
                    "the Target value '{trimmed}' carries a line-number reference ('{reference}') - a target carrying a line number can never match a real path, so this item would never fire"
                ),
                "anchor the file itself, and put the location in the text instead, as a symbol or a short quoted string",
            ));
        }
    }
    // GROUND 16: a Path/Dir target whose VALUE has one of three shapes can
    // never match a real file or directory, decided lexically - never by
    // touching the filesystem (see `crate::anchor_shape::unmatchable`, the
    // one place this decision is made; `serve/examples/stale_anchors.rs`'s
    // own read-only census sorts a non-resolving anchor into the same
    // buckets, among others, for its report). A census of the real store
    // found 29 anchors exactly this shape - 19 web routes stored as a
    // Path, 6 values that are not paths at all, 4 unexpanded
    // environment-variable templates - each one silent forever, because
    // nothing told whoever wrote it. Gated to Path/Dir for the same reason
    // ground 11 (continued) is: a leading slash or a bare word is ordinary
    // syntax for a Route/Host/Command/Symbol/Project, never a sign of rot
    // there - a Route value is SUPPOSED to start with a slash, and a
    // Command or Symbol is routinely a bare word.
    //
    // `BareWord` is further narrowed to `Path` alone, never `Dir`: a
    // directory has no "file extension" to begin with, so a bare,
    // unqualified directory name is the ORDINARY way to write a real
    // anchor, not a sign it is wrong - this store already anchors real
    // rules to bare directory names ("frozen", "node_modules", ".git",
    // measured across `serve`'s own real fixtures). Refusing that shape
    // would trade a silent non-firer for a wrongly refused legitimate
    // anchor, which is the one trade this ground must never make.
    // `EnvTemplate` and `Route` carry no such exception: an unexpanded
    // template or a route shape is exactly as wrong for a directory as it
    // is for a file.
    if matches!(kind, TargetKind::Path | TargetKind::Dir) {
        let shape = anchor_shape::unmatchable(trimmed)
            .filter(|shape| kind == TargetKind::Path || *shape != UnmatchableAnchor::BareWord);
        if let Some(shape) = shape {
            let (problem, fix) = match shape {
                UnmatchableAnchor::EnvTemplate => (
                    format!(
                        "the Target value '{trimmed}' contains an environment-variable template, which is never expanded on disk, so a Path/Dir target can never match it"
                    ),
                    "anchor the exact expanded file or command the fact is really about, not an unexpanded template".to_string(),
                ),
                UnmatchableAnchor::Route => (
                    format!(
                        "the Target value '{trimmed}' has the shape of a web route (a single leading slash, no further separator), so a Path/Dir target can never match it"
                    ),
                    "bind this as Target kind Route instead of Path/Dir - route is a target kind this store already has".to_string(),
                ),
                UnmatchableAnchor::BareWord => (
                    format!(
                        "the Target value '{trimmed}' has no path separator and no file extension, so it does not name a real file"
                    ),
                    "anchor the exact file or command the fact is really about, qualified with at least a directory or an extension".to_string(),
                ),
            };
            return Err(Refusal::new(problem, fix));
        }
    }
    if kind == TargetKind::Host && is_bare_host_name(&normalized_value) {
        return Err(Refusal::new(
            format!(
                "'{trimmed}' is a bare host name (localhost, 0.0.0.0) - every environment has one, so it matches everything"
            ),
            "name the exact hostname this binding must match, not a generic loopback or all-interfaces name",
        ));
    }
    if kind == TargetKind::Route && normalized_value == "/" {
        return Err(Refusal::new(
            "the Route value '/' is the root path, so it matches every route under it",
            "name the exact route this binding must match, not the bare root '/'",
        ));
    }
    Ok(())
}

/// GROUNDS 13/14 for a `Check::PathExists`/`Contains`/`Absent`/`AbsentAll`
/// (see `declare`): an empty path names nothing to check, and a glob
/// character in it means it does not name one exact file - a `Check` is
/// deliberately narrower than a Target binding (see `Check`'s own doc
/// comment: "no globs"), so this does not reuse `check_target_binding`
/// above. The path is trimmed before the emptiness check, the same as
/// `check_target_binding` does for a Target value, because a path made only
/// of whitespace can never name a real file either way. Never called for
/// `Forbidden`, the one form with no path at all - see `check_check` below.
fn check_path(path: &str) -> Result<(), Refusal> {
    if path.trim().is_empty() {
        return Err(Refusal::new(
            "a check has an empty or whitespace-only path",
            "name the exact file this check must look at, or drop the check entirely",
        ));
    }
    if path.contains('*') || path.contains('?') {
        return Err(Refusal::new(
            format!("the check path '{path}' contains a glob wildcard, so it does not name one exact file"),
            "name the exact file this check must look at instead of a glob pattern",
        ));
    }
    Ok(())
}

/// GROUND 13 (CONTINUED): `Contains`/`Absent`'s own single literal, checked
/// for emptiness WITHOUT trimming - unlike a path, a literal is content to
/// search for verbatim, and a literal of pure whitespace (a trailing-space
/// check, an exact indent) can be a deliberate, meaningful thing to look
/// for. Trimming it here would silently change what the check searches for,
/// which is exactly the kind of silent rot this whole mechanism exists to
/// catch, not commit.
fn check_single_literal(literal: &str) -> Result<(), Refusal> {
    if literal.is_empty() {
        return Err(Refusal::new(
            "a check has an empty literal",
            "give the exact string this check must find (or must not find), or drop the check entirely",
        ));
    }
    proves_something(literal)
}

/// GROUND 18 - a literal that cannot prove anything is refused.
///
/// A check is what earns a rule the right to block, which makes a WEAK check
/// worse than none at all: with no check you know you have nothing, while a
/// literal that appears in every file always holds, never reports, and looks
/// exactly like protection.
///
/// This refuses only what the literal's own SHAPE settles, without opening a
/// file - `declare` is pure and has no filesystem root to resolve against.
/// How often a literal really occurs is a measurement, and it belongs where
/// a file can actually be read (`serve/examples/check_census.rs` reports it).
///
/// THE ONE RULE: shorter than three characters AND entirely ASCII
/// alphanumeric. A fragment like "fn", "if" or "x" is in nearly every source
/// file, so a check built on one could never fail.
///
/// Deliberately NOT a plain length rule, and this is the whole reason the
/// condition is written the way it is. A SINGLE PUNCTUATION CHARACTER is one
/// of the most valuable literals this system has - an em dash, a curly
/// quote, an ellipsis - and a typography rule is exactly a set of
/// one-character literals. A naive "too short to be distinctive" test would
/// refuse the clearest real use there is. Pure whitespace is left alone for
/// the same kind of reason, spelled out in `check_single_literal`'s own doc
/// comment above: a trailing space or an exact indent is a deliberate thing
/// to search for.
///
/// Still allowed through, knowingly: a three-character keyword like "let" or
/// "pub", and any longer common word. Those are weak too, but the line has
/// to fall somewhere, and a missed weak check costs far less than a wrongly
/// refused good one.
fn proves_something(literal: &str) -> Result<(), Refusal> {
    if literal.chars().count() < 3 && literal.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(Refusal::new(
            format!("a check's literal '{literal}' is too short and too ordinary to prove anything - a fragment like that is in nearly every file, so the check would always hold and never report"),
            "name something distinctive instead: a symbol, a constant, a whole phrase. A single punctuation character IS distinctive and is not refused here",
        ));
    }
    Ok(())
}

/// The literal-LIST rule shared by `AbsentAll` (GROUND 15) and `Forbidden`
/// (GROUND 17) - the two SET forms, each with its own ground because each
/// refuses a shape no single-literal form can: an empty list forbids
/// nothing, so the check could never fail, which makes it dead weight
/// dressed up as a rule - the same reasoning `declare`'s ground 1 applies to
/// a Rule with no binding at all. Checks the list itself is non-empty and
/// that every entry in it is non-empty; a list of one is not refused for
/// either form - the SET form used with exactly one literal is an ordinary,
/// if unusual, way to write what the single-literal form (where one exists)
/// already says, not a malformed request. `label` names the check form in
/// the refusal text (e.g. "an absent_all check", "a forbidden check"), so
/// the message still says which one fired.
fn check_literal_list(literals: &[String], label: &str) -> Result<(), Refusal> {
    if literals.is_empty() {
        return Err(Refusal::new(
            format!("{label} has an empty literal list"),
            "name at least one exact string this check must find none of, or drop the check entirely",
        ));
    }
    if let Some(empty_at) = literals.iter().position(|literal| literal.is_empty()) {
        return Err(Refusal::new(
            format!("{label}'s literal list has an empty entry at position {empty_at}"),
            "give every entry in the literal list an exact, non-empty string to find, or remove the empty one",
        ));
    }
    // GROUND 18 applies to every entry, not just to a single-literal check:
    // one useless member is enough to make a set-carrying rule hold forever.
    for literal in literals {
        proves_something(literal)?;
    }
    Ok(())
}

/// The content-validation half of the write-time rules for a `Check` (see
/// `declare`, which calls this only once an item's kind has already been
/// confirmed eligible to carry one at all - ground 12). Each of the five
/// forms gets exactly the rule its own shape needs: the four that carry a
/// path share `check_path` unconditionally - `Contains`/`Absent` add a
/// single-literal rule on top, `AbsentAll` a literal-LIST rule instead - and
/// `Forbidden` (GROUND 17), the one form with no path at all (see its own
/// doc comment on `Check`), skips `check_path` entirely and gets only the
/// literal-list rule, shared with `AbsentAll` via `check_literal_list`
/// above rather than a second copy of the same two refusals.
fn check_check(check: &Check) -> Result<(), Refusal> {
    match check {
        Check::PathExists { path } => check_path(path),
        Check::Contains { path, literal } | Check::Absent { path, literal } => {
            check_path(path)?;
            check_single_literal(literal)
        }
        Check::AbsentAll { path, literals } => {
            check_path(path)?;
            check_literal_list(literals, "an absent_all check")
        }
        Check::Forbidden { literals } => check_literal_list(literals, "a forbidden check"),
    }
}

// GROUND 7 WAS HERE, AND THE DATA KILLED IT (2026-08-02).
//
// The rule was "a Target must be named in the item's own sentence, otherwise
// the binding is noise". It was reasoned, not measured. Run over 2962 real
// extracted items it refused 566 of them (19%), and every sampled refusal was
// a correctly bound, useful item:
//
//   target lib/analyze.js  <- "Capt machine-volumes op de safe envelope."
//   target consumer-orders.test.js
//                          <- "Zet alle imports voor de eerste test()-aanroep."
//   target docs/BUILD-NOTES.md
//                          <- "De veiligheidswaarschuwingen hier ... nooit afzwakken."
//
// A sentence delivered AT a file does not repeat that file's name - supplying
// that context is what delivery is for, and "hier" is a correct way to write it.
// Meanwhile the failure this rule guarded against was already measured on the
// same corpus: of 143 anchored notes exactly 1 pointed somewhere wrong (0.7%),
// and that one had a vague anchor, not a wrong path. Refusing 19% to prevent
// 0.7% is a bad trade, and a first widening attempt (accept the file name
// alone) recovered only 92 of the 566 - the shape of the rule was wrong, not
// its strictness.
//
// R9 says a compensating mechanism is a reported design failure, so this is not
// patched with an exception list. It is removed, and the numbers are in
// RESULTS.md. Ground 8 stays: a binding that matches EVERYTHING is a real
// failure, and it caught 7 real cases on the same run.
//
// What ground 7 was groping for still exists, but it is the opposite test and
// it is not mechanical: erase the target name from a sentence that does carry
// it, and if the sentence stays true and complete, the binding is noise. That
// needs a model, so it belongs in the extraction contract, not in a write gate.

/// Words that introduce a line-number reference: English "line" and the
/// Dutch "regel". Matched case-insensitively and only when the next token
/// starts with a digit, so "line up" or a "regel" meaning "a rule" never
/// match.
const LINE_NUMBER_WORDS: &[&str] = &["line", "regel"];

/// A conservative set of source/text file extensions, used to recognise a
/// "file.ext:93" line-number suffix (e.g. "guard.rs:610"). Deliberately
/// narrow and letters-only: a wider match would also catch a host:port like
/// "quote.nworks3d.be:8080" or an IP:port like "127.0.0.1:8080" - a missed
/// case is acceptable here, a false refusal is not.
const FILE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "rb", "php", "c", "h",
    "cpp", "hpp", "cc", "cs", "swift", "kt", "scala", "sh", "ps1", "psm1", "md", "txt", "json",
    "toml", "yaml", "yml", "html", "htm", "css", "scss", "sql", "vue", "lua",
];

/// A whitespace-delimited token stripped of the punctuation that commonly
/// wraps or follows one - "(L93)", "line 93,", "guard.rs:610." - while
/// leaving internal characters (the ':' and '.' the checks below key on)
/// untouched.
fn strip_edge_punctuation(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}

/// Whether `token` is exactly a capital 'L' followed by one or more digits
/// and nothing else: "L93" matches, "XL93" does not (does not start with
/// 'L'), and "L93x" does not (something other than a digit after the 'L').
fn is_capital_l_line_token(token: &str) -> bool {
    if !token.starts_with('L') || token.len() < 2 {
        return false;
    }
    token[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Whether `token` ends in ":<digits>" directly attached to a filename-
/// looking stem (a '.' followed by a known extension): "guard.rs:610"
/// matches, "127.0.0.1:8080" does not (the "extension" "1" is not a
/// letter), and "quote.nworks3d.be:8080" does not ("be" is not a known
/// source/text extension).
fn looks_like_file_line_suffix(token: &str) -> bool {
    let colon_idx = match token.rfind(':') {
        Some(idx) => idx,
        None => return false,
    };
    let (before, after) = (&token[..colon_idx], &token[colon_idx + 1..]);
    if after.is_empty() || !after.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let dot_idx = match before.rfind('.') {
        Some(idx) => idx,
        None => return false,
    };
    let (stem, ext) = (&before[..dot_idx], &before[dot_idx + 1..]);
    if stem.is_empty() || ext.is_empty() {
        return false;
    }
    FILE_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

/// Extensions that name PROGRAM SOURCE - a file that only exists inside one
/// repository. Deliberately not the same list as `FILE_EXTENSIONS` above:
/// that one recognises a line-number suffix and can afford to be wide, this
/// one decides a REFUSAL (ground 19), so it holds only what a repository
/// compiles or runs.
///
/// Configuration, documentation and operator scripts are left out on
/// purpose, and the measurement is why. On the owner's store, 59 of 421
/// global items name a file. Of those, the ones naming `settings.json`,
/// `CLAUDE.md`, `AGENTS.md`, `scripts/check-private-data.sh`,
/// `docker-compose.yml` or `ship-once.ps1` were all correctly global: they
/// are about the machine, or about every repository at once. Refusing those
/// would have been roughly 46 false refusals to catch 9 real ones. Program
/// source has no such counter-examples: not one global item named a `.rs`
/// file for a reason that survived reading.
const SOURCE_CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "go", "java", "rb", "php", "c", "h", "cpp", "hpp", "cc", "cs", "swift", "kt",
    "scala", "vue", "sql",
];

/// The JavaScript/TypeScript family, where a BARE token names a LIBRARY far
/// more often than a file: "Chart.js", "Node.js", "Vue.js". Measured on the
/// owner's store, 4 of the 5 bare `.js` tokens in global items were the
/// Chart.js library and every real repository file among them carried a
/// directory ("server/lib/pdf.js"). So these count only when the token also
/// carries a path separator - a deliberate miss on a bare "server.js",
/// because this gate's rule throughout is that a missed case is acceptable
/// and a false refusal is not.
const PATH_REQUIRED_EXTENSIONS: &[&str] = &["js", "jsx", "mjs", "cjs", "ts", "tsx"];

/// The source file `token` names, if it names one. `None` for anything with
/// no extension, an extension this does not treat as program source, a bare
/// role name every project has one of (`main.rs`, `mod.rs` - see
/// [`is_bare_role_name`]), a JS/TS-family name with no directory in it, or a
/// wildcard pattern.
///
/// The wildcard case is the fifth false-positive class the measurement found:
/// a global rule about pinning ESP32 library versions ends "anders faalt de
/// build op een ontbrekende esp32-hal-*.h header". That names a header the
/// toolchain ships, in a whole family of them, in no repository at all. A
/// pattern names a class of files; this ground is only ever about ONE file
/// that lives in ONE repository.
fn source_file_in_token(token: &str) -> Option<&str> {
    if token.contains('*') || token.contains('?') {
        return None;
    }
    let dot = token.rfind('.')?;
    let (stem, ext) = (&token[..dot], &token[dot + 1..]);
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    let ext = ext.to_lowercase();
    let normalized = token.replace('\\', "/");
    let has_directory = normalized.contains('/');
    let known = if SOURCE_CODE_EXTENSIONS.contains(&ext.as_str()) {
        true
    } else if PATH_REQUIRED_EXTENSIONS.contains(&ext.as_str()) {
        has_directory
    } else {
        false
    };
    if !known || is_bare_role_name(&normalized) {
        return None;
    }
    Some(token)
}

/// The first source file `text` names, if any, returned as the exact token
/// matched so a refusal can quote it. Tokenised exactly like
/// [`find_line_number_reference`], so "(serve/src/capture.rs)" and
/// "guard.rs," both reduce to the bare name.
fn find_source_file_reference(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(strip_edge_punctuation)
        .find_map(|token| source_file_in_token(token).map(str::to_string))
}

/// The line-number reference in `text`, if any - "line 93", the Dutch
/// "regel 93", "L93", or a "file.ext:93" suffix - returned as the exact
/// fragment matched, so the refusal can quote it instead of saying "some
/// line number somewhere". A version number, a percentage, a byte count, a
/// date, a time and a port in prose must never match here: see the ground
/// 11 tests below for exactly which forms are covered and which are
/// deliberately left alone.
fn find_line_number_reference(text: &str) -> Option<String> {
    let tokens: Vec<&str> = text.split_whitespace().map(strip_edge_punctuation).collect();
    for (i, token) in tokens.iter().enumerate() {
        if LINE_NUMBER_WORDS.contains(&token.to_lowercase().as_str()) {
            if let Some(next) = tokens.get(i + 1) {
                if next.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    return Some(format!("{token} {next}"));
                }
            }
        }
        if is_capital_l_line_token(token) || looks_like_file_line_suffix(token) {
            return Some((*token).to_string());
        }
    }
    None
}

/// The write-time gate for a new item. Called by `store::declare` before
/// anything is appended to the log; `store::revise` calls this too (a
/// revise must still satisfy every ground a fresh declaration would) and
/// then layers the field-preservation check on top (ground 9).
pub fn declare(item: &Item) -> Result<(), Refusal> {
    if item.kind == Kind::Rule && item.bindings.is_empty() {
        return Err(Refusal::new(
            "a Rule with no binding can never fire",
            "give it at least one binding: Moment(<action>), a Target, or Always",
        ));
    }
    if item.kind != Kind::Report && item.expires.is_some() {
        // Only a Report has a short operational life. The other three last until
        // someone revises them, so an expiry on them is a slow silent deletion.
        let why = match item.kind {
            Kind::Rule => "rules never expire",
            _ => "it lasts until it is revised",
        };
        return Err(Refusal::new(
            format!(
                "a {:?} carries an expires date, but {why}",
                item.kind
            ),
            "drop expires and revise it when it changes; only a Report may expire",
        ));
    }
    if matches!(item.kind, Kind::Report | Kind::Chunk) && !item.bindings.is_empty() {
        return Err(Refusal::new(
            format!("a {:?} carries a binding, but archive material may never reach a gate", item.kind),
            "drop the binding (Moment, Target and Always are all refused here) - archive material is found by searching, never served at a moment of action",
        ));
    }
    // The char limit exists so text stays readable "at the moment of
    // action" (see MAX_TEXT_CHARS's own doc comment) - which is exactly
    // Kind::can_fire's definition of "reaches a gate", so this reuses it
    // rather than re-listing Rule/Orientation itself.
    if item.kind.can_fire() {
        let len = item.text.chars().count();
        if len > MAX_TEXT_CHARS {
            return Err(Refusal::new(
                format!("the text is {len} characters, over the {MAX_TEXT_CHARS}-character limit for a {:?}", item.kind),
                format!("shorten the text to at most {MAX_TEXT_CHARS} characters; move the reasoning into a Report instead"),
            ));
        }
    }
    // GROUND 11: a line number is a snapshot of the file's shape at write
    // time, and the file keeps moving. Measured on 3 real cases, all 3 were
    // wrong within a week (a fact named line 93; the truth was line 95; a
    // correction to line 94 was also wrong) - nothing warned before this.
    // Gated on `can_fire` for the same reason ground 4 and ground 10 are:
    // required exactly for the two kinds that ever reach a gate, never
    // re-listed as Rule/Orientation itself.
    if item.kind.can_fire() {
        if let Some(reference) = find_line_number_reference(&item.text) {
            return Err(Refusal::new(
                format!("the text names a specific line number ('{reference}'), which rots as soon as the file's line count changes"),
                "name the location by symbol (function, struct or command name) or with a short quoted unique string from that line instead of a line number",
            ));
        }
    }
    // GROUND 19: a fact with no project is served in EVERY project, so it may
    // not name a file that exists in only one. Measured, and the reason this
    // ground exists: the owner's store held 421 global rules and orientations,
    // among them ones naming `guard.rs`, `thor/src/cli.rs`, `scope.rs` and
    // `serve/src/lookup.rs`. Every one was written while working inside THOR's
    // own checkout, and every one was then handed to a session in an unrelated
    // project as general truth. Nothing warned, because a global item is
    // exactly the item no project filter can ever scope out.
    //
    // The way out is a project, never a rephrase: naming the repository the
    // file belongs to is what makes the fact true again. Gated on `can_fire`
    // like grounds 4, 10, 11 and 12 - archive material is found by searching,
    // never served unprompted, so it is not this ground's problem.
    if item.kind.can_fire() && item.project.is_none() {
        if let Some(named) = find_source_file_reference(&item.text) {
            return Err(Refusal::new(
                format!(
                    "this item has no project, so it is served in every project, but its text names the source file '{named}', which exists in only one"
                ),
                "give it the project that file belongs to; if the fact really is true everywhere, say what it means without naming one repository's source",
            ));
        }
    }
    // GROUND 10: a Rule/Orientation never expires by design (ground 2), which
    // is exactly where staleness has nowhere else to hide - a detector that
    // decides at READ time whether such a fact has gone stale depends on a
    // model choosing to look, and that was measured at 0 of 20 independently.
    // A falsifier is filled in at WRITE time instead, so it travels with the
    // fact regardless of whether anything ever reads it back. Reuses
    // `Kind::can_fire` for the same reason the char limit above does: this is
    // required exactly for the two kinds that ever reach a gate, never
    // re-listed as Rule/Orientation itself. A Report/Lookup/Chunk may still
    // carry one (see `revise`'s field-preservation check below) but is never
    // required to: a Report already has its own expiry, and a Lookup/Chunk is
    // an archive fact answered by an explicit request, not a standing claim.
    if item.kind.can_fire() && item.falsifier.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err(Refusal::new(
            format!("a {:?} has no falsifier - nothing says what observation would prove it wrong", item.kind),
            "add a falsifier: one sentence naming the observation that would make this fact false, or write it as a Report instead if it never goes stale",
        ));
    }
    // GROUND 12: a check is a claim that an item can be proven CURRENT right
    // now, and only a Rule or Orientation is ever served at a gate where
    // "is this still current" matters - the same reason ground 3 refuses a
    // binding on archive material. Reuses `Kind::can_fire` rather than
    // re-listing Rule/Orientation (or their complement) a second time, for
    // the same reason ground 4/10/11 above do.
    if item.check.is_some() && !item.kind.can_fire() {
        return Err(Refusal::new(
            format!("a {:?} carries a check, but only a kind that can fire may carry one", item.kind),
            "drop the check, or make this a Rule/Orientation if it needs to be provably current at a gate",
        ));
    }
    if let Some(check) = &item.check {
        check_check(check)?;
    }
    if item.kind == Kind::Lookup && item.key.is_none() {
        return Err(Refusal::new(
            "a Lookup has no key",
            "give it a key - a Lookup is only ever served in answer to an explicit request for that key",
        ));
    }
    for binding in &item.bindings {
        if let Binding::Target { kind, value } = binding {
            check_target_binding(*kind, value)?;
        }
    }
    Ok(())
}

/// A field the existing item carried but the revised item drops. Named so a
/// caller sees exactly which field to carry forward, not just "invalid".
fn dropped_field(name: &'static str) -> Refusal {
    Refusal::new(
        format!("the revise drops the existing '{name}' field"),
        format!("carry the existing '{name}' value forward in the revised item, or revise it deliberately rather than omitting it"),
    )
}

/// The write-time gate for a revise: everything `declare` requires, PLUS no
/// field the existing item had may silently vanish (refusal ground 9).
pub fn revise(existing: &Item, updated: &Item) -> Result<(), Refusal> {
    declare(updated)?;
    if !existing.bindings.is_empty() && updated.bindings.is_empty() {
        return Err(dropped_field("bindings"));
    }
    if existing.severity.is_some() && updated.severity.is_none() {
        return Err(dropped_field("severity"));
    }
    if existing.project.is_some() && updated.project.is_none() {
        return Err(dropped_field("project"));
    }
    if !existing.tags.is_empty() && updated.tags.is_empty() {
        return Err(dropped_field("tags"));
    }
    if existing.expires.is_some() && updated.expires.is_none() {
        return Err(dropped_field("expires"));
    }
    if existing.key.is_some() && updated.key.is_none() {
        return Err(dropped_field("key"));
    }
    if existing.falsifier.is_some() && updated.falsifier.is_none() {
        return Err(dropped_field("falsifier"));
    }
    if existing.check.is_some() && updated.check.is_none() {
        return Err(dropped_field("check"));
    }
    Ok(())
}

/// Which optional fields a caller explicitly asked `revise` to clear, so a
/// deliberate clear can be told apart from a caller that simply forgot to
/// carry a field forward. `revise` above stays exactly as written - it
/// compares two `Item`s and nothing else, on purpose, so its own tests keep
/// proving the unconditional protection unchanged - because it cannot be
/// the one to see intent: intent lives in how a CALLER built `updated`
/// (did it copy `existing.field` forward, or was it told to blank it?),
/// never in the two finished items themselves. `ClearedFields::baseline`
/// (below) is the one place that turns "the caller asked to clear field X"
/// into something `revise`'s existing comparison already knows how to
/// honour, without changing that comparison at all.
///
/// Exactly the six fields the MCP/CLI convention documents as clearable via
/// an empty-string argument: severity, project, expires, key, falsifier,
/// check. `bindings` and `tags` are deliberately absent - neither carries an
/// "omit keeps, empty clears" convention (an empty list is always refused
/// for both; see their call sites), so there is no legitimate clear for this
/// type to ever encode for them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClearedFields {
    pub severity: bool,
    pub project: bool,
    pub expires: bool,
    pub key: bool,
    pub falsifier: bool,
    pub check: bool,
}

impl ClearedFields {
    /// `existing`, with exactly the fields marked `true` above already
    /// blanked out - the comparison baseline a caller should hand `revise`
    /// (in place of the real `existing`) once it knows which fields were
    /// deliberately cleared. Sound because `existing` has exactly one job in
    /// `store::revise`: the ground-9 comparison against `updated` (the
    /// persisted body always comes from `updated`, never from `existing` -
    /// see `store::revise`'s own body). Blanking a field HERE, before that
    /// comparison ever runs, means its absence from `updated` no longer
    /// reads as a silent loss for exactly the fields named `true`; every
    /// field left `false` keeps its real value, so a field that vanishes
    /// WITHOUT this signal is still refused exactly as before - see this
    /// module's own `cleared_fields` tests below, and `ClearedFields::
    /// default()` (all `false`) is a no-op that returns `existing` itself
    /// unchanged.
    pub fn baseline(self, existing: &Item) -> Item {
        let mut baseline = existing.clone();
        if self.severity {
            baseline.severity = None;
        }
        if self.project {
            baseline.project = None;
        }
        if self.expires {
            baseline.expires = None;
        }
        if self.key {
            baseline.key = None;
        }
        if self.falsifier {
            baseline.falsifier = None;
        }
        if self.check {
            baseline.check = None;
        }
        baseline
    }
}

/// Turn the four flat `check_kind`/`check_path`/`check_literal`/
/// `check_literals` MCP/CLI arguments into `model::item::Check`. The one
/// shared home both `mcp`'s `revise`/`remember` tools and `model`'s own
/// `model` CLI binary call, rather than each keeping its own
/// independently-maintained copy of the same decision - `mcp` depends on
/// `model` and a `src/bin` binary cannot export code for another crate to
/// import either way, so this is the one legitimate shared home, right
/// beside `check_check` above (which validates an already-built `Check`'s
/// CONTENT; this validates whether the four flat arguments even describe one
/// coherent `Check` at all - a different job).
///
/// `(None, None, None, [])` means "no check at all". Deliberately does NOT
/// re-validate an empty path, a glob character, or an empty single literal -
/// those are `check_check`'s own job (grounds 13/14), already enforced once
/// the resulting item reaches `declare`/`revise`. This function only
/// resolves the ARGUMENT SHAPE: do these four flat values even describe one
/// coherent check? The "omit everything keeps the existing check" and
/// "check_kind '' clears it" conventions on a revise are a layer above this
/// function, decided by the caller (see `mcp::revise`), not by it.
///
/// `check_literal` (singular, one string) and `check_literals` (plural, a
/// repeatable list) are two SEPARATE parameters, never one field carrying an
/// internal delimiter: `path_exists`/`contains`/`absent` take the singular;
/// `absent_all` (the SET form of `absent` - see `Check::AbsentAll`'s own doc
/// comment) takes the plural. A delimited single string was rejected on
/// purpose: the literals this exists for are punctuation characters (an em
/// dash, curly quotes, an ellipsis, ...), so any delimiter chosen could
/// itself be one of the literals being listed, and an agent would then have
/// to get escaping right to name its own forbidden character - exactly the
/// syntax burden this shape avoids. A repeatable field needs no syntax at
/// all: each entry travels whole and verbatim, the same way `--moment` and
/// `--target` already repeat on the CLI side, and the same way a plain JSON
/// array already works as an MCP tool argument.
///
/// Refuses, by name, exactly the eight ways the four fields can disagree
/// with each other (plus a ninth, for completeness: an unrecognised
/// check_kind name):
/// - a `check_path`/`check_literal`/`check_literals` given with no
///   `check_kind`.
/// - `check_kind` given with no `check_path` - every kind EXCEPT
///   `forbidden`, which takes no path at all (see the next two grounds).
/// - both `check_literal` and `check_literals` given together - a check is
///   either a single literal or a set of them, never both at once.
/// - `path_exists` given together with a `check_literal` or `check_literals`.
/// - `contains`/`absent` given `check_literals` (the set form) instead of a
///   single `check_literal`.
/// - `contains`/`absent` given with no `check_literal`.
/// - `absent_all` given a single `check_literal` instead of `check_literals`,
///   or given an empty `check_literals` list. Caught HERE, at this
///   argument-shape layer, rather than left solely to `check_check`'s own
///   (content-level) empty-list refusal: a `Vec` has no way to distinguish
///   "never mentioned" from "mentioned, empty" the way `Option<String>`
///   distinguishes `None` from `Some("")`, so both shapes read as "no
///   literals were named" here and get the same argument-shape refusal a
///   missing singular `check_literal` gets above - never silently building
///   an `AbsentAll` with zero literals and letting it fail later for a
///   less specific reason. `check_check`'s own empty-list refusal (ground
///   15) still exists unconditionally underneath this, for any `Check::
///   AbsentAll` built another way (directly, bypassing this translator).
/// - `forbidden` given a `check_path` - unlike every other kind, `forbidden`
///   carries no path at all (see `Check::Forbidden`'s own doc comment), so a
///   supplied one is refused by name rather than silently accepted and
///   dropped, or silently accepted and misread as an anchor it will never
///   actually be.
/// - `forbidden` given a single `check_literal` instead of `check_literals`,
///   or given an empty `check_literals` list - the same shape `absent_all`
///   refuses just above, for the same reason: `forbidden` is a SET form
///   too, never a singular one.
pub fn build_check(
    kind: Option<&str>,
    path: Option<String>,
    literal: Option<String>,
    literals: Vec<String>,
) -> Result<Option<Check>, String> {
    if literal.is_some() && !literals.is_empty() {
        return Err(
            "both check_literal and check_literals were given - check_literal names a single \
             literal, check_literals names a set of them; use exactly one, never both"
                .to_string(),
        );
    }
    let Some(kind) = kind else {
        if path.is_some() || literal.is_some() || !literals.is_empty() {
            return Err(
                "check_path, check_literal or check_literals was given with no check_kind - name \
                 check_kind (one of path_exists, contains, absent, absent_all, forbidden), or drop \
                 the check fields entirely"
                    .to_string(),
            );
        }
        return Ok(None);
    };
    // `forbidden` is the one check_kind that takes no check_path at all - it
    // names a standing prohibition with nothing external to anchor (see
    // `Check::Forbidden`'s own doc comment), so it is handled here, before
    // the "check_path is required" rule just below that exists only for the
    // four path-carrying kinds.
    if kind == "forbidden" {
        if let Some(path) = path {
            return Err(format!(
                "check_kind 'forbidden' was given a check_path ('{path}'), but forbidden carries \
                 no path at all - it is a standing prohibition with nothing to anchor; drop \
                 check_path, or use 'absent_all' instead if you meant to forbid these literals in \
                 one specific file"
            ));
        }
        if literal.is_some() {
            return Err(
                "check_kind 'forbidden' was given check_literal (a single literal), but \
                 'forbidden' takes check_literals (a set), even for a single one - use \
                 check_literals instead"
                    .to_string(),
            );
        }
        if literals.is_empty() {
            return Err(
                "check_kind 'forbidden' was given with no check_literals - name at least one \
                 exact string this rule forbids (repeat check_literals for each one), or drop \
                 check_kind entirely"
                    .to_string(),
            );
        }
        return Ok(Some(Check::Forbidden { literals }));
    }
    let Some(path) = path else {
        return Err(format!(
            "check_kind '{kind}' was given with no check_path - name the exact file this check \
             must look at, or drop check_kind entirely"
        ));
    };
    match kind {
        "path_exists" => {
            if literal.is_some() || !literals.is_empty() {
                return Err(format!(
                    "check_kind 'path_exists' was given together with check_literal or \
                     check_literals - path_exists only tests whether '{path}' exists, so a \
                     literal makes no sense here; drop check_literal/check_literals, or use \
                     'contains'/'absent'/'absent_all' to look for text inside the file instead"
                ));
            }
            Ok(Some(Check::PathExists { path }))
        }
        "contains" | "absent" => {
            if !literals.is_empty() {
                return Err(format!(
                    "check_kind '{kind}' was given check_literals (a set), but '{kind}' takes a \
                     single check_literal, not a set - use check_literal, or switch check_kind to \
                     'absent_all' if you meant to forbid a whole set of literals at once"
                ));
            }
            let Some(literal) = literal else {
                return Err(format!(
                    "check_kind '{kind}' was given with no check_literal - name the exact string \
                     this check must find (or must not find), or drop check_kind entirely"
                ));
            };
            if kind == "contains" {
                Ok(Some(Check::Contains { path, literal }))
            } else {
                Ok(Some(Check::Absent { path, literal }))
            }
        }
        "absent_all" => {
            if literal.is_some() {
                return Err(
                    "check_kind 'absent_all' was given check_literal (a single literal), but \
                     'absent_all' takes check_literals (a set) - use check_literals, or switch \
                     check_kind to 'absent' if you meant a single literal"
                        .to_string(),
                );
            }
            if literals.is_empty() {
                return Err(
                    "check_kind 'absent_all' was given with no check_literals - name at least one \
                     exact string this check must find none of (repeat check_literals for each \
                     one), or drop check_kind entirely"
                        .to_string(),
                );
            }
            Ok(Some(Check::AbsentAll { path, literals }))
        }
        other => Err(format!(
            "'{other}' is not a known check_kind; valid: path_exists, contains, absent, absent_all, forbidden"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Kind, Severity};

    fn base(kind: Kind) -> Item {
        Item {
            id: "id-1".to_string(),
            kind,
            text: "do the thing".to_string(),
            bindings: vec![],
            severity: None,
            // A project by default, for the same reason the falsifier below is
            // valid by default: ground 19 refuses a GLOBAL item that names a
            // source file, and most grounds' tests below need to name one
            // (`src/courier.rs` is the running example for anchors). Without a
            // project here they would all be exercising ground 19 instead of
            // the ground they are named after. Ground 19's own tests clear
            // this explicitly.
            project: Some("test-project".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            // A valid falsifier by default so every OTHER ground's tests
            // below keep exercising the ground they name, not ground 10 -
            // ground 10's own tests override this explicitly (to None or to
            // blank) to prove that check on its own.
            falsifier: Some("the thing turns out to have already been done".to_string()),
            check: None,
        }
    }

    // --------------------------------------------------------- ground 1

    #[test]
    fn a_rule_with_no_binding_is_refused() {
        let item = base(Kind::Rule);
        assert!(declare(&item).is_err());
    }

    // --------------------------------------------------------- ground 2

    #[test]
    fn a_rule_that_expires_is_refused() {
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        item.expires = Some("2027-01-01".to_string());
        assert!(declare(&item).is_err());
    }

    #[test]
    fn an_orientation_that_expires_is_refused() {
        let mut item = base(Kind::Orientation);
        item.expires = Some("2027-01-01".to_string());
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_lookup_that_expires_is_refused() {
        let mut item = base(Kind::Lookup);
        item.key = Some("ko-fi".to_string());
        item.expires = Some("2027-01-01".to_string());
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_report_that_expires_is_accepted() {
        let mut item = base(Kind::Report);
        item.expires = Some("2027-01-01".to_string());
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_chunk_that_expires_is_refused() {
        // A Chunk is archive like a Report, but it does not share a Report's
        // short operational life - it lives until revised, exactly like an
        // Orientation or a Lookup. Only a Report may carry `expires`.
        let mut item = base(Kind::Chunk);
        item.expires = Some("2027-01-01".to_string());
        assert!(declare(&item).is_err());
    }

    // --------------------------------------------------------- ground 3

    #[test]
    fn a_report_bound_to_a_moment_is_refused() {
        let mut item = base(Kind::Report);
        item.bindings = vec![Binding::Moment(Action::Push)];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_report_bound_to_a_target_is_refused() {
        let mut item = base(Kind::Report);
        item.text = "see src/main.rs".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/main.rs".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_report_bound_to_always_is_refused() {
        let mut item = base(Kind::Report);
        item.bindings = vec![Binding::Always];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_chunk_bound_to_a_moment_is_refused() {
        // A Chunk is archive exactly like a Report, at write time too: this
        // is the write-gate half of the guarantee (`serve::rank::select` is
        // the other, independent half - see
        // `a_chunk_can_never_reach_a_gate_even_bypassing_the_write_gate`).
        let mut item = base(Kind::Chunk);
        item.bindings = vec![Binding::Moment(Action::Push)];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_chunk_with_no_binding_is_accepted() {
        // Unlike a Rule (ground 1), a Chunk needs no binding at all - it is
        // never served at a gate, so there is nothing for a binding to do.
        let item = base(Kind::Chunk);
        assert!(declare(&item).is_ok());
    }

    // --------------------------------------------------------- ground 4

    #[test]
    fn a_rule_over_300_chars_is_refused() {
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        item.text = "x".repeat(301);
        assert!(declare(&item).is_err());
    }

    #[test]
    fn an_orientation_over_300_chars_is_refused() {
        let mut item = base(Kind::Orientation);
        item.bindings = vec![Binding::Always];
        item.text = "x".repeat(301);
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_rule_at_exactly_300_chars_is_accepted() {
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        item.text = "x".repeat(300);
        assert!(declare(&item).is_ok());
    }

    // --------------------------------------------------------- ground 5

    #[test]
    fn an_unknown_action_name_is_refused() {
        let err = parse_moment("yolo").unwrap_err();
        assert!(err.problem.contains("yolo"));
        assert!(err.fix.contains("publish"), "names a real action to use instead: {}", err.fix);
    }

    #[test]
    fn parse_moment_accepts_every_known_action_remember_included() {
        // The defect this prevents: a new Action variant landing in the
        // intent crate but staying refused here anyway - e.g. an allowlist
        // copied from ALL_ACTIONS by hand instead of parsing through
        // Action::from_str. Loops every known action, Remember included, so
        // a future variant added the same way is covered automatically
        // without anyone remembering to add a case for it here.
        for action in intent::ALL_ACTIONS {
            let result = parse_moment(action.as_str());
            assert!(result.is_ok(), "{action} should be accepted as a moment binding, like every other known action");
        }
    }

    #[test]
    fn a_rule_bound_to_the_remember_moment_is_accepted() {
        // The defect this prevents: the write gate special-casing which
        // actions a Rule may bind to, so a rule ABOUT how the memory itself
        // gets written could never reach a gate even though every other
        // action can. Goes through the full declare() gate, not just
        // parse_moment, so a ground added later that inspects the binding's
        // action would be caught here too.
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Moment(Action::Remember)];
        assert!(declare(&item).is_ok());
    }

    // --------------------------------------------------------- ground 6

    #[test]
    fn a_lookup_without_key_is_refused() {
        let item = base(Kind::Lookup);
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_lookup_with_key_is_accepted() {
        let mut item = base(Kind::Lookup);
        item.key = Some("release-checklist".to_string());
        assert!(declare(&item).is_ok());
    }

    // -------------------------------------------------------- ground 10

    #[test]
    fn a_rule_with_no_falsifier_is_refused() {
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        item.falsifier = None;
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("falsifier"), "names the missing field: {}", err.problem);
    }

    #[test]
    fn an_orientation_with_no_falsifier_is_refused() {
        let mut item = base(Kind::Orientation);
        item.falsifier = None;
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_blank_falsifier_is_refused_the_same_as_a_missing_one() {
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        item.falsifier = Some("   ".to_string());
        assert!(declare(&item).is_err(), "whitespace-only names nothing, same as no falsifier at all");
    }

    #[test]
    fn a_rule_with_a_falsifier_is_accepted() {
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        item.falsifier = Some("the next force-push to main lands clean with nobody reverting it".to_string());
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_report_needs_no_falsifier_it_already_has_an_expiry() {
        let mut item = base(Kind::Report);
        item.falsifier = None;
        item.expires = Some("2027-01-01".to_string());
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_lookup_needs_no_falsifier() {
        let mut item = base(Kind::Lookup);
        item.falsifier = None;
        item.key = Some("release-checklist".to_string());
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_chunk_needs_no_falsifier() {
        let mut item = base(Kind::Chunk);
        item.falsifier = None;
        assert!(declare(&item).is_ok());
    }

    // --------------------------------------------------------- ground 9 (falsifier half)

    #[test]
    fn a_revise_that_drops_the_falsifier_is_refused() {
        let mut existing = base(Kind::Report);
        existing.falsifier = Some("this incident happens again within a week".to_string());
        existing.expires = Some("2027-01-01".to_string());

        let mut updated = existing.clone();
        updated.falsifier = None;

        let err = revise(&existing, &updated).unwrap_err();
        assert!(err.problem.contains("falsifier"), "names the dropped field: {}", err.problem);
    }

    // --------------------------------------------------------- ground 7

    #[test]
    fn a_target_the_sentence_never_names_is_accepted() {
        // Ground 7 used to refuse this. Measured over 2962 real items it cost
        // 19% of them and every sampled case was correct, so the rule is gone.
        // This test pins its removal: a sentence delivered at a file need not
        // repeat that file's name.
        let mut item = base(Kind::Orientation);
        item.text = "put every import before the first test() call".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Path,
            value: "server/test/consumer-orders.test.js".to_string(),
        }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_target_named_in_text_case_and_separator_insensitively_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = r"see C:\Repo\Src\Server.rs for the handler".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/SERVER.rs".to_string() }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_target_named_by_its_file_name_alone_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "swap-binary.ps1 forces the semantic build and prunes the backups to 3".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Path,
            value: r"C:\Users\someone\thor-sync\swap-binary.ps1".to_string(),
        }];
        assert!(declare(&item).is_ok());
    }

    // --------------------------------------------------------- ground 8

    #[test]
    fn an_empty_target_value_is_refused() {
        let mut item = base(Kind::Orientation);
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "   ".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_glob_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "matches *.rs everywhere".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "*.rs".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_dot_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "applies to . in general".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: ".".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_bare_role_name_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "main.rs wires the cli".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "main.rs".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_directory_qualified_role_name_target_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the entry point is at cli/src/main.rs".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "cli/src/main.rs".to_string() }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_bare_hostname_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "localhost is where the dev server listens".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Host, value: "localhost".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn an_all_interfaces_host_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "binds to 0.0.0.0 in the container".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Host, value: "0.0.0.0".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_qualified_hostname_target_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "quote.nworks3d.be serves the quote portal".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Host, value: "quote.nworks3d.be".to_string() }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_root_route_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "applies to every route under /".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Route, value: "/".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_wildcard_route_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "catches every admin route".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Route, value: "/admin/*".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_specific_route_target_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "PATCH /admin/materials/:id updates one material".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Route, value: "PATCH /admin/materials/:id".to_string() }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_route_with_a_query_string_is_accepted() {
        // '?' is ordinary route syntax (a query string), not a glob wildcard -
        // measured on the real data this migration parked: 2 of 55 http-route
        // items carry a literal '?' and neither is a wildcard.
        let mut item = base(Kind::Orientation);
        item.text = "PUT /admin/orders/:id?preview=1 previews the update".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Route,
            value: "PUT /admin/orders/:id?preview=1".to_string(),
        }];
        assert!(declare(&item).is_ok());
    }

    // --------------------------------------------------------- ground 9

    #[test]
    fn a_revise_that_drops_a_field_is_refused() {
        let mut existing = base(Kind::Orientation);
        existing.bindings = vec![Binding::Always];
        existing.tags = vec!["safety".to_string()];

        let mut updated = existing.clone();
        updated.tags = vec![];

        let err = revise(&existing, &updated).unwrap_err();
        assert!(err.problem.contains("tags"), "names the dropped field: {}", err.problem);
    }

    #[test]
    fn a_revise_that_keeps_every_field_is_accepted() {
        let mut existing = base(Kind::Orientation);
        existing.bindings = vec![Binding::Always];
        existing.tags = vec!["safety".to_string()];

        let mut updated = existing.clone();
        updated.text = "do the thing, revised".to_string();

        assert!(revise(&existing, &updated).is_ok());
    }

    // -------------------------------------------------------- ground 11

    #[test]
    fn a_line_number_reference_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "the check lives at line 93 in the gate".to_string();
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("line 93"), "names the exact reference: {}", err.problem);
    }

    #[test]
    fn a_dutch_regel_reference_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "de controle staat op regel 93 in de poort".to_string();
        assert!(declare(&item).is_err());
    }

    #[test]
    fn an_l_number_reference_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "see gate.rs L93 for the check".to_string();
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_file_line_suffix_reference_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "the panic happened at guard.rs:610".to_string();
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_version_number_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the release shipped as v0.9.108".to_string();
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_percentage_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the cache hit rate is 93% under normal load".to_string();
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_byte_count_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the payload is 610 bytes on the wire".to_string();
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_date_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "this took effect on 2026-08-05".to_string();
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_time_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the nightly job runs at 09:30 daily".to_string();
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn an_ip_and_port_is_accepted() {
        // The trickiest case: an IP address has a '.' just like a filename
        // does, so the extension whitelist (letters only) is what keeps this
        // from being misread as "file.ext:line".
        let mut item = base(Kind::Orientation);
        item.text = "the service binds to 127.0.0.1:8080 in the container".to_string();
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_hostname_and_port_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "quote.nworks3d.be:8080 serves the quote portal".to_string();
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_report_with_a_line_number_reference_is_accepted() {
        // A Report never reaches a gate (Kind::can_fire is false), so ground
        // 11 - like the char limit and the falsifier requirement - never
        // applies to one: a line number in an archived write-up is not the
        // same defect as one baked into a standing Rule or Orientation.
        let mut item = base(Kind::Report);
        item.text = "the crash happened at guard.rs:610".to_string();
        assert!(declare(&item).is_ok());
    }

    // ------------------------------------------------ ground 11 (continued)

    #[test]
    fn a_path_target_with_a_line_number_is_refused() {
        // The real defect this closes: a census of the store found an item
        // anchored to the target value "thor/src/courier.rs:716" - a line
        // number in the ANCHOR, not just the text. Worse than the text case,
        // because the anchor decides whether the item ever fires at all, and
        // this value can never match a real file path.
        let mut item = base(Kind::Orientation);
        item.text = "the courier retry backoff is documented here".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Path,
            value: "thor/src/courier.rs:716".to_string(),
        }];
        let err = declare(&item).unwrap_err();
        assert!(
            err.problem.contains("thor/src/courier.rs:716"),
            "names the offending anchor value: {}",
            err.problem
        );
    }

    #[test]
    fn a_dir_target_with_a_line_number_is_refused() {
        // A Dir value need not carry a recognised file extension for this to
        // fire - the "line N" / Dutch "regel N" / "LN" forms the helper also
        // recognises do not depend on one, only the "file.ext:N" suffix form
        // does.
        let mut item = base(Kind::Orientation);
        item.text = "the retry helpers live under this directory".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Dir,
            value: "thor/src L716".to_string(),
        }];
        let err = declare(&item).unwrap_err();
        assert!(
            err.problem.contains("thor/src L716"),
            "names the offending anchor value: {}",
            err.problem
        );
    }

    #[test]
    fn a_path_target_without_a_line_number_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the courier retry backoff is documented here".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Path,
            value: "thor/src/courier.rs".to_string(),
        }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_host_target_with_a_port_is_accepted() {
        // Same colon-and-digits shape as the census example, but on a Host
        // binding: the new check is gated to Path/Dir only, so a host:port
        // must still pass through untouched.
        let mut item = base(Kind::Orientation);
        item.text = "the quote portal listens here".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Host,
            value: "quote.nworks3d.be:8080".to_string(),
        }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_command_target_with_a_colon_and_digits_is_accepted() {
        // Adversarial: "sh" is a recognised source-file extension, so this
        // value WOULD match the line-number helper's "file.ext:N" form if
        // the new check were not gated to Path/Dir. It must still be
        // accepted, because the binding kind here is Command.
        let mut item = base(Kind::Orientation);
        item.text = "run the deploy script to restart the service".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Command,
            value: "scripts/deploy.sh:8080".to_string(),
        }];
        assert!(declare(&item).is_ok());
    }

    // -------------------------------------------------------- ground 16
    //
    // THE DEFECT THIS CLOSES: a census of the real store found 29 anchors
    // that can never fire because their Target value has one of three
    // shapes - a web route stored as a Path, a value that is not a path at
    // all, or an unexpanded environment-variable template - and nothing
    // told whoever wrote them; they stay silent forever. Each test below
    // names the value in the refusal, per this file's own doctrine.

    #[test]
    fn an_env_template_path_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "the guard rulebook lives in the local app data folder".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Path,
            value: r"%LOCALAPPDATA%\thor\guard-rulebook.json".to_string(),
        }];
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("environment-variable"), "names what is wrong: {}", err.problem);
        assert!(err.problem.contains("LOCALAPPDATA"), "names the offending value: {}", err.problem);
    }

    #[test]
    fn an_env_template_dir_target_is_refused() {
        // EnvTemplate carries no Dir exception - an unexpanded template is
        // exactly as wrong for a directory as it is for a file.
        let mut item = base(Kind::Orientation);
        item.text = "the cache directory lives under the local app data folder".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: r"%LOCALAPPDATA%\thor\cache".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_route_like_path_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "the admin panel lives behind this route".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "/admin".to_string() }];
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("web route"), "names what is wrong: {}", err.problem);
        assert!(err.fix.contains("Route"), "says to use Target kind Route instead: {}", err.fix);
    }

    #[test]
    fn a_route_like_dir_target_is_refused() {
        // Route carries no Dir exception either - a single-leading-slash
        // route shape is exactly as wrong for a directory as for a file.
        let mut item = base(Kind::Orientation);
        item.text = "everything behind this route is off limits".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: "/admin".to_string() }];
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_bare_word_path_target_is_refused() {
        let mut item = base(Kind::Orientation);
        item.text = "the symbol gate decides every write-time refusal".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "gate".to_string() }];
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("no path separator"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn a_bare_word_dir_target_is_accepted() {
        // THE DEFECT THIS PREVENTS: a directory has no "file extension" to
        // begin with, so refusing a bare, unqualified directory name would
        // refuse the ORDINARY way this store already anchors a real rule
        // (see serve's own "frozen"/"node_modules"/".git" fixtures) - a
        // wrongly refused legitimate anchor, which this ground must never
        // produce.
        let mut item = base(Kind::Orientation);
        item.text = "frozen is a frozen archive and must never be written to".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: "frozen".to_string() }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_unc_path_target_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the shared site content lives on the vault share".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Path,
            value: r"\\fileserver\web\site\index.html".to_string(),
        }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_multi_segment_posix_absolute_path_target_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the nightly cron job is configured here".to_string();
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "/etc/crontab".to_string() }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_windows_absolute_path_target_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.text = "the handler is defined on the other machine's checkout".to_string();
        item.bindings = vec![Binding::Target {
            kind: TargetKind::Path,
            value: r"C:\Users\other\dev\thor2\model\src\gate.rs".to_string(),
        }];
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn ground_16_is_gated_to_path_and_dir_only() {
        // A leading slash is ordinary syntax for a Route (it is SUPPOSED to
        // start with one), and a bare word is ordinary syntax for a
        // Command or Symbol - ground 16 must never fire for any kind but
        // Path/Dir, mirroring ground 11 (continued)'s own gating.
        let mut route_item = base(Kind::Orientation);
        route_item.text = "the admin route is documented here".to_string();
        route_item.bindings = vec![Binding::Target { kind: TargetKind::Route, value: "/admin".to_string() }];
        assert!(declare(&route_item).is_ok());

        let mut command_item = base(Kind::Orientation);
        command_item.text = "gate is the command that runs the write-time checks".to_string();
        command_item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "gate".to_string() }];
        assert!(declare(&command_item).is_ok());
    }

    // -------------------------------------------------------- ground 12

    #[test]
    fn a_report_with_a_check_is_refused() {
        let mut item = base(Kind::Report);
        item.falsifier = None;
        item.expires = Some("2027-01-01".to_string());
        item.check = Some(Check::PathExists { path: "README.md".to_string() });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("check"), "names the offending field: {}", err.problem);
    }

    #[test]
    fn a_lookup_with_a_check_is_refused() {
        let mut item = base(Kind::Lookup);
        item.key = Some("release-checklist".to_string());
        item.check = Some(Check::PathExists { path: "README.md".to_string() });
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_chunk_with_a_check_is_refused() {
        let mut item = base(Kind::Chunk);
        item.falsifier = None;
        item.check = Some(Check::PathExists { path: "README.md".to_string() });
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_rule_with_a_check_is_accepted() {
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        item.check = Some(Check::PathExists { path: "README.md".to_string() });
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn an_orientation_with_a_check_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Contains { path: "README.md".to_string(), literal: "GPLv3".to_string() });
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_rule_with_no_check_is_still_accepted() {
        // A check is optional even for a kind that can fire - unlike
        // falsifier (ground 10), which IS required for these two kinds.
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        assert!(item.check.is_none());
        assert!(declare(&item).is_ok());
    }

    // -------------------------------------------------------- ground 13

    #[test]
    fn a_check_with_an_empty_path_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::PathExists { path: "   ".to_string() });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("path"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn a_check_with_an_empty_literal_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Contains { path: "README.md".to_string(), literal: "".to_string() });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("literal"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn a_whitespace_only_literal_is_accepted_trimming_would_change_what_it_searches_for() {
        // Unlike the path above, a literal is content to search for
        // verbatim - a whitespace-only literal (a trailing-space check, an
        // exact indent) can be a deliberate thing to look for, so this must
        // NOT be refused the same way an empty-after-trim path is.
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Contains { path: "README.md".to_string(), literal: "  ".to_string() });
        assert!(declare(&item).is_ok());
    }

    // -------------------------------------------------------- ground 14

    #[test]
    fn a_check_path_with_a_glob_star_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::PathExists { path: "src/*.rs".to_string() });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("glob"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn a_check_path_with_a_glob_question_mark_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Contains { path: "src/main?.rs".to_string(), literal: "fn main".to_string() });
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_check_path_without_a_glob_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Absent { path: "src/main.rs".to_string(), literal: "TODO".to_string() });
        assert!(declare(&item).is_ok());
    }

    // -------------------------------------------------------- ground 15
    //
    // AbsentAll's own validation: the path rule is shared unconditionally
    // with the other three forms (proven again here, not just assumed, since
    // `check_check` now extracts `path` through a second match arm added
    // for this variant), and the literal LIST gets its own two refusals no
    // single-literal form needed - an empty list, and an empty entry inside
    // an otherwise non-empty one.

    // GROUND 18 - a literal that cannot prove anything. The two acceptance
    // tests below matter more than the refusal: they pin the exact line, and
    // a naive length rule would break both.

    /// THE DEFECT THIS PREVENTS: a check whose literal is in nearly every
    /// file. It would always hold, never report, and look exactly like
    /// protection - worse than no check, because with no check you at least
    /// know you have nothing.
    #[test]
    fn a_check_on_a_short_ordinary_fragment_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Contains { path: "src/lib.rs".to_string(), literal: "fn".to_string() });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("fn"), "names the literal: {}", err.problem);
    }

    /// THE DEFECT THIS PREVENTS: refusing the single most valuable literal
    /// this system has. A typography rule IS a set of one-character
    /// literals, so any rule phrased as "too short to be distinctive" would
    /// refuse the clearest real use there is.
    #[test]
    fn a_single_punctuation_character_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Absent { path: "docs/STYLE.md".to_string(), literal: "\u{2014}".to_string() });
        assert!(declare(&item).is_ok());
    }

    /// THE DEFECT THIS PREVENTS: refusing a deliberate whitespace check. A
    /// trailing space or an exact indent is a real thing to search for, as
    /// `check_single_literal`'s own doc comment already says.
    #[test]
    fn a_whitespace_literal_is_still_accepted() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Absent { path: "docs/STYLE.md".to_string(), literal: " \n".to_string() });
        assert!(declare(&item).is_ok());
    }

    /// THE DEFECT THIS PREVENTS: one useless member letting a whole set
    /// hold forever. The rule applies per entry, not just to a single
    /// literal.
    #[test]
    fn one_useless_entry_refuses_the_whole_set() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::AbsentAll {
            path: "docs/STYLE.md".to_string(),
            literals: vec!["\u{2014}".to_string(), "if".to_string()],
        });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("if"), "names the offending entry: {}", err.problem);
    }

    #[test]
    fn an_absent_all_check_with_an_empty_literal_list_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::AbsentAll { path: "docs/STYLE.md".to_string(), literals: vec![] });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("empty"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn an_absent_all_check_with_an_empty_literal_inside_the_list_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::AbsentAll {
            path: "docs/STYLE.md".to_string(),
            literals: vec!["\u{2014}".to_string(), "".to_string()],
        });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("empty"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn an_absent_all_check_with_an_empty_path_is_refused() {
        // Same path rule as the other three forms, proven again for
        // AbsentAll specifically since `check_check` now extracts `path`
        // through its own match arm for this variant.
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::AbsentAll { path: "   ".to_string(), literals: vec!["TODO".to_string()] });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("path"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn an_absent_all_check_path_with_a_glob_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::AbsentAll { path: "src/*.rs".to_string(), literals: vec!["TODO".to_string()] });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("glob"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn an_absent_all_check_with_a_single_non_empty_literal_is_accepted() {
        // A list of one is not itself a refusal ground - the SET form used
        // with exactly one literal is an unusual way to write what Absent
        // already says, not a malformed request.
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::AbsentAll { path: "docs/STYLE.md".to_string(), literals: vec!["TODO".to_string()] });
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn an_absent_all_check_with_several_non_empty_literals_is_accepted() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::AbsentAll {
            path: "docs/STYLE.md".to_string(),
            literals: vec![
                "\u{2014}".to_string(),
                "\u{2013}".to_string(),
                "\u{2212}".to_string(),
                "\u{2018}".to_string(),
                "\u{2019}".to_string(),
                "\u{2026}".to_string(),
            ],
        });
        assert!(declare(&item).is_ok());
    }

    // -------------------------------------------------------- ground 17
    //
    // Forbidden's own validation: no path rule at all - Forbidden has no
    // path field to validate in the first place (see its own doc comment on
    // `Check`) - only the literal LIST rule it shares with AbsentAll via
    // `check_literal_list`: an empty list, and an empty entry inside an
    // otherwise non-empty one.

    #[test]
    fn a_forbidden_check_with_an_empty_literal_list_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Forbidden { literals: vec![] });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("empty"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn a_forbidden_check_with_an_empty_literal_inside_the_list_is_refused() {
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Forbidden { literals: vec!["\u{2014}".to_string(), "".to_string()] });
        let err = declare(&item).unwrap_err();
        assert!(err.problem.contains("empty"), "names what is wrong: {}", err.problem);
    }

    #[test]
    fn a_forbidden_check_with_a_single_non_empty_literal_is_accepted() {
        // A list of one is not itself a refusal ground - the SET form used
        // with exactly one literal is an unusual way to write a fact with
        // no set at all, not a malformed request.
        let mut item = base(Kind::Orientation);
        item.check = Some(Check::Forbidden { literals: vec!["TODO".to_string()] });
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_forbidden_check_with_several_non_empty_literals_is_accepted() {
        // The concrete case this variant exists for: the owner's typography
        // rule, six literals, one check, no file to anchor any of them to.
        let mut item = base(Kind::Rule);
        item.bindings = vec![Binding::Always];
        item.check = Some(Check::Forbidden {
            literals: vec![
                "\u{2014}".to_string(),
                "\u{2013}".to_string(),
                "\u{2212}".to_string(),
                "\u{2018}".to_string(),
                "\u{2019}".to_string(),
                "\u{2026}".to_string(),
            ],
        });
        assert!(declare(&item).is_ok());
    }

    // -------------------------------------------- ground 9 (check half)

    #[test]
    fn a_revise_that_drops_the_check_is_refused() {
        let mut existing = base(Kind::Orientation);
        existing.check = Some(Check::PathExists { path: "README.md".to_string() });

        let mut updated = existing.clone();
        updated.check = None;

        let err = revise(&existing, &updated).unwrap_err();
        assert!(err.problem.contains("check"), "names the dropped field: {}", err.problem);
    }

    #[test]
    fn a_revise_that_changes_the_check_form_is_accepted() {
        // Only a total removal (Some -> None) is a drop; swapping which
        // check form is carried is an ordinary correction, exactly like
        // changing a Target's value is for bindings.
        let mut existing = base(Kind::Orientation);
        existing.check = Some(Check::PathExists { path: "README.md".to_string() });

        let mut updated = existing.clone();
        updated.check = Some(Check::Contains { path: "README.md".to_string(), literal: "GPLv3".to_string() });

        assert!(revise(&existing, &updated).is_ok());
    }

    #[test]
    fn a_revise_that_swaps_a_path_carrying_check_for_a_path_less_forbidden_one_is_accepted() {
        // The same "swap forms, not a drop" rule as the test just above,
        // proven again across the one boundary that is new here: FROM a
        // form that carries a path TO one that carries none at all.
        // `revise`'s ground-9 comparison must see this as an ordinary
        // correction, not as `check` silently vanishing.
        let mut existing = base(Kind::Orientation);
        existing.check =
            Some(Check::AbsentAll { path: "docs/STYLE.md".to_string(), literals: vec!["TODO".to_string()] });

        let mut updated = existing.clone();
        updated.check = Some(Check::Forbidden { literals: vec!["TODO".to_string()] });

        assert!(revise(&existing, &updated).is_ok());
    }

    // -------------------------------------------------------- ground 19
    //
    // THE DEFECT THIS CLOSES, measured on the owner's store (2026-08-07). A
    // fact carrying no project is served in every project, and 421 of them
    // were. Among those: "In THOR 1.0 emit guard.rs enkel additionalContext",
    // "ensure_working_contract (thor/src/cli.rs) zaait...", "scope.rs
    // herschrijft verbatim-UNC-paden", "serve/src/lookup.rs MIN_SIMILARITY is
    // 0.50". Each was written inside THOR's own checkout and then handed to
    // sessions in unrelated projects as general truth. A project filter can
    // never scope out a global item, which is exactly why nothing warned.
    //
    // The acceptance tests below matter as much as the refusals: the same
    // measurement found 46 global items naming settings.json, CLAUDE.md,
    // check-private-data.sh, docker-compose.yml or Chart.js, and every one of
    // those was correctly global.

    fn global(kind: Kind, text: &str) -> Item {
        let mut item = base(kind);
        item.project = None;
        item.bindings = vec![Binding::Always];
        item.text = text.to_string();
        item
    }

    #[test]
    fn a_global_rule_naming_a_rust_source_file_is_refused() {
        let item = global(Kind::Rule, "In THOR 1.0 emit guard.rs enkel additionalContext, nooit een permissionDecision.");
        let refusal = declare(&item).unwrap_err();
        assert!(refusal.problem.contains("guard.rs"), "the refusal must quote what it saw: {refusal}");
        assert!(refusal.fix.contains("project"), "the way out is a project: {refusal}");
    }

    #[test]
    fn a_global_orientation_naming_a_source_path_is_refused() {
        let item = global(Kind::Orientation, "serve/src/lookup.rs MIN_SIMILARITY is 0.50, raised from 0.45.");
        assert!(declare(&item).is_err());
    }

    #[test]
    fn the_same_fact_with_a_project_is_accepted() {
        // The way out, proven: naming the repository the file belongs to is
        // the whole fix. If this ever fails, the ground has become a wall
        // instead of a gate and there is no way to store the fact at all.
        let mut item = global(Kind::Orientation, "serve/src/lookup.rs MIN_SIMILARITY is 0.50, raised from 0.45.");
        item.project = Some("The-AI-memory-bible".to_string());
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_global_report_naming_a_source_file_is_accepted() {
        // Archive material is found by searching, never served unprompted, so
        // it is not this ground's problem - the same gating grounds 4, 10, 11
        // and 12 use.
        let mut item = base(Kind::Report);
        item.project = None;
        item.text = "The measurement lives in serve/src/lookup.rs.".to_string();
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_global_rule_naming_a_config_file_is_accepted() {
        // Measured false-positive class 1: a machine-level fact about a file
        // every project's session passes through. Refusing this would have
        // been 16 false refusals on settings.json alone.
        let item = global(Kind::Rule, "Voeg nooit een nieuwe hook toe aan de globale settings.json zonder expliciete goedkeuring vooraf.");
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_global_rule_naming_a_document_or_a_script_is_accepted() {
        // Measured false-positive classes 2 and 3: documentation and operator
        // scripts. Both are about every repository at once, or about the
        // machine, never about one repository's source.
        for text in [
            "De store wint van elk document, ook van CLAUDE.md en AGENTS.md.",
            "Verifieer voor elke publicatie met scripts/check-private-data.sh plus een strikte grep die 0 geeft.",
            "Bindt een service op een LAN-IP, voeg dan een extra binding toe in docker-compose.yml.",
        ] {
            assert!(declare(&global(Kind::Rule, text)).is_ok(), "must stay writable: {text}");
        }
    }

    #[test]
    fn a_global_rule_naming_a_javascript_library_is_accepted() {
        // Measured false-positive class 4, and the reason js/ts need a
        // directory before they count: a bare ".js" token is a library name
        // far more often than a file. Four of the five bare .js tokens among
        // the owner's global items were this exact library.
        let item = global(Kind::Rule, "Chart.js wikkelt canvas-events in een rAF-throttled proxy, dus een synthetische klik bereikt de handler nooit.");
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_global_rule_naming_a_javascript_file_by_path_is_refused() {
        // The other half of the js/ts rule: with a directory it is a file in
        // one repository again, and the library reading is gone.
        let item = global(Kind::Rule, "server/lib/pdf.js embedt het wordmark-lettertype via registerBrandFonts().");
        assert!(declare(&item).is_err());
    }

    #[test]
    fn a_global_rule_naming_a_bare_role_file_is_accepted() {
        // `main.rs` names every project's entry point and no project's file -
        // the same reason ground 8 refuses it as a BINDING value. Reuses
        // `is_bare_role_name` rather than restating the list.
        let item = global(Kind::Rule, "Elke Rust-crate met een binary heeft een main.rs, en een lib.rs als ze ook een bibliotheek is.");
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_global_rule_naming_a_header_by_wildcard_is_accepted() {
        // Measured false-positive class 5: a toolchain header family, in no
        // repository at all. Found by simulating this ground against the live
        // store before shipping it, not by imagining cases.
        let item = global(Kind::Rule, "Pin lib_deps op de reeks die bij de espressif32-kern hoort, anders faalt de build op een ontbrekende esp32-hal-*.h header.");
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_version_number_is_never_read_as_a_file_name() {
        // "2.0" and "1.7.0" tokenise exactly like "guard.rs" does. A refusal
        // here would make most of the store's own history unwritable.
        let item = global(Kind::Rule, "Werk aan 2.0 eerst lokaal, met rmcp 1.7.0, en push pas wanneer het klaar is.");
        assert!(declare(&item).is_ok());
    }

    #[test]
    fn a_global_revise_that_only_adds_the_project_is_accepted() {
        // The repair path for the 421 items that already exist: the gate must
        // let a global fact naming a source file be corrected INTO a scoped
        // one. If ground 19 refused this too, every one of them would be
        // frozen exactly as it is, which is worse than the defect.
        let existing = global(Kind::Orientation, "scope.rs herschrijft verbatim-UNC-paden in canonical_root.");
        let mut updated = existing.clone();
        updated.project = Some("The-AI-memory-bible".to_string());
        assert!(revise(&existing, &updated).is_ok());
    }

    // ------------------------------------------------------ cleared_fields
    //
    // THE DEFECT THIS CLOSES (task report, 2026-08-06): ground 9 above is
    // unconditional by design - and see the six tests just above for each of
    // check/tags/falsifier being refused with no exception - but the MCP/CLI
    // layer documents an "omit keeps, empty string clears" convention for
    // severity/project/expires/key/falsifier/check that ground 9 had no way
    // to honour: a deliberate clear and an accidental drop looked identical
    // by the time they reached `revise`. `ClearedFields::baseline` closes
    // that gap WITHOUT touching `revise` itself - every test above this
    // section still exercises the exact same unconditional function, byte
    // for byte unchanged. These tests instead prove `baseline` from the
    // caller's side: build the comparison item it hands to the untouched
    // `revise`, and check both halves of the property - a named clear goes
    // through, and an unnamed one is still caught.

    #[test]
    fn cleared_fields_default_is_a_no_op_baseline() {
        // All-false must return `existing` completely unchanged - the exact
        // same behaviour `revise` already has today for every caller that
        // never heard of `ClearedFields` at all.
        let mut existing = base(Kind::Orientation);
        existing.severity = Some(Severity::Costly);
        existing.project = Some("thor2".to_string());
        existing.key = Some("some-key".to_string());
        existing.check = Some(Check::PathExists { path: "README.md".to_string() });

        assert_eq!(ClearedFields::default().baseline(&existing), existing);
    }

    #[test]
    fn a_severity_named_cleared_lets_revise_accept_dropping_it() {
        let mut existing = base(Kind::Orientation);
        existing.severity = Some(Severity::Costly);

        let mut updated = existing.clone();
        updated.severity = None;

        let cleared = ClearedFields { severity: true, ..Default::default() };
        assert!(revise(&cleared.baseline(&existing), &updated).is_ok());
    }

    #[test]
    fn a_project_named_cleared_lets_revise_accept_dropping_it() {
        let mut existing = base(Kind::Orientation);
        existing.project = Some("thor2".to_string());

        let mut updated = existing.clone();
        updated.project = None;

        let cleared = ClearedFields { project: true, ..Default::default() };
        assert!(revise(&cleared.baseline(&existing), &updated).is_ok());
    }

    #[test]
    fn an_expires_named_cleared_lets_revise_accept_dropping_it() {
        let mut existing = base(Kind::Report);
        existing.falsifier = None;
        existing.expires = Some("2027-01-01".to_string());

        let mut updated = existing.clone();
        updated.expires = None;

        let cleared = ClearedFields { expires: true, ..Default::default() };
        assert!(revise(&cleared.baseline(&existing), &updated).is_ok());
    }

    #[test]
    fn a_key_named_cleared_lets_revise_accept_dropping_it() {
        let mut existing = base(Kind::Orientation);
        existing.key = Some("some-key".to_string());

        let mut updated = existing.clone();
        updated.key = None;

        let cleared = ClearedFields { key: true, ..Default::default() };
        assert!(revise(&cleared.baseline(&existing), &updated).is_ok());
    }

    #[test]
    fn a_falsifier_named_cleared_lets_revise_accept_dropping_it() {
        // A Report on purpose: a Rule/Orientation REQUIRES a falsifier
        // (ground 10) regardless of ground 9, so clearing it there is
        // refused by declare(updated) either way - this isolates ground 9.
        let mut existing = base(Kind::Report);
        existing.falsifier = Some("this incident happens again within a week".to_string());
        existing.expires = Some("2027-01-01".to_string());

        let mut updated = existing.clone();
        updated.falsifier = None;

        let cleared = ClearedFields { falsifier: true, ..Default::default() };
        assert!(revise(&cleared.baseline(&existing), &updated).is_ok());
    }

    #[test]
    fn a_check_named_cleared_lets_revise_accept_dropping_it() {
        let mut existing = base(Kind::Orientation);
        existing.check = Some(Check::PathExists { path: "README.md".to_string() });

        let mut updated = existing.clone();
        updated.check = None;

        let cleared = ClearedFields { check: true, ..Default::default() };
        assert!(revise(&cleared.baseline(&existing), &updated).is_ok());
    }

    /// THE OTHER HALF OF THE PROPERTY: naming one field cleared must never
    /// widen the door for any OTHER field. A field that vanishes without
    /// being named in `ClearedFields` is still a silent loss, and ground 9
    /// must still catch it - proving `baseline` narrows to exactly the
    /// fields named `true`, not to "anything in `updated` that is now None".
    #[test]
    fn clearing_one_field_does_not_excuse_an_unnamed_field_from_also_vanishing() {
        let mut existing = base(Kind::Orientation);
        existing.severity = Some(Severity::Costly);
        existing.project = Some("thor2".to_string());

        let mut updated = existing.clone();
        updated.severity = None; // named as cleared, below - fine
        updated.project = None; // NOT named - must still be refused

        let cleared = ClearedFields { severity: true, ..Default::default() };
        let err = revise(&cleared.baseline(&existing), &updated).unwrap_err();
        assert!(err.problem.contains("project"), "names the field that was never granted a clear: {}", err.problem);
    }

    /// A caller may clear more than one field in the same revise; `baseline`
    /// must honour all of them together, not just the first.
    #[test]
    fn two_fields_named_cleared_in_the_same_revise_both_go_through() {
        let mut existing = base(Kind::Orientation);
        existing.severity = Some(Severity::Costly);
        existing.key = Some("some-key".to_string());

        let mut updated = existing.clone();
        updated.severity = None;
        updated.key = None;

        let cleared = ClearedFields { severity: true, key: true, ..Default::default() };
        assert!(revise(&cleared.baseline(&existing), &updated).is_ok());
    }

    // ----------------------------------------------------------- build_check
    //
    // Moved here from `mcp/src/lib.rs` and `model/src/bin/model.rs`, which
    // each carried a byte-for-byte identical copy (see this task's own
    // report): this is now the one place the decision is made, and the one
    // place it is tested. Both call sites keep their OWN tests only for what
    // is specific to them - the MCP tool-argument wiring and the clap flag
    // wiring, respectively - never a second copy of the pure-function cases
    // below.
    //
    // `literals` (the fourth, set-form parameter) is `vec![]` throughout this
    // first block: these are exactly the pre-existing cases, updated only to
    // call through the now four-parameter signature - see `build_check_
    // single_literal_forms_are_unaffected_by_the_new_list_parameter` below
    // for the single test that pins this regression-freeness directly.

    #[test]
    fn build_check_with_all_three_absent_is_none() {
        assert_eq!(build_check(None, None, None, vec![]), Ok(None));
    }

    #[test]
    fn build_check_accepts_path_exists() {
        let check = build_check(Some("path_exists"), Some("README.md".to_string()), None, vec![]).unwrap();
        assert_eq!(check, Some(Check::PathExists { path: "README.md".to_string() }));
    }

    #[test]
    fn build_check_accepts_contains() {
        let check =
            build_check(Some("contains"), Some("LICENSE".to_string()), Some("GPLv3".to_string()), vec![]).unwrap();
        assert_eq!(check, Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }));
    }

    #[test]
    fn build_check_accepts_absent() {
        let check =
            build_check(Some("absent"), Some("config.toml".to_string()), Some("TODO".to_string()), vec![]).unwrap();
        assert_eq!(check, Some(Check::Absent { path: "config.toml".to_string(), literal: "TODO".to_string() }));
    }

    #[test]
    fn build_check_refuses_check_kind_without_check_path() {
        let err = build_check(Some("path_exists"), None, None, vec![]).unwrap_err();
        assert!(err.contains("check_path"), "{err}");
    }

    #[test]
    fn build_check_refuses_contains_without_check_literal() {
        let err = build_check(Some("contains"), Some("README.md".to_string()), None, vec![]).unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
    }

    #[test]
    fn build_check_refuses_absent_without_check_literal() {
        let err = build_check(Some("absent"), Some("README.md".to_string()), None, vec![]).unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
    }

    #[test]
    fn build_check_refuses_path_exists_with_a_check_literal() {
        let err = build_check(Some("path_exists"), Some("README.md".to_string()), Some("GPLv3".to_string()), vec![])
            .unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
        assert!(err.contains("path_exists"), "{err}");
    }

    #[test]
    fn build_check_refuses_check_path_with_no_check_kind() {
        let err = build_check(None, Some("README.md".to_string()), None, vec![]).unwrap_err();
        assert!(err.contains("check_kind"), "{err}");
    }

    #[test]
    fn build_check_refuses_check_literal_with_no_check_kind() {
        let err = build_check(None, None, Some("GPLv3".to_string()), vec![]).unwrap_err();
        assert!(err.contains("check_kind"), "{err}");
    }

    #[test]
    fn build_check_refuses_an_unknown_check_kind_name() {
        let err = build_check(Some("bogus"), Some("README.md".to_string()), None, vec![]).unwrap_err();
        assert!(err.contains("bogus"), "{err}");
    }

    /// THE PROPERTY THE TESTS ABOVE ALREADY PROVE INDIVIDUALLY, PINNED HERE
    /// DIRECTLY IN ONE PLACE (task report on adding the set form): the three
    /// original forms (none, path_exists, contains, absent) must accept and
    /// refuse exactly what they always did, now that the signature carries a
    /// fourth, `absent_all`-only parameter alongside them.
    #[test]
    fn build_check_single_literal_forms_are_unaffected_by_the_new_list_parameter() {
        assert_eq!(build_check(None, None, None, vec![]), Ok(None));
        assert_eq!(
            build_check(Some("path_exists"), Some("README.md".to_string()), None, vec![]),
            Ok(Some(Check::PathExists { path: "README.md".to_string() }))
        );
        assert_eq!(
            build_check(Some("contains"), Some("LICENSE".to_string()), Some("GPLv3".to_string()), vec![]),
            Ok(Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }))
        );
        assert_eq!(
            build_check(Some("absent"), Some("config.toml".to_string()), Some("TODO".to_string()), vec![]),
            Ok(Some(Check::Absent { path: "config.toml".to_string(), literal: "TODO".to_string() }))
        );
    }

    // ------------------------------------------------- build_check: absent_all
    //
    // The set form's own argument-shape rules, mirroring the single-literal
    // section above one refusal ground at a time - each test named after the
    // exact defect it prevents, this file's own doctrine.

    #[test]
    fn build_check_accepts_absent_all_with_a_single_literal() {
        // A list of one is not itself a refusal ground - see check_check's
        // own ground 15 tests for the same acceptance one layer down.
        let check =
            build_check(Some("absent_all"), Some("docs/STYLE.md".to_string()), None, vec!["TODO".to_string()])
                .unwrap();
        assert_eq!(check, Some(Check::AbsentAll { path: "docs/STYLE.md".to_string(), literals: vec!["TODO".to_string()] }));
    }

    #[test]
    fn build_check_accepts_absent_all_with_the_real_typography_rule_literal_set() {
        // The concrete case this whole feature exists for: the owner's
        // typography rule forbids six punctuation characters as ONE rule,
        // not six near-duplicate items - see `Check::AbsentAll`'s own doc
        // comment and this task's own report.
        let literals: Vec<String> = vec![
            "\u{2014}".to_string(), // em dash
            "\u{2013}".to_string(), // en dash
            "\u{2212}".to_string(), // minus glyph
            "\u{2018}".to_string(), // curly single quote (open)
            "\u{2019}".to_string(), // curly single quote (close)
            "\u{2026}".to_string(), // ellipsis glyph
        ];
        let check =
            build_check(Some("absent_all"), Some("docs/STYLE.md".to_string()), None, literals.clone()).unwrap();
        assert_eq!(check, Some(Check::AbsentAll { path: "docs/STYLE.md".to_string(), literals }));
    }

    #[test]
    fn build_check_refuses_absent_all_with_no_check_literals() {
        // Caught HERE, at the argument-shape layer - see build_check's own
        // doc comment for why an empty list is refused at this layer rather
        // than left solely to check_check's (content-level) ground 15.
        let err = build_check(Some("absent_all"), Some("docs/STYLE.md".to_string()), None, vec![]).unwrap_err();
        assert!(err.contains("check_literals"), "names the missing field: {err}");
        assert!(err.contains("absent_all"), "{err}");
    }

    #[test]
    fn build_check_refuses_absent_all_given_a_single_check_literal_instead_of_a_list() {
        let err = build_check(
            Some("absent_all"),
            Some("docs/STYLE.md".to_string()),
            Some("TODO".to_string()),
            vec![],
        )
        .unwrap_err();
        assert!(err.contains("check_literals"), "names the field to use instead: {err}");
        assert!(err.contains("absent_all"), "{err}");
    }

    #[test]
    fn build_check_refuses_check_literals_given_to_contains_instead_of_check_literal() {
        let err = build_check(
            Some("contains"),
            Some("README.md".to_string()),
            None,
            vec!["GPLv3".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
        assert!(err.contains("contains"), "{err}");
    }

    #[test]
    fn build_check_refuses_check_literals_given_to_absent_instead_of_check_literal() {
        let err = build_check(
            Some("absent"),
            Some("config.toml".to_string()),
            None,
            vec!["TODO".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
        assert!(err.contains("absent"), "{err}");
    }

    #[test]
    fn build_check_refuses_path_exists_with_check_literals() {
        let err = build_check(
            Some("path_exists"),
            Some("README.md".to_string()),
            None,
            vec!["GPLv3".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
        assert!(err.contains("path_exists"), "{err}");
    }

    #[test]
    fn build_check_refuses_giving_both_check_literal_and_check_literals_together() {
        // THE DEFECT THIS PREVENTS: a caller naming both a single literal AND
        // a set at once has a self-contradictory request - silently picking
        // one and ignoring the other would be exactly the kind of guessing
        // this gate refuses everywhere else.
        let err = build_check(
            Some("absent_all"),
            Some("docs/STYLE.md".to_string()),
            Some("TODO".to_string()),
            vec!["FIXME".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
        assert!(err.contains("check_literals"), "{err}");
    }

    #[test]
    fn build_check_refuses_both_literal_forms_together_even_with_no_check_kind_at_all() {
        // The "both given" refusal fires unconditionally, before check_kind
        // is even inspected - proven separately from the case above (which
        // names a real check_kind) so a future reordering of these checks
        // cannot accidentally make the both-given refusal depend on kind
        // being present first.
        let err =
            build_check(None, None, Some("TODO".to_string()), vec!["FIXME".to_string()]).unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
        assert!(err.contains("check_literals"), "{err}");
    }

    #[test]
    fn build_check_refuses_check_literals_with_no_check_kind() {
        let err = build_check(None, None, None, vec!["GPLv3".to_string()]).unwrap_err();
        assert!(err.contains("check_kind"), "{err}");
    }

    // -------------------------------------------------- build_check: forbidden
    //
    // The path-less SET form's own argument-shape rules. Mirrors the
    // absent_all section above one refusal ground at a time, plus the one
    // ground unique to this kind: unlike every other kind, `forbidden`
    // REFUSES a check_path rather than requiring one - each test below is
    // named after the exact defect it prevents, this file's own doctrine.

    #[test]
    fn build_check_accepts_forbidden_with_a_single_literal_and_no_path() {
        // THE CORE NEW CAPABILITY: every other check_kind refuses a missing
        // check_path (see build_check_refuses_check_kind_without_check_path
        // above) - forbidden is the one kind where OMITTING check_path is
        // the correct, accepted shape.
        let check = build_check(Some("forbidden"), None, None, vec!["TODO".to_string()]).unwrap();
        assert_eq!(check, Some(Check::Forbidden { literals: vec!["TODO".to_string()] }));
    }

    #[test]
    fn build_check_accepts_forbidden_with_the_real_typography_rule_literal_set() {
        // The concrete case this whole variant exists for: the owner's
        // typography rule forbids six punctuation characters as ONE rule,
        // with no file to anchor any of them to - see `Check::Forbidden`'s
        // own doc comment and this task's own report. Same literal set as
        // `build_check_accepts_absent_all_with_the_real_typography_rule_
        // literal_set` above, this time with no check_path at all.
        let literals: Vec<String> = vec![
            "\u{2014}".to_string(), // em dash
            "\u{2013}".to_string(), // en dash
            "\u{2212}".to_string(), // minus glyph
            "\u{2018}".to_string(), // curly single quote (open)
            "\u{2019}".to_string(), // curly single quote (close)
            "\u{2026}".to_string(), // ellipsis glyph
        ];
        let check = build_check(Some("forbidden"), None, None, literals.clone()).unwrap();
        assert_eq!(check, Some(Check::Forbidden { literals }));
    }

    #[test]
    fn build_check_refuses_forbidden_with_no_check_literals() {
        let err = build_check(Some("forbidden"), None, None, vec![]).unwrap_err();
        assert!(err.contains("check_literals"), "names the missing field: {err}");
        assert!(err.contains("forbidden"), "{err}");
    }

    #[test]
    fn build_check_refuses_forbidden_given_a_single_check_literal_instead_of_a_list() {
        let err = build_check(Some("forbidden"), None, Some("TODO".to_string()), vec![]).unwrap_err();
        assert!(err.contains("check_literals"), "names the field to use instead: {err}");
        assert!(err.contains("forbidden"), "{err}");
    }

    #[test]
    fn build_check_refuses_forbidden_given_a_check_path() {
        // THE GROUND UNIQUE TO THIS KIND: forbidden carries no path at all
        // (see `Check::Forbidden`'s own doc comment) - a supplied check_path
        // must be refused by name, never silently accepted and dropped.
        let err = build_check(Some("forbidden"), Some("docs/STYLE.md".to_string()), None, vec!["TODO".to_string()])
            .unwrap_err();
        assert!(err.contains("check_path"), "names the offending field: {err}");
        assert!(err.contains("forbidden"), "{err}");
    }

    #[test]
    fn build_check_refuses_giving_both_check_literal_and_check_literals_to_forbidden() {
        let err = build_check(
            Some("forbidden"),
            None,
            Some("TODO".to_string()),
            vec!["FIXME".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("check_literal"), "{err}");
        assert!(err.contains("check_literals"), "{err}");
    }

    #[test]
    fn build_check_single_literal_and_absent_all_forms_are_unaffected_by_forbidden() {
        // Mirrors build_check_single_literal_forms_are_unaffected_by_the_
        // new_list_parameter above: every kind that existed before
        // `forbidden` must accept and refuse exactly what it always did,
        // now that a fifth, path-less kind sits alongside them.
        assert_eq!(build_check(None, None, None, vec![]), Ok(None));
        assert_eq!(
            build_check(Some("path_exists"), Some("README.md".to_string()), None, vec![]),
            Ok(Some(Check::PathExists { path: "README.md".to_string() }))
        );
        assert_eq!(
            build_check(Some("contains"), Some("LICENSE".to_string()), Some("GPLv3".to_string()), vec![]),
            Ok(Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }))
        );
        assert_eq!(
            build_check(Some("absent"), Some("config.toml".to_string()), Some("TODO".to_string()), vec![]),
            Ok(Some(Check::Absent { path: "config.toml".to_string(), literal: "TODO".to_string() }))
        );
        assert_eq!(
            build_check(Some("absent_all"), Some("docs/STYLE.md".to_string()), None, vec!["TODO".to_string()]),
            Ok(Some(Check::AbsentAll { path: "docs/STYLE.md".to_string(), literals: vec!["TODO".to_string()] }))
        );
    }
}
