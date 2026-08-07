//! ONE-OFF working tool for the identifier-battery measurement (LANE-A,
//! 2026-08-05). Dumps the exact corpus `search`/`search_best_effort_cached`
//! serve (non-Lookup, non-expired, via the same public `search_with_expired`
//! the live surface calls) to a JSON file, so a battery can be built by
//! reading real fact text for query/gold selection.
//!
//! Read-only. Point DUMP_DB at a COPY of the store, never the live one.
//! Output is written wherever DUMP_OUT says - point it at the scratchpad,
//! never into this repo (the dump carries real private fact text).
//!
//! Not part of any shipped surface: a measurement-only example, like
//! `recall_native`'s own battery-dump env vars.

use serve::lookup::search_with_expired;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("DUMP_DB").expect("set DUMP_DB to a COPY of thor.db"));
    let store = EventStore::new(&db)?;
    let (hits, withheld) = search_with_expired(&store, "");
    eprintln!("live non-Lookup items: {}   withheld as expired: {}", hits.len(), withheld);

    let out: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            serde_json::json!({
                "id": h.id,
                "kind": format!("{:?}", h.item.kind),
                "text": h.item.text,
                "tags": h.item.tags,
                "project": h.item.project,
                "key": h.item.key,
            })
        })
        .collect();

    let out_path = std::env::var("DUMP_OUT").expect("set DUMP_OUT");
    std::fs::write(&out_path, serde_json::to_string_pretty(&out)?)?;
    println!("wrote {} items to {}", out.len(), out_path);
    Ok(())
}
