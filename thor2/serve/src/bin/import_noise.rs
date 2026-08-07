//! One-off import: bring 1.0's noise judgements into 2.0 as real events.
//!
//! WHY. Decay by silence was falsified on its first production day (see
//! `serve::decay`): zero marks of usefulness occur in real use, so "never
//! marked useful" is a constant and the rule became a timer that retired the
//! most-used rules first. What decay needed instead was an explicit NEGATIVE
//! signal - and the owner had already produced 182 of them. 1.0 kept a
//! `noise` namespace in its sidecar ledger: every time a served fact was a
//! distraction, it was recorded, with a count. The migration left the whole
//! ledger behind, so 455 fireable 2.0 items descend from a fact he had
//! explicitly called noise, one of them nine times.
//!
//! WHAT THIS DOES. For every 1.0 fact with a noise count, find the 2.0 items
//! derived from it (their `source:<id>` tag) and append that many
//! `item_marked_noise` events. The count is carried, not flattened: his own
//! threshold treats one judgement as "not here, this time" and two as a
//! pattern, so collapsing nine marks into one would throw away the strongest
//! signal in the ledger.
//!
//! Judgements are events, not a flag: they land in the same hash-chained log
//! as everything else, `history` walks them, and a later mark of usefulness
//! overrides them permanently without anything to undo.
//!
//! NOT RUN, AND HERE IS WHY - the measurement that stopped it (2026-08-03).
//! A dry run said 599 items, 998 judgements, 194 taken past the retirement
//! line. Applied to a COPY and inspected, those 194 included 153 fireable
//! rules and orientations, and among them THREE of severity `irreversible`
//! and ELEVEN `costly`: rules about what may go to GitHub versus what is a
//! secret, about not widening a token's scope, about not automating the
//! unlock of an encrypted volume.
//!
//! Those judgements are real, and they are also EVIDENCE FROM A BROKEN
//! REGIME. 1.0 scoped nothing by project, and the same day this tool was
//! written that defect was found and fixed on the 2.0 side: every project's
//! rules had been competing in every project. So "this was noise" most likely
//! meant "this fired somewhere it did not belong" - the exact condition the
//! scoping fix removes. Importing them now would retire safety rules for a
//! bug that no longer exists, and would do it silently.
//!
//! Judging that on a severity exception list would be a compensating
//! mechanism (CONTRACT R9): the problem is not which rules are heavy, it is
//! that the judgements were made about different behaviour. So the mechanism
//! ships and the historical ledger does not get imported. Every noise
//! judgement from now on is made against correct scoping and means what it
//! says.
//!
//! The tool stays because the reasoning above could turn out to be wrong: if
//! the same items get judged noise again under correct scoping, that is a new
//! and valid signal, and it will arrive through the ordinary `mark --noise`
//! path rather than through a bulk import.
//!
//! Idempotent: a second run counts what is already there and appends only the
//! difference.
//!
//! Usage:
//!     import_noise --db <2.0 store> --ledger <1.0 ledger.db>
//!     ... --apply     to write

use clap::Parser;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(name = "import-noise", about = "Carry 1.0's explicit noise judgements into 2.0 as events")]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    /// 1.0's ledger sidecar (the `kv` table with an `ns` of "noise").
    #[arg(long)]
    ledger: PathBuf,
    #[arg(long)]
    apply: bool,
}

/// `<1.0 fact id> -> how many times it was called noise`, read from a COPY.
/// The 1.0 ledger is never opened in place: this whole rebuild promised not
/// to touch 1.0, and a read that heals a schema is still a write.
fn ledger_noise(path: &std::path::Path) -> anyhow::Result<BTreeMap<String, usize>> {
    let work = std::env::temp_dir().join(format!("thor2-ledger-read-{}.db", std::process::id()));
    std::fs::copy(path, &work)?;
    let result = (|| -> anyhow::Result<BTreeMap<String, usize>> {
        let conn = rusqlite::Connection::open(&work)?;
        let mut stmt = conn.prepare("SELECT k, v FROM kv WHERE ns = 'noise'")?;
        let mut out = BTreeMap::new();
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (key, value) = row?;
            if let Ok(n) = value.trim().parse::<usize>() {
                if n > 0 {
                    out.insert(key, n);
                }
            }
        }
        Ok(out)
    })();
    let _ = std::fs::remove_file(&work);
    result
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let ledger = match ledger_noise(&cli.ledger) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cannot read the ledger at {}: {e}", cli.ledger.display());
            return ExitCode::FAILURE;
        }
    };
    if ledger.is_empty() {
        println!("no noise judgements in that ledger - nothing to import");
        return ExitCode::SUCCESS;
    }
    println!("noise judgements in 1.0: {} fact(s), {} mark(s) total", ledger.len(), ledger.values().sum::<usize>());

    let mut store = match EventStore::open_existing(&cli.db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the store at {}: {e}", cli.db.display());
            return ExitCode::FAILURE;
        }
    };

    let already = serve::usefulness::noise_counts(&store);

    // Every live 2.0 item, with the 1.0 fact it came from.
    let mut planned: Vec<(String, usize)> = Vec::new();
    let mut fireable = 0usize;
    for live in serve::live::live_items(&store) {
        let Some(source) = live.item.tags.iter().find_map(|t| t.strip_prefix("source:")) else { continue };
        let Some(&want) = ledger.get(source) else { continue };
        let have = already.get(&live.id).copied().unwrap_or(0);
        if want > have {
            if live.item.kind.can_fire() {
                fireable += 1;
            }
            planned.push((live.id.clone(), want - have));
        }
    }

    if planned.is_empty() {
        println!("nothing to import: every judgement is already in the log");
        return ExitCode::SUCCESS;
    }
    let events: usize = planned.iter().map(|(_, n)| n).sum();
    println!();
    println!("2.0 items to judge: {} ({fireable} of them fireable)", planned.len());
    println!("judgement events to append: {events}");
    let retiring = planned
        .iter()
        .filter(|(id, n)| already.get(id).copied().unwrap_or(0) + n >= serve::decay::NOISE_MARKS_BEFORE_STALE)
        .count();
    println!(
        "items this would take past the {}-judgement line (retired from the injection surfaces, still findable): {}",
        serve::decay::NOISE_MARKS_BEFORE_STALE,
        retiring
    );

    if !cli.apply {
        println!("\nnothing written (pass --apply to import)");
        return ExitCode::SUCCESS;
    }

    // One timestamp for the whole import, and it says what it is: these are
    // carried judgements, not judgements made today.
    let marked_at = serve::time::now_iso8601();
    let mut written = 0usize;
    let mut failed = 0usize;
    for (id, n) in &planned {
        for _ in 0..*n {
            match serve::mark::record_noise(&mut store, "import", "import", "import-noise-1.0", &marked_at, id) {
                Ok(_) => written += 1,
                Err(e) => {
                    eprintln!("{id}: {e}");
                    failed += 1;
                }
            }
        }
    }
    println!("\nimported: {written} judgement(s)");
    if failed > 0 {
        eprintln!("failed:   {failed}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
