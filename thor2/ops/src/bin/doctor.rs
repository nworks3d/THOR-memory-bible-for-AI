//! CLI for `ops::health`: one plain-language line per component.

use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "doctor", about = "One plain-language line per component: the memory store, the code index, the replica, and how many rules still lack a falsifier")]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    #[arg(long = "index-db")]
    index_db: Option<PathBuf>,
    #[arg(long)]
    repo: Option<PathBuf>,
    /// The replica's base URL, e.g. http://nas:5555. Reads THOR_SYNC_TOKEN
    /// for the shared secret when given.
    #[arg(long)]
    to: Option<String>,
    /// Override the semantic embedding model's directory (feature
    /// `semantic`). Omitted = the per-user default (see
    /// `serve::semantic_paths::default_model_dir`).
    /// A directory whose immediate subdirectories are checkouts. Each one is
    /// resolved, and any project key held by an item that no checkout answers
    /// to is reported - the class of defect that is otherwise invisible from
    /// every surface (see `ops::health::orphan_projects_line`).
    #[arg(long)]
    checkouts: Option<PathBuf>,
    #[arg(long = "model-dir")]
    model_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let token = match &cli.to {
        Some(_) => match std::env::var("THOR_SYNC_TOKEN") {
            Ok(t) if !t.trim().is_empty() => Some(t),
            _ => {
                eprintln!("THOR_SYNC_TOKEN is not set - cannot check the replica named with --to");
                None
            }
        },
        None => None,
    };
    let replica = match (&cli.to, &token) {
        (Some(url), Some(tok)) => Some((url.as_str(), tok.as_str())),
        _ => None,
    };

    for line in ops::health::report(&cli.db, cli.index_db.as_deref(), cli.repo.as_deref(), replica, cli.model_dir.as_deref(), cli.checkouts.as_deref()) {
        println!("{line}");
    }
    ExitCode::SUCCESS
}
