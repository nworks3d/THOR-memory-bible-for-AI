//! Migration CLI: I/O only. Reads a JSONL export, opens (or creates) the
//! target store, and hands the whole run to `serve::migrate::run` - the one
//! place that decides what gets written, refused or skipped. See that
//! module's own doc comment for the counting rules this prints.

use clap::Parser;
use serve::migrate;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(name = "migrate", about = "Migrate a JSONL item export into a 2.0 store through the real write gate")]
struct Cli {
    /// Path to the target event-log database. Created if it does not exist;
    /// a rerun against an existing one is safe - an item already present is
    /// skipped, never re-declared (see serve::migrate::already_migrated).
    #[arg(long)]
    db: PathBuf,
    /// Path to the JSONL export (one item per line).
    #[arg(long)]
    input: PathBuf,
    #[arg(long, default_value = "migration")]
    session_id: String,
    #[arg(long, default_value = "migration")]
    lineage_id: String,
    #[arg(long, default_value = "migration")]
    actor: String,
}

fn main() {
    let cli = Cli::parse();

    let text = match std::fs::read_to_string(&cli.input) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("could not read {}: {e}", cli.input.display());
            std::process::exit(1);
        }
    };

    let mut store = match EventStore::new(&cli.db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("could not open store at {}: {e}", cli.db.display());
            std::process::exit(1);
        }
    };

    let report = migrate::run(&mut store, &text, &cli.session_id, &cli.lineage_id, &cli.actor);

    println!("items in:           {}", report.items_in);
    println!("written:            {}", report.written);
    println!("already present:    {}", report.already_present);
    println!("malformed:          {}", report.malformed);
    println!("refused (total):    {}", report.refused_total());
    for (ground, count) in &report.refused {
        println!("  {ground:<40} {count}");
    }

    if !report.hard_errors.is_empty() {
        eprintln!("\n{} HARD ERROR(S) - the migration did not run cleanly:", report.hard_errors.len());
        for e in &report.hard_errors {
            eprintln!("  {e}");
        }
    }

    let accounted = report.accounted_for();
    if accounted != report.items_in {
        eprintln!(
            "\nCOUNT MISMATCH: items_in={} but written+refused+malformed+already_present={accounted} - something was lost silently, investigate before trusting this run",
            report.items_in
        );
        std::process::exit(1);
    }

    let represented = migrate::source_facts_represented(&store);
    println!("\nsource facts represented (via source: tag): {represented}");

    if !report.hard_errors.is_empty() {
        std::process::exit(1);
    }
}
