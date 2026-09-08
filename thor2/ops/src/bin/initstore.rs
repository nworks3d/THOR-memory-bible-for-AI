//! `initstore --db <path>`: bring a store from nothing to one that answers,
//! and nothing else.
//!
//! WHY THIS EXISTS SEPARATELY FROM `install`. `install` always resolves a
//! settings.json to write hooks into (defaulting under HOME/USERPROFILE when
//! none is named) and refuses outright when the `serve` binary it would point
//! those hooks at is not there. Both are wrong for a container entry point:
//! there is no assistant configuration in the image to touch, no `serve`
//! binary ships beside `mcp` here, and an entry point has no business
//! rewriting an assistant's settings on every container start regardless.
//! This binary calls exactly the store half of what `install` does -
//! `ensure_store`, then (only for a store this call created)
//! `seed_working_contract`, then `seed_response_rulebook` - via
//! `ops::install::ensure_and_seed_store`, and stops there.
//!
//! Prints to stderr, not stdout: `deploy/entrypoint.sh` runs this before
//! handing stdout to `mcp`, which speaks the tool protocol on it from its
//! very first byte. A status line ahead of that would be a stray non-JSON
//! line on a stream a client reads as nothing but JSON-RPC.

use clap::Parser;
use ops::install::{ensure_and_seed_store, RulebookOutcome, StoreOutcome};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "initstore",
    version = env!("CARGO_PKG_VERSION"),
    about = "Create and seed a THOR store if one is not already there, and touch nothing else"
)]
struct Cli {
    /// Path to the store. Created - with the working contract and the
    /// response-guard rulebook - when it does not exist yet; left completely
    /// untouched when it does.
    #[arg(long)]
    db: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match ensure_and_seed_store(&cli.db) {
        Ok(report) => {
            match report.store {
                StoreOutcome::Created => {
                    let stored = report.seeded.iter().filter(|s| s.stored).count();
                    eprintln!("+ created a memory at {} with {stored} starting notes", cli.db.display());
                    for s in report.seeded.iter().filter(|s| !s.stored) {
                        eprintln!("  ! {} was refused: {}", s.id, s.refusal.as_deref().unwrap_or("no reason given"));
                    }
                }
                StoreOutcome::AlreadyThere => {
                    eprintln!("= memory already at {} (left untouched)", cli.db.display());
                }
            }
            match report.rulebook.outcome {
                RulebookOutcome::Written => {
                    eprintln!("+ wrote a starting response-guard rulebook to {}", report.rulebook.path.display())
                }
                RulebookOutcome::AlreadyThere => eprintln!(
                    "= response-guard rulebook already at {} (left untouched)",
                    report.rulebook.path.display()
                ),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("initstore: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}
