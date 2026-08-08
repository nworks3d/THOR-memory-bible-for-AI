//! Draw the sample for the measurement 2.0 never took: of the facts this
//! memory actually serves, how many are simply UNTRUE about the repository
//! they claim to describe?
//!
//! Temporary probe, deliberately not part of the shipped surface. Picks the
//! most-served project-scoped fireable items whose project has a working
//! copy, deterministically (servings desc, then id), and writes them as JSON
//! for a reader to check one by one. Nothing here judges anything: a machine
//! that could tell a true fact from a false one is the thing this project
//! does not have, which is exactly why the sample has to be read.

use std::collections::HashMap;
use thor_core::event_store::{EventKind, EventStore};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let db = args.next().expect("usage: truth_sample <db> <checkouts> <n>");
    let checkouts = args.next().expect("usage: truth_sample <db> <checkouts> <n>");
    let want: usize = args.next().unwrap_or_else(|| "100".to_string()).parse()?;

    let store = EventStore::open_existing(std::path::Path::new(&db))?;

    let mut served: HashMap<String, usize> = HashMap::new();
    for (kind, id) in store.event_kinds()? {
        if kind == EventKind::ItemServed {
            *served.entry(id).or_default() += 1;
        }
    }

    // Only projects with a real working copy: a fact cannot be checked
    // against a repository that is not on this disk.
    let mut roots: HashMap<String, std::path::PathBuf> = HashMap::new();
    for entry in std::fs::read_dir(&checkouts)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(key) = serve::project::resolve_project(&path) {
                roots.insert(key, path);
            }
        }
    }

    let live = serve::live::live_items(&store);
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for li in live.iter().filter(|li| li.item.kind.can_fire()) {
        let Some(project) = li.item.project.clone() else { continue };
        let Some(root) = roots.get(&project) else { continue };
        if li.item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always)) {
            continue;
        }
        let anchors: Vec<String> = li
            .item
            .bindings
            .iter()
            .filter_map(|b| match b {
                model::item::Binding::Target { kind, value } => Some(format!("{kind:?}:{value}")),
                model::item::Binding::Moment(a) => Some(format!("Moment:{a:?}")),
                model::item::Binding::Always => None,
            })
            .collect();
        rows.push(serde_json::json!({
            "id": li.id,
            "text": li.item.text,
            "project": project,
            "root": root.to_string_lossy(),
            "anchors": anchors,
            "check": li.item.check.as_ref().map(|c| format!("{c:?}")),
            "served": served.get(&li.id).copied().unwrap_or(0),
        }));
    }

    rows.sort_by(|a, b| {
        let sb = b["served"].as_u64().unwrap_or(0);
        let sa = a["served"].as_u64().unwrap_or(0);
        sb.cmp(&sa).then_with(|| a["id"].as_str().unwrap_or("").cmp(b["id"].as_str().unwrap_or("")))
    });
    rows.truncate(want);
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}
