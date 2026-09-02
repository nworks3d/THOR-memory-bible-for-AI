//! The port's closing gate: open an existing store READ-ONLY (no create, no
//! schema/FTS heal - see EventStore::open_existing), verify chain integrity
//! over every event, run the differential auditor (a second, independently
//! implemented fold that must land on the identical head-sets), check the
//! heads projection (stored head_state/entity_meta against that same fold),
//! check the FTS recall projection (row-set match, then FTS5's own
//! index-integrity check), and print the real event count plus PASS/FAIL per
//! step.
//!
//! Usage: verify <path-to-thor.db> [--rebuild-fts] [--rebuild-heads]
//!
//! `--rebuild-fts` only matters when the FTS step fails: it rebuilds the
//! index from the log (see `rebuild_fts`'s own doc comment for why that is
//! always safe) and re-reports PASS/FAIL. `--rebuild-heads` is the same
//! shape for the heads step: only matters when it fails, rebuilds head_state
//! and entity_meta from the log in one transaction (see
//! `rebuild_heads_projection`'s own doc comment) and re-reports PASS/FAIL.
//! Without the matching flag, a failing step is reported and left alone -
//! never rewritten silently.

use std::path::Path;
use std::process::ExitCode;

use thor_core::auditor::{verify_chain_integrity, DifferentialAuditor};
use thor_core::event_store::{rebuild_fts, verify_fts_integrity, verify_fts_projection, EventStore};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let rebuild_fts_flag = args.iter().any(|a| a == "--rebuild-fts");
    let rebuild_heads_flag = args.iter().any(|a| a == "--rebuild-heads");
    let Some(path) = args.iter().skip(1).find(|a| !a.starts_with("--")) else {
        eprintln!("usage: verify <path-to-thor.db> [--rebuild-fts] [--rebuild-heads]");
        return ExitCode::from(2);
    };
    let path = Path::new(path);

    let mut store = match EventStore::open_existing(path) {
        Ok(s) => s,
        Err(e) => {
            println!("open store: FAIL ({e})");
            return ExitCode::FAILURE;
        }
    };
    println!("open store: PASS ({})", path.display());

    let events = match store.get_all_events() {
        Ok(e) => e,
        Err(e) => {
            println!("read events: FAIL ({e})");
            return ExitCode::FAILURE;
        }
    };
    println!("event count: {}", events.len());

    let mut all_ok = true;

    match verify_chain_integrity(&events) {
        Ok(()) => println!("chain integrity: PASS"),
        Err(e) => {
            println!("chain integrity: FAIL ({e})");
            all_ok = false;
        }
    }

    match DifferentialAuditor::verify_consistency(&events) {
        Ok(()) => println!("differential auditor (second independent fold): PASS"),
        Err(e) => {
            println!("differential auditor (second independent fold): FAIL ({e})");
            all_ok = false;
        }
    }

    let mut heads_ok = true;
    let mut heads_issues: Vec<String> = Vec::new();
    match store.verify_heads_projection() {
        Ok(issues) if issues.is_empty() => println!("heads projection: PASS"),
        Ok(issues) => {
            println!("heads projection: FAIL ({} issue(s))", issues.len());
            for issue in &issues {
                println!("  - {issue}");
            }
            heads_issues = issues;
            heads_ok = false;
        }
        Err(e) => {
            println!("heads projection: FAIL ({e})");
            heads_ok = false;
        }
    }
    if !heads_ok {
        if rebuild_heads_flag {
            let before = heads_issues.len();
            match store.rebuild_heads_projection() {
                Ok(tip) => match store.verify_heads_projection() {
                    Ok(after) if after.is_empty() => {
                        println!("heads rebuild: PASS (tip {tip}, {before} issue(s) -> 0)");
                        heads_ok = true;
                    }
                    Ok(after) => println!(
                        "heads rebuild: FAIL ({before} issue(s) -> {} still present)",
                        after.len()
                    ),
                    Err(e) => println!("heads rebuild: FAIL (rebuilt but re-check errored: {e})"),
                },
                Err(e) => println!("heads rebuild: FAIL ({e})"),
            }
        } else {
            println!(
                "heads: re-run with --rebuild-heads to repair (safe - the projection is a \
                 derived fold of the log, rebuilding it loses nothing)"
            );
        }
    }
    all_ok = all_ok && heads_ok;

    let mut fts_ok = true;
    match verify_fts_projection(store.conn()) {
        Ok(()) => println!("FTS projection: PASS"),
        Err(e) => {
            println!("FTS projection: FAIL ({e})");
            fts_ok = false;
        }
    }
    match verify_fts_integrity(store.conn()) {
        Ok(()) => println!("FTS integrity: PASS"),
        Err(e) => {
            println!("FTS integrity: FAIL ({e})");
            fts_ok = false;
        }
    }
    if !fts_ok {
        if rebuild_fts_flag {
            match rebuild_fts(store.conn()) {
                Ok(n) => {
                    println!("FTS rebuild: PASS ({n} rows reindexed)");
                    fts_ok = true;
                }
                Err(e) => println!("FTS rebuild: FAIL ({e})"),
            }
        } else {
            println!(
                "FTS: re-run with --rebuild-fts to repair (safe - the index is a derived \
                 projection of the log, rebuilding it loses nothing)"
            );
        }
    }
    all_ok = all_ok && fts_ok;

    if all_ok {
        println!("verify: PASS ({} events)", events.len());
        ExitCode::SUCCESS
    } else {
        println!("verify: FAIL");
        ExitCode::FAILURE
    }
}
