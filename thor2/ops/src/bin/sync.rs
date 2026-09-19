//! CLI for the replica sync transport (see `ops::transport`):
//!
//!   recv    run the receiver: listen, and append only what continues our
//!           chain - refuse anything else, with a reason, and write nothing.
//!   ship    ask a receiver where it stands, then send it the difference.
//!   status  say whether both sides agree, and by how much they do not.
//!
//! The shared secret is read from THOR_SYNC_TOKEN, never a CLI flag - a
//! secret on the command line ends up in shell history and `ps`.

use clap::{Parser, Subcommand};
use ops::transport;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "sync",
    version = env!("CARGO_PKG_VERSION"),
    about = "Replicate THOR's log to/from a remote copy over a bearer-gated HTTP transport"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Listen on `--bind` and ingest shipped batches into the store at `--db`.
    Recv {
        #[arg(long)]
        db: PathBuf,
        /// Address to listen on, e.g. 0.0.0.0:5555.
        #[arg(long)]
        bind: String,
        /// Capture inbox file. Give this when writes may ARRIVE here (a
        /// remote connector on this machine): the connector queues them into
        /// this file and the authority drains it. Without it this receiver
        /// is a mirror only, and a write reaching it has nowhere to go.
        #[arg(long)]
        inbox: Option<PathBuf>,
    },
    /// Pull the captures queued at `--from`, apply them to the store at
    /// `--db`, and let the receiver drop its copy. Run this on the authority
    /// (the machine whose log is allowed to grow), before the ship.
    Drain {
        #[arg(long)]
        db: PathBuf,
        /// Base URL of the receiver holding the captures, e.g.
        /// http://10.0.0.50:5556.
        #[arg(long)]
        from: String,
    },
    /// Push the local store's backlog to the receiver at `--to`.
    Ship {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        to: String,
        #[arg(long, default_value_t = transport::DEFAULT_BATCH)]
        batch: usize,
    },
    /// Local tip, and - with `--to` - the replica's tip and the lag between them.
    Status {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        to: Option<String>,
    },
}

fn require_token() -> anyhow::Result<String> {
    let t = std::env::var("THOR_SYNC_TOKEN").unwrap_or_default();
    anyhow::ensure!(
        !t.trim().is_empty(),
        "THOR_SYNC_TOKEN is not set - this transport has no other auth; refusing to proceed without it"
    );
    Ok(t)
}

fn short(h: &str) -> &str {
    &h[..h.len().min(8)]
}

/// How many items one hourly ship embeds at most - small enough that a first
/// catch-up over several hundred missing/stale items spreads across a few
/// runs instead of making one hourly run run long. See `serve::vectors::
/// refresh`'s own doc comment for what "an item" means here (never a part).
///
/// MEASURED, NOT ESTIMATED (2026-09-19, the owner's real store): repairing a
/// backlog of 705 items took 2m58s, so about 0.25s per ITEM - roughly sixty
/// times a bare per-text embedding, because a real item is split into parts
/// and every part is embedded. So this budget is about 75s of work when a
/// backlog exists, and a second or two in the steady state of a few new or
/// changed items per hour. Nothing waits on it (it runs after the ship work
/// and cannot change its outcome), and a bigger budget is what drains a
/// backlog on a machine that is often switched off.
#[cfg(feature = "semantic")]
const HOURLY_VECTORS_BUDGET: usize = 300;

/// Best-effort meaning-search maintenance, piggybacked on every hourly ship
/// so a fact written today does not sit unfindable by meaning until a human
/// remembers to run `vectors-build` (see `serve::vectors`'s own doc comment
/// on exactly that rot). Called from the `Ship` arm below, after the ship
/// work itself - and can never affect it either way: this returns nothing,
/// so there is nothing here for `main` to fold into its own `outcome`.
#[cfg(feature = "semantic")]
fn hourly_vectors_refresh(db: &Path) {
    if let Some(line) = maybe_refresh_vectors(db, serve::semantic_paths::default_model_dir(), HOURLY_VECTORS_BUDGET) {
        println!("{line}");
    }
}

