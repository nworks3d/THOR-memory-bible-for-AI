//! Which live facts can never reach a block, and what is holding the ground.
//!
//! One-shot census behind the capacity refusal (`model::store::capacity`).
//! Every item it lists is one that was written BEFORE that refusal existed:
//! every binding it carries is already full of heavier rivals, so it is cover
//! that looks real and fires nowhere. Read-only.

use model::store::{capacity, Capacity};
use thor_core::event_store::EventStore;

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("usage: unblockable <db>");
    let store = EventStore::open_existing(std::path::Path::new(&db))?;
    let live = serve::live::live_items(&store);

    let mut rows: Vec<serde_json::Value> = Vec::new();
    for li in live.iter().filter(|li| li.item.kind.can_fire()) {
        let Capacity::DeadOnArrival(refusal) = capacity(&store, &li.item)? else { continue };
        let bindings: Vec<String> = li
            .item
            .bindings
            .iter()
            .map(|b| match b {
                model::item::Binding::Target { kind, value } => format!("{kind:?}:{value}"),
                model::item::Binding::Moment(a) => format!("Moment:{a:?}"),
                model::item::Binding::Always => "Always".to_string(),
            })
            .collect();
        rows.push(serde_json::json!({
            "id": li.id,
            "text": li.item.text,
            "project": li.item.project,
            "severity": li.item.severity.map(|s| format!("{s:?}")),
            "bindings": bindings,
            "check": li.item.check.is_some(),
            "why": format!("{refusal}"),
        }));
    }
    rows.sort_by(|a, b| a["id"].as_str().unwrap_or("").cmp(b["id"].as_str().unwrap_or("")));
    eprintln!("{} unblockable", rows.len());
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}
