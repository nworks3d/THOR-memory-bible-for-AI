//! Read-only analysis: which LIVE Rule/Orientation checks (see
//! `model::item::Check`, `model::check::run`) contradict another live
//! item's own check, on the SAME file. PHASE 1 of the contradiction-
//! detection work - this only measures and reports, it writes nothing to
//! the store and refuses nothing at write time. Whether a write-gate ground
//! is worth building on top of this depends entirely on what this run finds
//! on the owner's real store (see this project's own session report for
//! those numbers and the judgement made on each flagged pair).
//!
//! THE IDEA. A note may carry a machine-runnable check alongside its prose
//! falsifier. Two notes whose checks cannot both hold at once, or whose
//! checks currently disagree about the same file, are worth a look - not by
//! guessing at wording, but by running the proofs themselves.
//!
//! TWO WAYS TWO CHECKS ON THE SAME FILE CAN CONTRADICT, kept strictly
//! separate because they are decided two different ways and carry two
//! different confidences:
//!
//!   (a) DIRECT OPPOSITION - purely structural, decided by the two checks'
//!       own SHAPE, never by reading the file: one side's check requires an
//!       exact literal to be PRESENT (`Contains`; `Requires` in principle -
//!       see "REQUIRES CANNOT ACTUALLY REACH HERE" below) while the other
//!       requires the exact SAME literal ABSENT (`Absent`, `AbsentAll`).
//!       These two conditions can never both be true, for any content the
//!       file could ever hold, so deciding this needs no root and no
//!       filesystem read - a root is only used afterwards, to also report
//!       what each side's check happens to produce RIGHT NOW, for the
//!       record.
//!   (b) BOTH RUNNABLE, DISAGREEING NOW - empirical, decided by actually
//!       running both checks against a real checkout: neither comes back
//!       `CannotRun`, and one comes back `Holds` while the other comes back
//!       `Fails`. This says nothing about whether the two checks could ever
//!       coexist in principle (unlike (a)) - only that, on the file as it
//!       stands right now, they disagree. A much noisier signal than (a):
//!       two checks can legitimately read differently on the same file
//!       without either one being wrong (a `PathExists` and a `Contains` on
//!       the same file ask unrelated questions and can easily land on
//!       different answers with nothing wrong on either side).
//!
//! Every other same-file pair is class (c): not a contradiction, and never
//! reported as one.
//!
//! REQUIRES CANNOT ACTUALLY REACH HERE. `Check::Requires` carries no `path`
//! at all (`model::check::named_path` returns `None` for it, exactly like
//! `Forbidden`) - it is conditional on a CALL, never on a file's content -
//! so it can never share a "same file" pairing with anything and never
//! actually reaches `direct_opposition` through this tool's own pairing loop
//! below. `model::gate::opposing_literals`'s own `positive_literals` helper
//! still answers for it, exhaustively, for the same reason `model::check::
//! run`'s own `Forbidden`/`Requires` match arms stay real code rather than
//! `unreachable!()`: correct even if some future change ever gave `Requires`
//! a path to resolve, and it costs nothing to leave written out rather than
//! folded into a catch-all.
//!
//! PAIRING - "the SAME file" means: `model::check::named_path` returns
//! `Some` for both sides (so `Forbidden`/`Requires` never pair with
//! anything), the two paths are equal once each is put through
//! `model::normalize::normalize_target` - reused, never a second hand-rolled
//! path comparison - AND the two items carry the same project identity, each
//! put through `model::normalize::normalize_project` (`None` matches only
//! `None`). The project half is not optional: `model::store::
//! unsettled_neighbours` already establishes, for the identical reason, that
//! "a path is only unique WITHIN a project" - `README.md` names a different
//! real file in every checkout, so pairing across projects would invent
//! contradictions between two files that merely share a name.
//!
//! PROJECT RESOLUTION for running a check (needed for (b), and for the
//! "right now" outcome this tool prints even on an (a) pair) - resolved
//! EXACTLY like `ops::health::decay_line` and `serve/examples/check_lint.rs`:
//! every immediate subdirectory of CHECK_CONTRADICTIONS_CHECKOUTS is handed
//! to `serve::project::resolve_project`, and whatever project key that
//! returns becomes the checkout a same-named item's relative paths resolve
//! against. A GLOBAL item (`project: None`) is never run against any root -
//! the identical scope `ops::health::decay_check` already places on itself,
//! for the identical reason: a global item names no ONE checkout, so there
//! is no single root running it against would prove anything about. It can
//! still be flagged under (a), which needs no root at all.
//!
//! Read-only throughout. CHECK_CONTRADICTIONS_DB must point at a COPY of a
//! store, never the live one: opened through `EventStore::open_existing`,
//! the same constructor `check_census.rs`/`check_lint.rs`/`dead_anchors.rs`
//! and the inspection commands (doctor, fsck, status) all use, because it
//! does no schema work and no FTS heal on open (see that constructor's own
//! doc comment in `core/src/event_store.rs`). Every store read after that
//! (`serve::live::live_items`) is a SELECT-only reader too, and every
//! filesystem read is `model::check::run` itself, which only ever reads and
//! bounds every file at `model::check::MAX_CHECK_FILE_BYTES`.
//!
//! Neither variable falls back to a default path. Either one missing and
//! this prints why and exits before opening anything - no store open, no
//! directory walked, no check run.
//!
//! SCOPE - only a live item whose kind can fire (`Kind::can_fire`: Rule or
//! Orientation) is ever counted, the same restriction `check_census.rs`/
//! `check_lint.rs`/`stale_anchors.rs` all place on which items matter here: a
//! Report/Lookup/Chunk may never carry a check at all (`gate::declare`,
//! ground 12).
//!
//! Run (from the `thor2` workspace root):
//!   CHECK_CONTRADICTIONS_DB=<path to a COPY of a store> \
//!   CHECK_CONTRADICTIONS_CHECKOUTS=<path to a directory of per-project checkouts> \
//!   cargo run -p serve --example check_contradictions

