//! Read-only census: runs every live Rule/Orientation's machine-runnable
//! `check` (see `model::item::Check`, `model::check::run`) and reports the
//! result. Nothing in the workspace does this over a whole store today - a
//! check is only ever run one at a time, from a single guard
//! (`serve::absent_guard`) looking at a single anchored file at the moment a
//! write happens. This tool asks the same question of every live item at
//! once: of the ones that carry a check at all, how many still hold, how
//! many now fail, and how many cannot be run right now.
//!
//! Read-only. CHECK_CENSUS_DB must point at a COPY of a store, never the
//! live one. This never writes, creates, migrates or repairs anything in the
//! store it opens: it opens through `EventStore::open_existing`, the same
//! constructor `stale_anchors` (this crate's other read-only census) and the
//! inspection commands (doctor, fsck, status) use, precisely because it does
//! no schema work and no FTS heal on open - see that constructor's own doc
//! comment in `core/src/event_store.rs`. Every store read after that
//! (`serve::live::live_items`) is a SELECT-only reader too.
//!
//! CHECK_CENSUS_ROOT must point at the filesystem root a check's own
//! relative path should be resolved against - normally a checkout, or a copy
//! of one, of the repository the store's checks were originally written
//! against. Nothing under this root is ever written to either: resolving and
//! running every check is entirely `model::check::run`'s own job (never
//! reimplemented here), and that function only ever reads.
//!
//! Neither variable falls back to a default path. Either one missing and
//! this prints why and exits before opening anything - no store open, no
//! check ever run.
//!
//! SCOPE - only a live item whose kind can fire (`Kind::can_fire`: Rule or
//! Orientation) is ever counted, the same restriction `stale_anchors` places
//! on which items its own anchors matter for: a Report/Lookup/Chunk may
//! never carry a check at all (see `gate::declare`, ground 12), so counting
//! one here would only ever add a silent zero.
//!
//! FAILS vs CANNOT RUN - never the same thing, never merged. A FAILS item's
//! check ran and came back false, which means the fact it guards is provably
//! wrong right now. A CANNOT RUN item's check could not be run at all (a
//! missing file for Contains/Absent, an IO error, non-UTF-8 content, a file
//! over the runner's size limit, or a path that does not stay inside
//! CHECK_CENSUS_ROOT) - it proves nothing either way about the fact. See
//! `model::check::Outcome`'s own doc comment for the full definition this
//! tool only ever reads, never redefines.
//!
//! Run (from the `thor2` workspace root):
//!   CHECK_CENSUS_DB=<path to a COPY of a store> \
//!   CHECK_CENSUS_ROOT=<path to a checkout the checks were written against> \
//!   cargo run -p serve --example check_census

use model::check::{run, Outcome};
use model::item::Check;
use serve::live::live_items;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// One live item whose check came back `Fails` or `CannotRun` - held only
/// long enough to be printed at the end of `main`, never past it.
struct CheckedItem {
    id: String,
    check: Check,
    text: String,
}

/// Render a `Check` as one readable line: the variant's own wire name (see
/// `Check`'s `#[serde(rename_all = "snake_case")]`) written as a
/// function-call-shaped label, followed by its path and, for
/// `Contains`/`Absent`, its literal. Never spans more than one line: any
/// newline embedded in the literal is collapsed by `one_line` below, the
/// same way a snippet of an item's own text is.
fn describe_check(check: &Check) -> String {
    match check {
        Check::PathExists { path } => format!("path_exists({path})"),
        Check::Contains { path, literal } => {
            format!("contains({path}, \"{}\")", one_line(literal))
        }
        Check::Absent { path, literal } => format!("absent({path}, \"{}\")", one_line(literal)),
        Check::AbsentAll { path, literals } => {
            let quoted: Vec<String> = literals.iter().map(|literal| format!("\"{}\"", one_line(literal))).collect();
            format!("absent_all({path}, [{}])", quoted.join(", "))
        }
        // No path to print: this form carries none by construction. It names
        // nothing outside itself, so there is nothing that could rot and
        // nothing to resolve - which is exactly why it always holds.
        Check::Forbidden { literals } => {
            let quoted: Vec<String> = literals.iter().map(|literal| format!("\"{}\"", one_line(literal))).collect();
            format!("forbidden([{}])", quoted.join(", "))
        }
    }
}

/// `s` with every newline and carriage return collapsed to a single space.
/// Applied to a literal or an item's own text, neither of which this tool
/// controls the shape of, so a value that happens to embed one can never
/// break the one-row-per-item shape of this tool's output.
fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// The first 80 characters of `text`, counted in Unicode scalar values
/// (never bytes, so this never panics slicing a multi-byte character in
/// two), with embedded newlines collapsed the same way `describe_check`
/// collapses them in a literal.
fn snippet(text: &str) -> String {
    one_line(&text.chars().take(80).collect::<String>())
}

fn main() -> anyhow::Result<()> {
    let db_var = std::env::var("CHECK_CENSUS_DB").ok();
    let root_var = std::env::var("CHECK_CENSUS_ROOT").ok();
    let (Some(db), Some(root)) = (db_var, root_var) else {
        eprintln!(
            "check_census requires two environment variables, neither with a default:\n\
             CHECK_CENSUS_DB   - path to a COPY of a store (never the live thor.db)\n\
             CHECK_CENSUS_ROOT - path to a checkout, or a copy of one, of the repository the \
             checks resolve against\n\
             At least one is unset, so this exits now: no store has been opened, nothing has \
             been read or run."
        );
        std::process::exit(1);
    };
    let db = PathBuf::from(db);
    let root = PathBuf::from(root);

    let store = EventStore::open_existing(&db)?;

    let mut fireable_count = 0usize;
    let mut with_check_count = 0usize;
    let mut holds_count = 0usize;
    let mut fails: Vec<CheckedItem> = Vec::new();
    let mut cannot_run: Vec<CheckedItem> = Vec::new();

    for live in live_items(&store) {
        if !live.item.kind.can_fire() {
            continue; // only Rule/Orientation can ever carry a check (gate::declare ground 12)
        }
        fireable_count += 1;
        let Some(check) = live.item.check else { continue };
        with_check_count += 1;
        let id = live.id;
        let text = live.item.text;
        match run(&check, &root) {
            Outcome::Holds => holds_count += 1,
            Outcome::Fails => fails.push(CheckedItem { id, check, text }),
            Outcome::CannotRun => cannot_run.push(CheckedItem { id, check, text }),
        }
    }

    println!("live fireable items: {fireable_count}");
    println!("carry a check: {with_check_count}");
    println!("holds: {holds_count}");
    println!("fails: {}", fails.len());
    println!("cannot run: {}", cannot_run.len());
    println!();
    println!(
        "a FAILS item's check ran and came back false right now, so the fact it guards is \
         provably wrong; a CANNOT RUN item's check could not be run at all and proves nothing \
         either way - these are never the same thing."
    );

    println!();
    println!("items whose check FAILS ({}):", fails.len());
    for row in &fails {
        println!("id: {}", row.id);
        println!("  check: {}", describe_check(&row.check));
        println!("  text:  {}", snippet(&row.text));
    }

    println!();
    println!("items whose check CANNOT RUN ({}):", cannot_run.len());
    for row in &cannot_run {
        println!("id: {}", row.id);
        println!("  check: {}", describe_check(&row.check));
        println!("  text:  {}", snippet(&row.text));
    }

    Ok(())
}
