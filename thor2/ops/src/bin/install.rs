//! CLI for `ops::install`: wires THOR's three hooks (SessionStart,
//! PreToolUse, UserPromptSubmit) into an agent's settings.json.
//!
//! `--settings` is required and has no default on purpose: this command
//! writes to a config file, and a tool that mutates a file should never guess
//! which one when it was not told.

use clap::Parser;
use ops::install::{install_hooks, standard_hooks, HookOutcome};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "install", about = "Install THOR's SessionStart/PreToolUse/UserPromptSubmit hooks into an agent's settings.json")]
struct Cli {
    /// The settings.json to install into. Backed up to <path>.bak first.
    #[arg(long)]
    settings: PathBuf,
    /// Path to the `serve` binary the hooks will call.
    #[arg(long = "serve-exe")]
    serve_exe: String,
    /// Path to the THOR event-log database the hooks should read/write.
    #[arg(long)]
    db: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let specs = standard_hooks(&cli.serve_exe, &cli.db);
    match install_hooks(&cli.settings, &specs) {
        Ok(report) => {
            for (event, outcome) in &report.results {
                match outcome {
                    HookOutcome::Added => println!("+ {event}"),
                    HookOutcome::AlreadyPresent => println!("= {event} (already installed)"),
                }
            }
            match &report.backup_path {
                Some(p) => println!("backed up the previous settings to {}", p.display()),
                None => println!("no previous settings file existed - nothing to back up"),
            }
            println!("restart the agent for the hooks to take effect");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("install refused: {e}");
            ExitCode::FAILURE
        }
    }
}
