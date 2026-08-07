//! The port's closing gate: open an existing store READ-ONLY (no create, no
//! schema/FTS heal - see EventStore::open_existing), verify chain integrity
//! over every event, run the differential auditor (a second, independently
//! implemented fold that must land on the identical head-sets), and print the
//! real event count plus PASS/FAIL per step.
//!
//! Usage: verify <path-to-thor.db>

use std::path::Path;
use std::process::ExitCode;

use thor_core::auditor::{verify_chain_integrity, DifferentialAuditor};
use thor_core::event_store::EventStore;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: verify <path-to-thor.db>");
        return ExitCode::from(2);
    };
    let path = Path::new(&path);

    let store = match EventStore::open_existing(path) {
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

    if all_ok {
        println!("verify: PASS ({} events)", events.len());
        ExitCode::SUCCESS
    } else {
        println!("verify: FAIL");
        ExitCode::FAILURE
    }
}