use model::check::{named_path, run, Outcome};
use model::item::{Check, Kind};
use model::normalize::{normalize_project, normalize_target};
use serve::live::live_items;
use std::collections::BTreeMap;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// One live Rule/Orientation carrying a check, prepared for same-file
/// pairing. `path` is `None` for `Forbidden`/`Requires` (see `model::check::
/// named_path`) - such a candidate still counts toward "carries a check at
/// all" but can never be placed in a pairing group below.
struct Candidate {
    id: String,
    kind: Kind,
    text: String,
    project: Option<String>,
    check: Check,
    path: Option<String>,
}

/// `s` with every newline/carriage return collapsed to a single space -
/// applied to a literal or an item's own text, neither of which this tool
/// controls the shape of, so a value that embeds one can never break this
/// report's one-stanza-per-side shape. Same rule `check_census.rs` uses.
fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// Render a `Check` as one readable line - mirrors `check_census.rs`'s own
/// `describe_check` exactly (each example in this directory is a standalone
/// binary with no shared helper module between them, so this is a
/// deliberate, small, tracked duplication rather than a new cross-example
/// dependency).
fn describe_check(check: &Check) -> String {
    match check {
        Check::PathExists { path } => format!("path_exists({path})"),
        Check::Contains { path, literal } => format!("contains({path}, \"{}\")", one_line(literal)),
        Check::Absent { path, literal } => format!("absent({path}, \"{}\")", one_line(literal)),
        Check::AbsentAll { path, literals } => {
            let quoted: Vec<String> = literals.iter().map(|l| format!("\"{}\"", one_line(l))).collect();
            format!("absent_all({path}, [{}])", quoted.join(", "))
        }
        Check::Forbidden { literals } => {
            let quoted: Vec<String> = literals.iter().map(|l| format!("\"{}\"", one_line(l))).collect();
            format!("forbidden([{}])", quoted.join(", "))
        }
        Check::Requires { when, required } => {
            let quoted: Vec<String> = required.iter().map(|r| format!("\"{}\"", one_line(r))).collect();
            format!("requires(\"{}\" -> [{}])", one_line(when), quoted.join(", "))
        }
    }
}

