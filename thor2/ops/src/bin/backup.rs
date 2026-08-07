//! Off-site backup of the event log into a git clone.
//!
//! The backup IS the log, exported as canonical append-only JSONL (see
//! `thor_core::backup`). This binary is only the CLI around it: it opens the
//! store, names the subdirectory to write into, and prints what happened.
//!
//! Why the subdirectory has a default that is NOT "thor": a 2.0 store is a
//! different hash chain over the same memory, and 1.0's backup already owns
//! `thor/events.jsonl` in the same repository. Sharing that file would replace
//! the fallback with a log that does not continue it. So 2.0 writes to `thor2/`
//! and both chains survive side by side for as long as they need to.

use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;
use thor_core::backup;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(
    name = "backup",
    about = "Export the event log into a git clone, then commit and push it"
)]
struct Cli {
    /// Path to the store to back up.
    #[arg(long)]
    db: PathBuf,
    /// Path to an existing git clone with an `origin` remote and a `main` branch.
    #[arg(long)]
    repo: PathBuf,
    /// Directory inside the repo to write into. One plain name, no slashes.
    #[arg(long, default_value = "thor2")]
    subdir: String,
    /// Back up even if the last one was recent. Without this, a run inside the
    /// debounce window prints why it did nothing and exits 0 - that is a hook
    /// behaving, not a failure.
    #[arg(long)]
    force: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let store = match EventStore::open_existing(&cli.db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("cannot open the store at {}: {e}", cli.db.display());
            return ExitCode::FAILURE;
        }
    };
    if !cli.repo.join(".git").exists() {
        eprintln!(
            "{} is not a git clone (no .git) - point --repo at a clone that already has an origin",
            cli.repo.display()
        );
        return ExitCode::FAILURE;
    }

    // A backup is a write and a declaration, so it fails loudly (CONTRACT R5).
    match backup::backup_to_repo(&store, &cli.repo, &cli.subdir, cli.force) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("backup failed: {e}");
            ExitCode::FAILURE
        }
    }
}
