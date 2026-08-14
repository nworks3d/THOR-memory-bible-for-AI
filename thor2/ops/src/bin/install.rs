//! CLI for `ops::install`: the whole setup in one command.
//!
//! It creates the store if there is not one yet, wires THOR's four hooks into
//! an agent's settings.json, registers the tool server the agent writes
//! through, and optionally gives the current folder its own memory scope.
//!
//! WHY ONE COMMAND AND NOT FOUR. Every step left to a person is a step that
//! gets skipped, and each of these fails QUIETLY when it is missing: no hooks
//! means the memory never speaks, no tool server means the agent can never
//! write anything down, no project marker means everything piles into the
//! shared memory every project sees. None of those announce themselves. The
//! only honest fix is to do them together and say plainly what happened.
//!
//! `--settings` and `--mcp-json` are paths this command WRITES to, so neither
//! is ever guessed: name them, or they are not touched. Everything else has a
//! default that can be worked out from where this binary is and where the
//! platform keeps per-user data.

use clap::Parser;
use ops::install::{
    default_data_dir, ensure_store, install_hooks, install_tool_server, seed_working_contract,
    standard_hooks, write_project_marker, HookOutcome, MarkerOutcome, ServerOutcome, StoreOutcome,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "install",
    about = "Set THOR up: create the store, wire the hooks, register the tool server"
)]
struct Cli {
    /// The agent's settings.json to install the hooks into. Backed up to
    /// <path>.bak first. Required: this command writes to it, and a tool that
    /// rewrites a config file should never guess which one.
    #[arg(long)]
    settings: PathBuf,

    /// The .mcp.json to register the tool server in. Backed up the same way.
    /// Leave it out and the hooks are installed on their own, which gives the
    /// agent a memory it can read but never write to.
    #[arg(long = "mcp-json")]
    mcp_json: Option<PathBuf>,

    /// The `serve` binary the hooks will call. Defaults to the one next to
    /// this installer.
    #[arg(long = "serve-exe")]
    serve_exe: Option<PathBuf>,

    /// The `mcp` binary the agent writes through. Defaults to the one next to
    /// this installer.
    #[arg(long = "mcp-exe")]
    mcp_exe: Option<PathBuf>,

    /// Where the memory lives. Defaults to the per-user data directory, never
    /// next to the binaries: a store inside a build output directory is one
    /// `cargo clean` away from being gone.
    #[arg(long)]
    db: Option<PathBuf>,

    /// A directory of code indexes, one per project. Defaults to `codeindex`
    /// beside the store.
    #[arg(long = "code-index-root")]
    code_index_root: Option<PathBuf>,

    /// Give the current folder its own memory scope under this key. Without
    /// one, everything stored while working here lands in the shared memory
    /// that every project sees.
    #[arg(long)]
    project: Option<String>,
}