/// DIRECT OPPOSITION (class a) - see this file's own doc comment. Reuses
/// `model::gate::opposing_literals` (GROUND 29) rather than keeping a
/// second, driftable definition of the same rule here: this census must
/// never disagree with what the write gate would actually refuse today, the
/// same reasoning `duplicate_pairs.rs` already gives for calling `model::
/// store`'s own near-duplicate primitives instead of re-deriving them.
/// Before GROUND 29 existed this file carried its own copy (`positive_
/// literals`/`negative_literals`/`shared_literals`/`direct_opposition`),
/// which is exactly what phase 1 of this work measured; GROUND 29's own doc
/// comment now carries that measurement as its evidence.
fn direct_opposition<'a>(a: &'a Check, b: &'a Check) -> Vec<String> {
    model::gate::opposing_literals(a, b)
}

/// Where a candidate's check would actually be run - or why it cannot be run
/// at all right now. `Global`/`NoCheckout` are never a check FAILING; they
/// are the absence of a root to run it against in the first place, and stay
/// distinct from `model::check::Outcome::CannotRun` for the same reason that
/// enum keeps `CannotRun` apart from `Fails`: "no root to try against" and
/// "tried and could not tell" are different facts.
enum RunResult {
    Ran(Outcome),
    /// The item is global (`project: None`) - see this file's own doc
    /// comment: no single checkout to mean, so no root this tool will guess.
    Global,
    /// The item's project does not resolve to any subdirectory found under
    /// CHECK_CONTRADICTIONS_CHECKOUTS.
    NoCheckout,
}

impl RunResult {
    /// `Some(outcome)` only when the check actually ran and produced a
    /// DECIDED answer (`Holds`/`Fails`) - never for `CannotRun`, and never
    /// for `Global`/`NoCheckout`. The one predicate class (b) is built on:
    /// "both runnable" means both sides answer `Some` here.
    fn decided(&self) -> Option<Outcome> {
        match self {
            RunResult::Ran(o @ (Outcome::Holds | Outcome::Fails)) => Some(*o),
            _ => None,
        }
    }
}

impl std::fmt::Display for RunResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunResult::Ran(Outcome::Holds) => write!(f, "Holds"),
            RunResult::Ran(Outcome::Fails) => write!(f, "Fails"),
            RunResult::Ran(Outcome::CannotRun) => write!(f, "CannotRun"),
            RunResult::Global => write!(f, "unresolved (global item: no single checkout to run it against)"),
            RunResult::NoCheckout => write!(f, "unresolved (project has no matching checkout under CHECKOUTS)"),
        }
    }
}

