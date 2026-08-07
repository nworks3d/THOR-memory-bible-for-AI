//! One-off repair: rewrite every item whose stored project NAMES the global
//! tier ("global", "null", ...) so it belongs to the global tier properly.
//!
//! Why a tool and not a script: this writes to a real memory. It goes through
//! `model::store::revise`, so every repair passes the same write gate as any
//! other write, lands as a `fact_revised` event in the log, and is visible in
//! `history` afterwards. Nothing is edited in place and nothing is deleted.
//!
//! Why it exists at all - see `model::normalize::normalize_project`. Project
//! identity had no normalisation, 1.0 spells its global tier as the literal
//! word "global", and the migration carried that word through as a project
//! name. 74 of the owner's global rules ended up owned by a project called
//! "global". Harmless while nothing scoped by project; a total, silent loss of
//! the global tier the moment the moment surface started scoping correctly.
//!
//! The write path is fixed, so this can only ever be needed for items written
//! before that fix. It is idempotent: a second run finds nothing to do.
//!
//! Usage:
//!     repair_project --db <path>            report what it would change
//!     repair_project --db <path> --apply    make the changes

use clap::Parser;
use model::normalize::normalize_project;
use model::store;
use std::path::PathBuf;
use std::process::ExitCode;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(name = "repair-project", about = "Move items whose project NAMES the global tier onto the global tier")]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    /// Without this, nothing is written - the run only reports.
    #[arg(long)]
    apply: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut store = match EventStore::open_existing(&cli.db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the store at {}: {e}", cli.db.display());
            return ExitCode::FAILURE;
        }
    };

    // Every live item whose project is set but normalises away.
    let affected: Vec<(String, String)> = serve::live::live_items(&store)
        .into_iter()
        .filter_map(|li| {
            let current = li.item.project.clone()?;
            normalize_project(Some(&current)).is_none().then_some((li.id, current))
        })
        .collect();

    if affected.is_empty() {
        println!("nothing to repair: no live item names the global tier as its project");
        return ExitCode::SUCCESS;
    }

    let mut by_value: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, value) in &affected {
        *by_value.entry(value.as_str()).or_default() += 1;
    }
    println!("items whose project names the global tier: {}", affected.len());
    for (value, n) in &by_value {
        println!("  {value:12} {n}");
    }

    if !cli.apply {
        println!("\nnothing written (pass --apply to make these changes)");
        return ExitCode::SUCCESS;
    }

    let mut repaired = 0usize;
    let mut failed = 0usize;
    for (id, _) in &affected {
        let existing = match store::show(&store, id) {
            Ok(item) => item,
            Err(e) => {
                eprintln!("{id}: cannot read: {e}");
                failed += 1;
                continue;
            }
        };
        let mut updated = existing.clone();
        updated.project = None;
        // `revise` normalises both sides, so this reads as "same meaning,
        // written properly" rather than as dropping a field.
        match store::revise(&mut store, "repair", "repair", "repair-project", &existing, &updated) {
            Ok(_) => repaired += 1,
            Err(e) => {
                eprintln!("{id}: refused: {e}");
                failed += 1;
            }
        }
    }

    println!("\nrepaired: {repaired}");
    if failed > 0 {
        eprintln!("failed:   {failed}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