/// The binary named `name` sitting next to this installer, with the
/// platform's own executable suffix.
fn sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let data_dir = default_data_dir();
    let serve_exe = match cli.serve_exe.or_else(|| sibling("serve")) {
        Some(p) => p,
        None => {
            eprintln!("could not work out where `serve` is - name it with --serve-exe");
            return ExitCode::FAILURE;
        }
    };
    let mcp_exe = cli.mcp_exe.or_else(|| sibling("mcp"));
    let db = match cli.db.or_else(|| data_dir.as_ref().map(|d| d.join("thor.db"))) {
        Some(p) => p,
        None => {
            eprintln!("could not work out where to keep the memory - name it with --db");
            return ExitCode::FAILURE;
        }
    };
    let code_index_root = cli
        .code_index_root
        .or_else(|| db.parent().map(|d| d.join("codeindex")));

    // A hook pointing at a binary that is not there fails OPEN: the agent
    // carries on and the memory simply never speaks again. Nothing reports
    // that afterwards, and THIS is the only moment it is still visible - so it
    // is a refusal, not a warning.
    //
    // A warning stood here until 2026-08-14. It went to stderr in the middle
    // of a wall of "+ done" lines, which is precisely where an install that
    // did not work hides: a newcomer reads the successes, restarts the agent,
    // and owns a memory that will never speak to them. An installer that can
    // see the failure and completes anyway is the same silent-failure class
    // this whole system exists to refuse.
    if !serve_exe.exists() {
        eprintln!(
            "there is no file at {} - refusing to write hooks that call it. A hook fails open, so \
             this would install quietly and stay broken: the agent carries on, the memory never \
             speaks, and nothing ever reports it. Build it first (cargo build --release --features \
             semantic, in thor2), or name the real one with --serve-exe.",
            serve_exe.display()
        );
        return ExitCode::FAILURE;
    }

    println!("memory:       {}", db.display());
    println!("hooks call:   {}", serve_exe.display());
    match &mcp_exe {
        Some(p) => println!("writes via:   {}", p.display()),
        None => println!("writes via:   (not set - the agent will not be able to write)"),
    }
    println!();

    // 1. The store. First, because every later step points at it, and because
    // the health check refuses to run without one.
    match ensure_store(&db) {
        Ok(StoreOutcome::Created) => {
            println!("+ created a memory at {}", db.display());
            // Only ever on a store this run created. A brand new memory is
            // empty, and an agent with nothing in front of it writes notes in
            // shapes that never come back - which fails silently on both
            // sides. Seeding an EXISTING memory would be pushing four notes
            // into someone's real work uninvited, so that never happens.
            match seed_working_contract(&db) {
                Ok(seeded) => {
                    let stored = seeded.iter().filter(|s| s.stored).count();
                    println!("+ put {stored} starting notes in it, on how to write one that comes back");
                    for s in seeded.iter().filter(|s| !s.stored) {
                        println!("  ! {} was refused: {}", s.id, s.refusal.as_deref().unwrap_or("no reason given"));
                    }
                    println!("  they are ordinary notes: unpin one, rewrite it, or throw it out");
                }
                Err(e) => println!("  ! could not write the starting notes: {e}"),
            }
        }
        Ok(StoreOutcome::AlreadyThere) => println!("= memory already there, left untouched"),
        Err(e) => {
            eprintln!("install refused: could not create the memory: {e}");
            return ExitCode::FAILURE;
        }
    }

    // 2. The hooks: what lets the memory speak at all.
    let specs = standard_hooks(&serve_exe.display().to_string(), &db.display().to_string());
    match install_hooks(&cli.settings, &specs) {
        Ok(report) => {
            for (event, outcome) in &report.results {
                match outcome {
                    HookOutcome::Added => println!("+ hook {event}"),
                    HookOutcome::AlreadyPresent => println!("= hook {event} (already there)"),
                    HookOutcome::Replaced => println!("+ hook {event} (repointed a dead one)"),
                }
            }
            for old in &report.replaced {
                println!("  replaced a hook that called a binary no longer there: {old}");
            }
            if let Some(p) = &report.backup_path {
                println!("  previous settings backed up to {}", p.display());
            }
        }
        Err(e) => {
            eprintln!("install refused: {e}");
            eprintln!("the memory was created, but nothing was wired up - fix the above and run this again");
            return ExitCode::FAILURE;
        }
    }

    // 3. The tool server: what lets the agent write back.
    match (&cli.mcp_json, &mcp_exe) {
        (Some(path), Some(exe)) => {
            let mut args = vec!["--db".to_string(), db.display().to_string()];
            if let Some(root) = &code_index_root {
                args.push("--code-index-root".to_string());
                args.push(root.display().to_string());
            }
            match install_tool_server(path, "thor", &exe.display().to_string(), &args) {
                Ok(report) => match report.outcome {
                    ServerOutcome::Added => println!("+ tool server registered in {}", path.display()),
                    ServerOutcome::AlreadyPresent => println!("= tool server already registered"),
                    ServerOutcome::Replaced => {
                        println!("+ tool server registered in {}", path.display());
                        if let Some(old) = &report.replaced {
                            println!("  replaced the previous one, which pointed at {old}");
                        }
                        if let Some(p) = &report.backup_path {
                            println!("  previous file backed up to {}", p.display());
                        }
                    }
                },
                Err(e) => {
                    eprintln!("the hooks are installed, but registering the tool server failed: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        (Some(_), None) => {
            eprintln!("--mcp-json was given but `mcp` could not be found - name it with --mcp-exe");
            return ExitCode::FAILURE;
        }
        (None, _) => {
            println!("- no tool server registered (--mcp-json was not given)");
            println!("  the agent will be able to READ its memory but never write to it");
        }
    }

    // 4. The project scope, only when asked for.
    if let Some(key) = &cli.project {
        let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match write_project_marker(&here, key) {
            Ok(MarkerOutcome::Written) => println!("+ this folder now has its own memory, under {key:?}"),
            Ok(MarkerOutcome::AlreadyThere) => println!("= this folder already has its own memory, under {key:?}"),
            Err(e) => {
                eprintln!("everything else is installed, but the project scope was refused: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    println!();
    println!("Done. Restart the agent - it reads these files once, at startup.");
    println!("Then check it with:  doctor --db \"{}\"", db.display());
    ExitCode::SUCCESS
}
