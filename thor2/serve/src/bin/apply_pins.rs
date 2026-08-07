//! One-time repair CLI: read the OLD system's pinned fact ids out of its
//! legacy key-value ledger (`thor_core::ledger::read_kv`, namespace `pins`,
//! key `list` - a JSON array of old fact ids) and give every live item this
//! store tagged `source:<old-id>` the `Always` binding, through the real
//! write gate (`serve::pins::apply_pins`), never around it.
//!
//! The caller is expected to hand `--ledger-db` a COPY of the live 1.0 ledger
//! file, never the original - this binary opens it read-only regardless
//! (`thor_core::ledger` never writes), but a copy also avoids contending with
//! the running system for a read lock. 1.0 is frozen (CONTRACT.md); this is
//! read-only against it either way.

use clap::Parser;
use serve::pins;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(name = "apply_pins", about = "Apply the old system's pinned fact ids as Always bindings, through the real write gate")]
struct Cli {
    /// Path to the target 2.0 store to modify.
    #[arg(long)]
    db: PathBuf,
    /// Path to a COPY of the old system's ledger sidecar (never the live
    /// file - this tool never writes to it either way, but a copy avoids
    /// even a read lock on the running system).
    #[arg(long)]
    ledger_db: PathBuf,
    #[arg(long, default_value = "apply-pins")]
    session_id: String,
    #[arg(long, default_value = "apply-pins")]
    lineage_id: String,
    #[arg(long, default_value = "apply-pins")]
    actor: String,
}

/// The one fixed location pins live at in the old ledger - not configurable:
/// this tool exists for exactly this one migration step, not as a general
/// kv-namespace browser.
const PINS_NAMESPACE: &str = "pins";
const PINS_KEY: &str = "list";

fn main() {
    let cli = Cli::parse();

    let raw = match thor_core::ledger::read_kv(&cli.ledger_db, PINS_NAMESPACE, PINS_KEY) {
        Ok(Some(raw)) => raw,
        Ok(None) => {
            eprintln!(
                "no pins found at ({PINS_NAMESPACE}, {PINS_KEY}) in {} - nothing to apply",
                cli.ledger_db.display()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("could not read ledger at {}: {e}", cli.ledger_db.display());
            std::process::exit(1);
        }
    };

    let old_ids: Vec<String> = match serde_json::from_str(&raw) {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("pins value at ({PINS_NAMESPACE}, {PINS_KEY}) is not a JSON array of strings: {e}");
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

    let report = pins::apply_pins(&mut store, &cli.session_id, &cli.lineage_id, &cli.actor, &old_ids);

    println!("pins total:           {}", report.pins_total);
    println!("pins linked:          {}", report.pins_linked);
    println!("pins unmatched:       {}", report.unmatched_pins.len());
    for id in &report.unmatched_pins {
        println!("  unmatched: {id}");
    }
    println!("items bound (Always): {}", report.items_bound);
    println!("items already Always: {}", report.items_already_always);
    println!("items not fireable (archive, skipped by design): {}", report.items_not_fireable);

    if !report.hard_errors.is_empty() {
        eprintln!("\n{} HARD ERROR(S) - the run did not complete cleanly:", report.hard_errors.len());
        for e in &report.hard_errors {
            eprintln!("  {e}");
        }
        std::process::exit(1);
    }
}