/// A build without the `semantic` feature has no vectors to refresh at all -
/// the compiled-out twin of `hourly_vectors_refresh` above, so the call site
/// in `main` never needs its own `#[cfg]`.
#[cfg(not(feature = "semantic"))]
fn hourly_vectors_refresh(_db: &Path) {}

/// `hourly_vectors_refresh`'s testable core. `model_dir` is a plain parameter
/// rather than resolved internally so a test can drive the "no model at all"
/// case without touching real environment variables. `None` means skip
/// silently and print nothing at all - a machine with no model is a normal,
/// supported setup, never a failure to report.
#[cfg(feature = "semantic")]
fn maybe_refresh_vectors(db: &Path, model_dir: Option<PathBuf>, budget: usize) -> Option<String> {
    let model_dir = model_dir?;
    run_vectors_refresh(db.to_path_buf(), model_dir, budget)
}

/// Open the store, refresh the vectors, and turn the result into one line -
/// wrapped in `fail_open` (below) so neither a returned error nor a panic
/// anywhere in this chain (a corrupt sidecar, a bad model file, a bug in the
/// embedder) ever escapes to the caller. This is the one place that decides
/// what the hourly log sees.
#[cfg(feature = "semantic")]
fn run_vectors_refresh(db: PathBuf, model_dir: PathBuf, budget: usize) -> Option<String> {
    fail_open(move || {
        let store = thor_core::event_store::EventStore::open_existing(&db)?;
        let vectors_path = serve::semantic_paths::default_vectors_path(&db);
        let outcome = serve::vectors::refresh(&store, &model_dir, &vectors_path, budget)?;
        Ok(vectors_refresh_line(&outcome))
    })
}

/// One honest line for a real outcome, or `None` when nothing happened worth
/// telling the hourly log about (already fully caught up) - printing "0
/// embedded, 0 deleted" every single hour forever would be exactly the kind
/// of silence-shaped noise nobody would ever actually read.
#[cfg(feature = "semantic")]
fn vectors_refresh_line(outcome: &serve::vectors::RefreshOutcome) -> Option<String> {
    if outcome.model_id_mismatch {
        return Some("vectors refresh: skipped (stored model_id does not match this binary - run vectors-build)".to_string());
    }
    if outcome.embedded == 0 && outcome.deleted == 0 {
        return None;
    }
    Some(format!("vectors refresh: {} embedded, {} deleted, {} remaining", outcome.embedded, outcome.deleted, outcome.remaining))
}

/// Run `f`, collapsing EITHER a returned error OR a real panic into one short
/// line - never lets either reach the caller. `catch_unwind` alone only stops
/// a panic from unwinding past this call; it does not stop Rust's default
/// panic hook from printing its own multi-line message first, so the hook is
/// silenced for the duration of the call and restored immediately after - the
/// same belt-and-braces `bin/serve.rs`'s own `cmd_hook` already uses for the
/// identical "must never be the reason a caller sees more than one line"
/// requirement.
#[cfg(feature = "semantic")]
fn fail_open<F>(f: F) -> Option<String>
where
    F: FnOnce() -> anyhow::Result<Option<String>> + std::panic::UnwindSafe,
{
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(f);
    std::panic::set_hook(previous_hook);
    match result {
        Ok(Ok(line)) => line,
        Ok(Err(e)) => Some(format!("vectors refresh: skipped ({})", one_line(&e.to_string()))),
        Err(_) => Some("vectors refresh: skipped (panicked)".to_string()),
    }
}

