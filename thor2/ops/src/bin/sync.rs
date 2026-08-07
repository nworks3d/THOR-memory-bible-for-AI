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
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "sync", about = "Replicate THOR's log to/from a remote copy over a bearer-gated HTTP transport")]
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
        Command::Ship { db, to, batch } => require_token().and_then(|token| {
            let store = thor_core::event_store::EventStore::open_existing(&db)?;
            let summary = transport::push_once(&store, &to, &token, batch)?;
            println!(
                "shipped: {} applied, {} already present, {} batch(es), receiver now at seq {}",
                summary.applied, summary.skipped, summary.batches, summary.final_cursor
            );
            Ok(())
        }),
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