/// Run `check` against whatever root `project` resolves to under `roots` -
/// see this file's own "PROJECT RESOLUTION" doc comment.
fn run_against(project: &Option<String>, check: &Check, roots: &BTreeMap<String, PathBuf>) -> RunResult {
    match project {
        None => RunResult::Global,
        Some(p) => match roots.get(p) {
            Some(base) => RunResult::Ran(run(check, base)),
            None => RunResult::NoCheckout,
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    DirectOpposition,
    BothRunnableDisagreeing,
    NotAContradiction,
}

/// One flagged (class a or b) pair, held only long enough to print it at the
/// end of `main`.
struct PairResult {
    class: Class,
    a_idx: usize,
    b_idx: usize,
    file: String,
    project: Option<String>,
    shared_literals: Vec<String>,
    outcome_a: String,
    outcome_b: String,
}

fn print_pair(candidates: &[Candidate], p: &PairResult) {
    let a = &candidates[p.a_idx];
    let b = &candidates[p.b_idx];
    println!("{}", "-".repeat(80));
    println!(
        "file: {}  project: {}",
        p.file,
        p.project.as_deref().unwrap_or("(global)")
    );
    if p.shared_literals.is_empty() {
        println!("shared literal(s): n/a (empirical disagreement, not a literal clash)");
    } else {
        println!("shared literal(s): {}", p.shared_literals.iter().map(|l| format!("\"{l}\"")).collect::<Vec<_>>().join(", "));
    }
    println!("a: id={} kind={:?}", a.id, a.kind);
    println!("   check:     {}", describe_check(&a.check));
    println!("   right now: {}", p.outcome_a);
    println!("   text:      {}", one_line(&a.text));
    println!("b: id={} kind={:?}", b.id, b.kind);
    println!("   check:     {}", describe_check(&b.check));
    println!("   right now: {}", p.outcome_b);
    println!("   text:      {}", one_line(&b.text));
}

fn main() -> anyhow::Result<()> {
    let db_var = std::env::var("CHECK_CONTRADICTIONS_DB").ok();
    let checkouts_var = std::env::var("CHECK_CONTRADICTIONS_CHECKOUTS").ok();
    let (Some(db), Some(checkouts)) = (db_var, checkouts_var) else {
        eprintln!(
            "check_contradictions requires two environment variables, neither with a default:\n\
             CHECK_CONTRADICTIONS_DB         - path to a COPY of a store (never the live thor.db)\n\
             CHECK_CONTRADICTIONS_CHECKOUTS  - path to a directory of per-project checkouts, resolved \
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
    // serve/examples/check_lint.rs - see this file's own "PROJECT
    // RESOLUTION" doc comment. A checkouts directory that cannot be read at
    // all degrades to an empty map rather than an error, the same fails-open
    // stance those tools take on this identical read.
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
    let mut candidates: Vec<Candidate> = Vec::new();

    for live in live_items(&store) {
        let item = live.item;
        if !item.kind.can_fire() {
            continue; // only Rule/Orientation can ever carry a check (gate::declare ground 12)
        }
        fireable_total += 1;
        let Some(check) = item.check else { continue };
        let path = named_path(&check).map(normalize_target);
        candidates.push(Candidate {
            id: live.id,
            kind: item.kind,
            text: item.text,
            project: normalize_project(item.project.as_deref()),
            check,
            path,
        });
    }

    let with_check_total = candidates.len();
    let with_path_total = candidates.iter().filter(|c| c.path.is_some()).count();

    // Group by (project, normalised path) - see this file's own "PAIRING"
    // doc comment for exactly why both halves of the key matter.
    let mut groups: BTreeMap<(Option<String>, String), Vec<usize>> = BTreeMap::new();
    for (idx, c) in candidates.iter().enumerate() {
        if let Some(path) = &c.path {
            groups.entry((c.project.clone(), path.clone())).or_default().push(idx);
        }
    }

    let mut pairs_sharing_file = 0usize;
    let mut untestable_no_root = 0usize;
    let mut results: Vec<PairResult> = Vec::new();

    for (key, indices) in &groups {
        if indices.len() < 2 {
            continue;
        }
        let (project, file) = key;
        for i in 0..indices.len() {
            for j in (i + 1)..indices.len() {
                pairs_sharing_file += 1;
                let a_idx = indices[i];
                let b_idx = indices[j];
                let a = &candidates[a_idx];
                let b = &candidates[b_idx];

                let shared = direct_opposition(&a.check, &b.check);
                let run_a = run_against(project, &a.check, &roots);
                let run_b = run_against(project, &b.check, &roots);

                let class = if !shared.is_empty() {
                    Class::DirectOpposition
                } else if let (Some(oa), Some(ob)) = (run_a.decided(), run_b.decided()) {
                    if oa != ob { Class::BothRunnableDisagreeing } else { Class::NotAContradiction }
                } else {
                    if matches!(run_a, RunResult::Global | RunResult::NoCheckout)
                        || matches!(run_b, RunResult::Global | RunResult::NoCheckout)
                    {
                        untestable_no_root += 1;
                    }
                    Class::NotAContradiction
                };

                if class == Class::NotAContradiction {
                    continue;
                }

                results.push(PairResult {
                    class,
                    a_idx,
                    b_idx,
                    file: file.clone(),
                    project: project.clone(),
                    shared_literals: shared,
                    outcome_a: run_a.to_string(),
                    outcome_b: run_b.to_string(),
                });
            }
        }
    }

    let class_a_total = results.iter().filter(|r| r.class == Class::DirectOpposition).count();
    let class_b_total = results.iter().filter(|r| r.class == Class::BothRunnableDisagreeing).count();

    println!("CHECK_CONTRADICTIONS_DB: {}", db.display());
    println!("CHECK_CONTRADICTIONS_CHECKOUTS: {}", checkouts.display());
    println!("resolved checkouts under CHECKOUTS: {} project(s)", roots.len());
    println!();
    println!("live, fireable (Rule/Orientation) items: {fireable_total}");
    println!("of those, carrying a check at all: {with_check_total}");
    println!(
        "  check names no file at all (forbidden/requires - never pairable): {}",
        with_check_total - with_path_total
    );
    println!("  check names a file, eligible for pairing: {with_path_total}");
    println!();
    println!("same-file pairs considered (same normalised path, same project): {pairs_sharing_file}");
    println!("  (a) DIRECT OPPOSITION (structural, no root needed): {class_a_total}");
    println!("  (b) BOTH RUNNABLE, DISAGREEING NOW (needs a resolvable root): {class_b_total}");
    println!(
        "  (c) everything else, not a contradiction: {}",
        pairs_sharing_file - class_a_total - class_b_total
    );
    println!(
        "    of which, could only ever have been tested for (b) and were not, for lack of a \
         resolvable root (a global item, or a project with no matching checkout under \
         CHECKOUTS): {untestable_no_root}"
    );

    if !results.is_empty() {
        println!();
        println!("flagged pairs - class (a), DIRECT OPPOSITION ({class_a_total}):");
        for r in results.iter().filter(|r| r.class == Class::DirectOpposition) {
            print_pair(&candidates, r);
        }
        println!();
        println!("flagged pairs - class (b), BOTH RUNNABLE, DISAGREEING NOW ({class_b_total}):");
        for r in results.iter().filter(|r| r.class == Class::BothRunnableDisagreeing) {
            print_pair(&candidates, r);
        }
    }

    println!();
    println!(
        "class (a) is a structural guarantee: the two conditions cannot both be true, ever, \
         regardless of what the file holds. class (b) is empirical only - it says the two checks \
         disagree on this file RIGHT NOW, never that they could not coexist. Read every (b) pair \
         with that difference in mind before treating it as a contradiction."
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(path: &str, literal: &str) -> Check {
        Check::Contains { path: path.to_string(), literal: literal.to_string() }
    }
    fn absent(path: &str, literal: &str) -> Check {
        Check::Absent { path: path.to_string(), literal: literal.to_string() }
    }

    // -------------------------------------------------------- direct_opposition
    //
    // A single smoke test: `direct_opposition` is now a thin pass-through to
    // `model::gate::opposing_literals` (GROUND 29), which carries the full
    // test matrix (same literal, different literals, AbsentAll sets, both-
    // positive, PathExists/Forbidden never participating, case sensitivity)
    // at its own definition in `model/src/gate.rs`. Re-asserting that whole
    // matrix here would test the identical code twice rather than this
    // file's own wiring - see this file's own doc comment on `direct_
    // opposition` for why the definition moved there.

    #[test]
    fn direct_opposition_forwards_to_gate_opposing_literals() {
        let a = contains("f.md", "GPLv3");
        let b = absent("f.md", "GPLv3");
        assert_eq!(direct_opposition(&a, &b), vec!["GPLv3".to_string()]);
        assert!(direct_opposition(&a, &absent("f.md", "MIT")).is_empty());
    }

    #[test]
    fn forbidden_and_requires_name_no_path_and_can_never_be_paired() {
        // The precondition every pair in main's own loop is built on: only a
        // check with a path can ever land in a same-file group at all.
        assert_eq!(named_path(&Check::Forbidden { literals: vec!["x".to_string()] }), None);
        assert_eq!(
            named_path(&Check::Requires { when: "git commit".to_string(), required: vec!["x".to_string()] }),
            None
        );
    }

    // -------------------------------------------------------------- RunResult

    #[test]
    fn decided_is_none_for_cannot_run() {
        assert_eq!(RunResult::Ran(Outcome::CannotRun).decided(), None);
    }

    #[test]
    fn decided_is_none_for_global_and_no_checkout() {
        assert_eq!(RunResult::Global.decided(), None);
        assert_eq!(RunResult::NoCheckout.decided(), None);
    }

    #[test]
    fn decided_is_some_for_holds_and_fails() {
        assert_eq!(RunResult::Ran(Outcome::Holds).decided(), Some(Outcome::Holds));
        assert_eq!(RunResult::Ran(Outcome::Fails).decided(), Some(Outcome::Fails));
    }
}