/// Collapse `reason` to one line - the same job `ops::ship_state`'s own
/// private `one_line` does for its sidecar, reimplemented here rather than
/// exposed from there: an `anyhow` chain's `{e}` can carry embedded
/// newlines, and this function's whole contract is "at most one line".
#[cfg(feature = "semantic")]
fn one_line(reason: &str) -> String {
    reason.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Recv { db, bind, inbox } => {
            require_token().and_then(|token| transport::run_receiver(&db, &bind, token, inbox))
        }
        Command::Drain { db, from } => require_token().and_then(|token| {
            let summary = ops::drain::drain_from(&db, &from, &token)?;
            for line in &summary.lines {
                println!("{line}");
            }
            println!(
                "drain: {} captured, {} applied, {} NOT applied",
                summary.pulled, summary.applied, summary.not_applied
            );
            // A capture that did not land is not a crash, but it is also not
            // a success: the owner wrote something down and it is not in his
            // memory. Exit non-zero so an hourly job's log shows it.
            anyhow::ensure!(
                summary.all_applied(),
                "{} captured write(s) did NOT land - see the LOST line(s) above for the reason each was refused",
                summary.not_applied
            );
            Ok(())
        }),
        Command::Ship { db, to, batch } => {
            let outcome = require_token().and_then(|token| {
                let store = thor_core::event_store::EventStore::open_existing(&db)?;
                let summary = transport::push_once(&store, &to, &token, batch)?;
                println!(
                    "shipped: {} applied, {} already present, {} batch(es), receiver now at seq {}",
                    summary.applied, summary.skipped, summary.batches, summary.final_cursor
                );
                // Reaching here means `push_once` returned Ok: the receiver
                // agreed with everything shipped, including the "nothing to
                // ship" case (see `push_once`'s own doc comment on the AHEAD/
                // DIFFERENT-tip checks that guard that case from a false
                // success). Record it even though nothing else here reads it
                // this run - see `ops::ship_state`'s own doc comment for the
                // four-week silence this exists to close.
                if let Err(e) = ops::ship_state::record_success(&db, summary.final_cursor) {
                    eprintln!("ship state NOT recorded ({e}) - the shipment above still succeeded");
                }
                Ok(())
            });
            // A failed attempt is recorded too - every error path above,
            // not just `push_once`'s own (a missing token, an unopenable
            // store) - so `ops::health::ship_line` can alarm on a broken
            // hourly ship exactly like it does on a refused or timed-out
            // push, instead of the sidecar simply not moving.
            if let Err(e) = &outcome {
                if let Err(e2) = ops::ship_state::record_failure(&db, &e.to_string()) {
                    eprintln!("ship state NOT recorded ({e2}) - the failure above still stands");
                }
            }
            // Best-effort meaning-search maintenance, piggybacked on every
            // hourly ship (see `hourly_vectors_refresh`'s own doc comment).
            // Runs whether the ship above succeeded or not - vector freshness
            // is unrelated to replication - and can never turn a successful
            // ship into a failed one or vice versa: `outcome` was already
            // computed above and is returned below completely unchanged.
            hourly_vectors_refresh(&db);
            outcome
        }
        Command::Status { db, to } => {
            let remote_token = if to.is_some() { Some(require_token()) } else { None };
            let outcome = (|| -> anyhow::Result<()> {
                let token = remote_token.transpose()?;
                let remote = match (&to, &token) {
                    (Some(url), Some(tok)) => Some((url.as_str(), tok.as_str())),
                    _ => None,
                };
                let status = transport::sync_status(&db, remote)?;
                println!(
                    "local:   contiguous_seq {} (tip {})",
                    status.local_contiguous_seq,
                    short(&status.local_tip_hash)
                );
                match status.remote {
                    None => println!("(no --to given: local status only)"),
                    Some(Ok(remote)) => {
                        let lag = status.local_contiguous_seq - remote.contiguous_seq;
                        if lag == 0 {
                            println!("replica: contiguous_seq {} (reachable) - in sync", remote.contiguous_seq);
                        } else if lag > 0 {
                            println!(
                                "replica: contiguous_seq {} (reachable) - LAG {lag} event(s) not yet replicated",
                                remote.contiguous_seq
                            );
                        } else {
                            println!(
                                "replica: contiguous_seq {} (reachable) - AHEAD by {} (not a pure replica of this store)",
                                remote.contiguous_seq, -lag
                            );
                        }
                    }
                    Some(Err(e)) => println!("replica: UNREACHABLE - {e}"),
                }
                Ok(())
            })();
            outcome
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[cfg(feature = "semantic")]
mod tests {
    use super::*;

    // ---------------------------------------------------------- fail_open()

    /// THE DEFECT THIS PREVENTS: a plain `Result`-based fail-open (map the
    /// error and move on) says nothing about a PANIC - a bug anywhere in the
    /// embedder would still take the whole `sync ship` process down with it,
    /// changing its exit code. This forces an actual panic (not merely a
    /// returned `Err`) through `fail_open` to prove `catch_unwind` is doing
    /// real work here, not just `anyhow`'s own `?`.
    #[test]
    fn fail_open_survives_a_real_panic_not_just_a_returned_error() {
        let line = fail_open(|| -> anyhow::Result<Option<String>> { panic!("synthetic failure for the test") });
        assert_eq!(line.as_deref(), Some("vectors refresh: skipped (panicked)"));
    }

    #[test]
    fn fail_open_collapses_a_returned_error_to_one_line() {
        let line = fail_open(|| -> anyhow::Result<Option<String>> { anyhow::bail!("boom") });
        assert_eq!(line.as_deref(), Some("vectors refresh: skipped (boom)"));
    }

    #[test]
    fn fail_open_passes_an_ok_value_through_untouched() {
        let line = fail_open(|| -> anyhow::Result<Option<String>> { Ok(Some("hello".to_string())) });
        assert_eq!(line.as_deref(), Some("hello"));
    }

    // ---------------------------------------------------- maybe_refresh_vectors()

    /// THE DEFECT THIS PREVENTS: a machine with no semantic model installed
    /// is a normal, supported setup (see `serve::semantic_paths::SearchMode::
    /// ModelMissing`) - the hourly ship must stay completely silent about it,
    /// never print a line that could be mistaken for a problem.
    #[test]
    fn no_model_directory_skips_silently() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert_eq!(maybe_refresh_vectors(&db, None, 0), None);
    }

    // --------------------------------------------------- run_vectors_refresh()

    /// THE SHIP-PATH GUARANTEE: a `serve::vectors::refresh` failure (here, a
    /// model directory that resolves but holds none of the real model files)
    /// must collapse to one short line and a plain `Option`, never propagate
    /// as an `Err` that could flip `sync ship`'s own exit code or add more
    /// than the one line to its output.
    #[test]
    fn a_refresh_error_collapses_to_one_line_never_an_error_value() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("thor.db");
        {
            // A live item must exist, or `refresh` never has a reason to load
            // an embedder at all and would return `Ok` with nothing to do -
            // exactly the case this test must NOT exercise.
            let mut store = thor_core::event_store::EventStore::new(&db_path).unwrap();
            let item = model::item::Item {
                id: "r1".to_string(),
                kind: model::item::Kind::Report,
                text: "a fact that would need embedding".to_string(),
                bindings: vec![],
                severity: None,
                project: Some("test-project".to_string()),
                tags: vec![],
                expires: Some("2027-01-01".to_string()),
                key: None,
                falsifier: None,
                check: None,
            };
            model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        }

        // Deliberately bogus: no model files here at all, so `Embedder::load`
        // inside `serve::vectors::refresh` must fail.
        let bogus_model_dir = dir.path().join("no-such-model");
        let line = run_vectors_refresh(db_path, bogus_model_dir, 0);
        assert!(
            line.as_deref().is_some_and(|l| l.starts_with("vectors refresh: skipped (")),
            "an error must still produce exactly one short line, never a silent nothing: {line:?}"
        );
    }
}
