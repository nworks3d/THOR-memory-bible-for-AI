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
    /// A directory whose immediate subdirectories are checkouts. Each one is
    /// resolved, and any project key held by an item that no checkout answers
    /// to is reported - the class of defect that is otherwise invisible from
    /// every surface (see `ops::health::orphan_projects_line`).
    ///
    /// Optional since 2026-09-07: omitted, doctor INFERS one from where it is
    /// standing (the parent of the nearest repo above the current directory -
    /// see `ops::health::infer_checkouts_root`) rather than leaving decay and
    /// crowding unmeasured. This flag always overrides that guess; the report
    /// itself says which one was actually used (`ops::health::
    /// checkouts_root_line`).
    #[arg(long)]
    checkouts: Option<PathBuf>,
    /// Override the semantic embedding model's directory (feature
    /// `semantic`). Omitted = the per-user default (see
    /// `serve::semantic_paths::default_model_dir`).
    #[arg(long = "model-dir")]
    model_dir: Option<PathBuf>,
    /// Turn this report into a gate: exit 1 when something gate-worthy was
    /// found (a broken event chain, a dead anchor, or a proof that now comes
    /// out false), exit 0 when clean. Without this flag doctor always exits
    /// 0, findings or not - that behaviour is unchanged. See
    /// `ops::health::gate_verdict` for exactly what counts as gate-worthy and
    /// why, and for why a store that could not even be judged (missing, or
    /// present but unreadable) exits 0 as well: a cloud session with no
    /// thor.db must never be blocked by its own absence.
    #[arg(long)]
    gate: bool,
    /// With --gate, narrow the dead-anchor/false-proof check to this
    /// project's own items, so one repository's gate can never fail because
    /// of another checkout's rot under the same --checkouts directory.
    /// Ignored without --gate.
    #[arg(long)]
    project: Option<String>,
    /// Name every finding instead of the first twenty. The capped lists keep a
    /// daily report readable; a cleanup needs the whole list, and until
    /// 2026-08-15 there was no way to get it - the only route was reading the
    /// store by hand, which is exactly the kind of detour this report exists to
    /// remove.
    #[arg(long)]
    full: bool,
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

    // THE DEFECT THIS CLOSES, reported at the end of two separate sessions:
    // "decay and crowding not measured (requires --checkouts)" - doctor left
    // its two most important lines unmeasured and gave no hint the flag even
    // existed. An explicit --checkouts still always wins
    // (`ops::health::resolve_checkouts_root`); this only fills the gap when
    // it was left out, and the printed line says which happened either way.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let checkouts_root = ops::health::resolve_checkouts_root(cli.checkouts.as_deref(), &cwd);
    let checkouts = checkouts_root.as_path();

    println!("{}", ops::health::checkouts_root_line(&checkouts_root));
    for line in ops::health::report(&cli.db, cli.index_db.as_deref(), cli.repo.as_deref(), replica, cli.model_dir.as_deref(), checkouts, cli.full) {
        println!("{line}");
    }

    // --gate turns the report above into an exit code, reusing its verdict
    // rather than deciding anything new here - see `ops::health::gate_verdict`
    // for which findings count and why. Without --gate, cli.gate is false and
    // control falls straight through to the unchanged ExitCode::SUCCESS below.
    // Uses the SAME resolved `checkouts` as the report above (inferred or
    // explicit, never the raw un-resolved flag) so a dead anchor the report
    // just named under an inferred root can never pass the gate silently for
    // no reason but this call forgetting to infer it too.
    if cli.gate {
        match ops::health::gate_verdict(&cli.db, checkouts, cli.project.as_deref()) {
            ops::health::GateVerdict::Failing => return ExitCode::FAILURE,
            ops::health::GateVerdict::Clean | ops::health::GateVerdict::NotAvailable => return ExitCode::SUCCESS,
        }
    }

    ExitCode::SUCCESS
}
