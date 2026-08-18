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
    /// Path to the store to back up, or - with --restore-from - the FRESH store
    /// to rebuild into.
    #[arg(long)]
    db: PathBuf,
    /// Path to an existing git clone with an `origin` remote and a `main` branch.
    /// Required for a backup; ignored when restoring.
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Rebuild a store from an exported log instead of backing one up: point it
    /// at an `events.jsonl` from this repo.
    ///
    /// WHY THIS FLAG EXISTS. `thor_core::backup::restore_jsonl` was written,
    /// tested and reachable from nowhere: 2.0 could write a backup every session
    /// and had no way on earth to read one back. A backup you cannot restore is
    /// a promise, not a fallback - found 2026-08-15 while documenting the
    /// restore steps and discovering there were none.
    ///
    /// Restores only into a store that does not exist yet or is empty, and
    /// verifies every replayed hash against the recorded one, so a log that was
    /// tampered with fails loudly instead of quietly becoming your memory.
    #[arg(long = "restore-from")]
    restore_from: Option<PathBuf>,
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

    // Restore mode, checked first: it is the one path that must work when the
    // live store is gone, so it never opens an existing store.
    if let Some(from) = &cli.restore_from {
        let file = match std::fs::File::open(from) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("cannot read the exported log at {}: {e}", from.display());
                return ExitCode::FAILURE;
            }
        };
        let mut store = match EventStore::new(&cli.db) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cannot create the store at {}: {e}", cli.db.display());
                return ExitCode::FAILURE;
            }
        };
        return match backup::restore_jsonl(&mut store, std::io::BufReader::new(file)) {
            Ok(n) => {
                println!("restored {n} events into {}", cli.db.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("restore failed: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let store = match EventStore::open_existing(&cli.db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("cannot open the store at {}: {e}", cli.db.display());
            return ExitCode::FAILURE;
        }
    };
    let Some(repo) = cli.repo.clone() else {
        eprintln!("--repo is required to back up (or pass --restore-from to rebuild a store instead)");
        return ExitCode::FAILURE;
    };
    if !repo.join(".git").exists() {
        eprintln!(
            "{} is not a git clone (no .git) - point --repo at a clone that already has an origin",
            repo.display()
        );
        return ExitCode::FAILURE;
    }

    // A backup is a write and a declaration, so it fails loudly (CONTRACT R5).
    // THE SECOND LANE TRAVELS WITH THE FIRST. Until 2026-08-18 the hourly
    // backup carried the event log and nothing else, so the library - the
    // owner's recipes, books, training log - existed in exactly one file on
    // one machine. Written before the export below, so both land in the same
    // commit; a debounced run that commits nothing simply leaves a fresh file
    // for the next one.
    let library_path = library::default_path(&cli.db);
    if library_path.exists() {
        let out_dir = repo.join(&cli.subdir);
        match std::fs::create_dir_all(&out_dir)
            .map_err(|e| e.to_string())
            .and_then(|()| library::Library::open(&library_path))
            .and_then(|lib| {
                let mut f = std::fs::File::create(out_dir.join("library.jsonl")).map_err(|e| e.to_string())?;
                lib.export_jsonl(&mut f)
            }) {
            Ok(n) => println!("library: {n} line(s) exported"),
            // Never let this stop the event-log backup: half a backup beats
            // none, and the failure is printed rather than swallowed.
            Err(e) => eprintln!("library: NOT exported ({e}) - the event log backup still runs"),
        }
    }

    match backup::backup_to_repo(&store, &repo, &cli.subdir, cli.force) {
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
