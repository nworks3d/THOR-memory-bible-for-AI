//! One-off repair: give back the project attribution the migration dropped.
//!
//! THE DEFECT. The migration derived an item's project from its 1.0 entity
//! id, which only carries one when the id is of the newer `<project>:<id>`
//! shape. The older generation of facts have plain ULID ids, so their project
//! came out empty - even though their 1.0 footer named one. Measured on the
//! live store: 1017 FIREABLE items (561 rules, 456 orientations) sit in the
//! global tier while 1.0 attributed them to a specific project. Global means
//! "applies everywhere", so every one of them fires in every project. That is
//! the noise complaint this whole rebuild exists to answer, sitting in the
//! data rather than in the code.
//!
//! WHAT THIS RESTRICTS ITSELF TO, and why. Of those 1017, 861 belong to a
//! project that has a real checkout, and moving them there is pure gain: they
//! stop firing in unrelated projects and start firing in their own. The other
//! 156 belong to projects with no checkout on this machine (investments,
//! recipes, a CNC project). Attributing those correctly is defensible, but it
//! would take them from "fires everywhere" to "fires nowhere", which is a
//! judgement about someone's memory rather than a repair. So `--checkouts`
//! is REQUIRED and only keys that actually resolve are touched. The rest are
//! reported, and `doctor`'s own project-keys line keeps naming them.
//!
//! Everything goes through `model::store::revise`: the same write gate as any
//! other write, one `fact_revised` event each, visible in `history`, nothing
//! edited in place and nothing deleted. Idempotent - a second run finds
//! nothing to do.
//!
//! Usage:
//!     restore_attribution --db <2.0 store> --legacy <1.0 store> --checkouts <dir>
//!     ... --apply     to make the changes

use clap::Parser;
use model::store;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::ExitCode;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(name = "restore-attribution", about = "Give back the project attribution the migration dropped")]
struct Cli {
    /// The 2.0 store to repair.
    #[arg(long)]
    db: PathBuf,
    /// A 1.0 store to read the original attribution from. Read-only.
    #[arg(long)]
    legacy: PathBuf,
    /// A directory whose immediate subdirectories are checkouts. Only project
    /// keys that actually resolve from one of them are applied - unless
    /// `--include-unresolved` is also given.
    #[arg(long)]
    checkouts: PathBuf,
    /// Also attribute items whose 1.0 project has NO checkout here.
    ///
    /// The first pass deliberately skipped these: attributing a rule to a
    /// project with no code would take it from "fires everywhere" to "fires
    /// nowhere at a gate". That reasoning holds for a RULE. It is wrong for
    /// the facts that are actually here - measured, they are knowledge:
    /// investment allocations, a printer's construction notes, CNC feeds and
    /// speeds, recipes. Those are lookup material, not action rules; they
    /// should never have fired at a gate in the first place, and "fires
    /// nowhere at a gate" is exactly correct for them. They stay fully
    /// findable via search (project scope never touches the lookup door), so
    /// this only removes noise: a recipe fact no longer surfaces while coding.
    /// If a checkout for one of these projects ever appears, its facts fire
    /// there automatically.
    #[arg(long)]
    include_unresolved: bool,
    /// Without this, nothing is written - the run only reports.
    #[arg(long)]
    apply: bool,
}

/// The project a 1.0 body's own footer names, or `None` when it names the
/// global tier or none at all. Parsed from the footer's `| project: <x>` field
/// - the only field this tool reads, and never the body text.
fn legacy_project(body: &str) -> Option<String> {
    let idx = body.find("| project: ")?;
    let rest = &body[idx + "| project: ".len()..];
    let value = rest.split(" |").next()?.trim().trim_end_matches(']').trim();
    model::normalize::normalize_project(Some(value))
}

/// Every 1.0 entity that names a real project.
///
/// The store is COPIED aside before it is opened, and the copy is what gets
/// read. Opening a store heals its schema and its search index, which is a
/// write - and a 1.0 store is exactly the thing this whole rebuild promised
/// not to touch. A repair tool that quietly modifies the fallback it is
/// repairing away from would be the worst possible version of today's
/// recurring defect.
fn legacy_attribution(path: &std::path::Path) -> anyhow::Result<BTreeMap<String, String>> {
    let work = std::env::temp_dir().join(format!(
        "thor2-legacy-read-{}.db",
        std::process::id()
    ));
    std::fs::copy(path, &work)?;
    for suffix in ["-wal", "-shm"] {
        let side = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        if side.exists() {
            let _ = std::fs::copy(&side, std::path::PathBuf::from(format!("{}{suffix}", work.display())));
        }
    }

    let result = (|| -> anyhow::Result<BTreeMap<String, String>> {
        let store = EventStore::open_existing(&work)?;
        let events = store.get_all_events()?;
        let heads = thor_core::cas::compute_head_sets(&events);
        let by_hash: std::collections::HashMap<&str, &thor_core::event_store::Event> =
            events.iter().map(|e| (e.this_hash.as_str(), e)).collect();

        let mut out = BTreeMap::new();
        for (entity_id, head_set) in &heads {
            for rev in &head_set.heads {
                let Some(event) = by_hash.get(rev.as_str()) else { continue };
                if let Some(project) = legacy_project(&event.body) {
                    out.insert(entity_id.clone(), project);
                    break;
                }
            }
        }
        Ok(out)
    })();

    let _ = std::fs::remove_file(&work);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(std::path::PathBuf::from(format!("{}{suffix}", work.display())));
    }
    result
}

/// Which project keys a real checkout under `root` resolves to.
fn resolvable(root: &std::path::Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    out.insert(key);
                }
            }
        }
    }
    out
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let legacy = match legacy_attribution(&cli.legacy) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cannot read the legacy store at {}: {e}", cli.legacy.display());
            return ExitCode::FAILURE;
        }
    };
    let reachable = resolvable(&cli.checkouts);
    if reachable.is_empty() {
        eprintln!("no checkout under {} resolves to a project - refusing to guess", cli.checkouts.display());
        return ExitCode::FAILURE;
    }
    println!("legacy facts naming a project: {}", legacy.len());
    println!("project keys with a real checkout: {}", reachable.len());

    let mut store = match EventStore::open_existing(&cli.db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the store at {}: {e}", cli.db.display());
            return ExitCode::FAILURE;
        }
    };

    // Only items that are global NOW, are fireable, and whose source fact
    // named a project. Whether an unresolved project is applied or skipped
    // depends on --include-unresolved (see its own doc comment).
    let mut to_apply: Vec<(String, String)> = Vec::new();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for live in serve::live::live_items(&store) {
        if live.item.project.is_some() || !live.item.kind.can_fire() {
            continue;
        }
        let Some(source) = live.item.tags.iter().find_map(|t| t.strip_prefix("source:")) else { continue };
        let Some(project) = legacy.get(source) else { continue };
        if reachable.contains(project) || cli.include_unresolved {
            to_apply.push((live.id.clone(), project.clone()));
        } else {
            *skipped.entry(project.clone()).or_default() += 1;
        }
    }

    println!();
    println!("fireable items to re-attribute: {}", to_apply.len());
    let mut by_project: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, p) in &to_apply {
        *by_project.entry(p.as_str()).or_default() += 1;
    }
    for (p, n) in &by_project {
        let mark = if reachable.contains(*p) { "" } else { "  (no checkout - findable via search only)" };
        println!("  {p:24} {n}{mark}");
    }
    if !skipped.is_empty() {
        let total: usize = skipped.values().sum();
        println!();
        println!("left alone ({total} item(s)): their project has no checkout here. Pass");
        println!("--include-unresolved to attribute them too (they become search-only, never a gate):");
        for (p, n) in &skipped {
            println!("  {p:24} {n}");
        }
    }

    if !cli.apply {
        println!("\nnothing written (pass --apply to make these changes)");
        return ExitCode::SUCCESS;
    }

    let mut done = 0usize;
    let mut failed = 0usize;
    for (id, project) in &to_apply {
        let existing = match store::show(&store, id) {
            Ok(item) => item,
            Err(e) => {
                eprintln!("{id}: cannot read: {e}");
                failed += 1;
                continue;
            }
        };
        let mut updated = existing.clone();
        updated.project = Some(project.clone());
        match store::revise(&mut store, "repair", "repair", "restore-attribution", &existing, &updated) {
            Ok(_) => done += 1,
            Err(e) => {
                eprintln!("{id}: refused: {e}");
                failed += 1;
            }
        }
    }

    println!("\nre-attributed: {done}");
    if failed > 0 {
        eprintln!("failed:        {failed}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
