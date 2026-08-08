//! Does a fact stay silent because its moment never came, or because it
//! loses every time the moment DOES come?
//!
//! WHY THIS QUESTION AND NOT THE COUNT. A census said 2443 of 2817 facts had
//! never once fired, which reads like a store full of dead weight. It is not
//! that simple: a fact anchored at a file nobody touched this year is doing
//! exactly its job by waiting, and every anchor in that store resolves, so
//! none of them is silent through rot. But a block shows at most a handful of
//! items, so a fact CAN be eligible at a moment that really happens and still
//! never surface. Those two look identical from outside - both are "never
//! fired" - and they need opposite treatments. Retiring the second kind is
//! the right call; retiring the first kind throws away the warning you wrote
//! for the day somebody finally opens that file.
//!
//! HOW IT MEASURES. For every distinct path anchor that resolves inside a
//! checkout, it asks the REAL selector (`serve::serve`, the same call
//! `check` and `why` make) what applies there and what the block would show.
//! An item that appears in `shown` at even one of its own anchors is
//! reachable. One that appears in `all` but never in `shown`, at every anchor
//! it carries, is structurally invisible: the moment arrives, the item is
//! eligible, and something always outranks it.
//!
//! WHERE THIS IS WRONG, both directions. It probes an anchor as though the
//! file were being touched ALONE, which is how a file guard sees it, so a
//! real turn that also carries a command or a moment can rank differently -
//! an item called reachable here might still lose in practice (under-reports
//! crowding). And it only probes anchors that resolve today, so a fact whose
//! moment is a command or an action it never sees is counted nowhere at all
//! rather than as waiting (under-reports the waiting group).
//!
//! Read-only: `open_existing`, SELECT-only reads, no writes anywhere.
//!
//!   CROWD_DB=<copy> CROWD_CHECKOUTS=<dir of checkouts> \
//!     cargo run --release -p serve --example crowding

use model::item::{Binding, TargetKind};
use serve::input::ServeInput;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// The item cap to simulate alongside the real one. The character budget is
/// deliberately NOT changed: the question is what a higher item count buys
/// when the block may not get any longer to read.
const ALT_MAX_ITEMS: usize = 10;

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("CROWD_DB").expect("set CROWD_DB - point it at a COPY"));
    let checkouts = PathBuf::from(std::env::var("CROWD_CHECKOUTS").expect("set CROWD_CHECKOUTS"));

    let store = EventStore::open_existing(&db)?;

    let mut roots: BTreeMap<String, PathBuf> = Default::default();
    if let Ok(entries) = std::fs::read_dir(&checkouts) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    roots.insert(key, path);
                }
            }
        }
    }

    // Every distinct anchor worth probing, and who claims it.
    let mut anchors: BTreeMap<(String, String), BTreeSet<String>> = Default::default();
    let mut anchored_items: HashSet<String> = Default::default();
    for li in serve::live::live_items(&store).iter().filter(|li| li.item.kind.can_fire()) {
        let Some(project) = li.item.project.clone() else { continue };
        let Some(root) = roots.get(&project) else { continue };
        for binding in &li.item.bindings {
            let Binding::Target { kind: TargetKind::Path, value } = binding else { continue };
            if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                continue;
            }
            if !root.join(value.replace('\\', "/")).exists() {
                continue;
            }
            anchors.entry((project.clone(), value.clone())).or_default().insert(li.id.clone());
            anchored_items.insert(li.id.clone());
        }
    }

    println!("probing {} distinct anchor(s) across {} checkout(s)\n", anchors.len(), roots.len());

    let mut reachable: HashSet<String> = Default::default();
    let mut eligible_somewhere: HashSet<String> = Default::default();
    let mut reachable_at_alt: HashSet<String> = Default::default();
    let mut alt_shown_total = 0usize;
    let mut crowded_anchors = 0usize;
    let mut withheld_total = 0usize;
    let mut worst: Vec<(usize, usize, String, String)> = Vec::new();

    for ((project, path), _claimants) in &anchors {
        let mut input = ServeInput::default();
        input.add_file(path);
        // WITHOUT THIS the probe is worthless and looks fine. `rank::select`
        // filters on the input's own project (`project::applies_to`), so an
        // empty one lets only GLOBAL items through: the first run of this
        // tool reported 26 eligible items across 225 anchors and almost no
        // crowding, because every project-scoped fact was silently filtered
        // out. Caught by running the same probe from the real checkout by
        // hand and getting 14 where the tool said 0.
        input.project = Some(project.clone());
        let served = serve::serve(&store, &input);

        // What a HIGHER item cap would show, with the character budget left
        // exactly where it is. Both caps apply and the first to bind stops the
        // block, so raising only the item count cannot make the block longer
        // to read - it only lets SHORT facts use the room that long ones
        // already would have. Simulated over the same ranking the real cap
        // consumes, so this is the production order, not a re-sort.
        let mut used = 0usize;
        let mut would_show: HashSet<&str> = HashSet::new();
        for r in &served.all {
            if would_show.len() >= ALT_MAX_ITEMS {
                break;
            }
            let len = r.item.text.chars().count();
            if used + len > serve::render::MAX_BLOCK_CHARS {
                break;
            }
            used += len;
            would_show.insert(r.id.as_str());
        }
        for id in &would_show {
            reachable_at_alt.insert((*id).to_string());
        }
        alt_shown_total += would_show.len();

        let shown: HashSet<&str> = served.selection.shown.iter().map(|r| r.id.as_str()).collect();
        for r in &served.all {
            eligible_somewhere.insert(r.id.clone());
            if shown.contains(r.id.as_str()) {
                reachable.insert(r.id.clone());
            }
        }
        if served.all.len() > served.selection.shown.len() {
            crowded_anchors += 1;
            withheld_total += served.all.len() - served.selection.shown.len();
            worst.push((served.all.len(), served.selection.shown.len(), project.clone(), path.clone()));
        }
    }

    let invisible: Vec<&String> = eligible_somewhere.iter().filter(|id| !reachable.contains(*id)).collect();
    let invisible_at_alt: Vec<&String> =
        eligible_somewhere.iter().filter(|id| !reachable_at_alt.contains(*id)).collect();

    println!("anchors where more applies than the block can show : {crowded_anchors} of {}", anchors.len());
    println!("items withheld, summed over every anchor probed     : {withheld_total}");
    println!();
    println!("items eligible at one of their own anchors          : {}", eligible_somewhere.len());
    println!("  of those, reachable (shown at least once)         : {}", reachable.len());
    println!("  of those, STRUCTURALLY INVISIBLE (never shown)    : {}", invisible.len());
    println!();
    println!("anchored items that no probe ever found eligible    : {}", anchored_items.difference(&eligible_somewhere).count());
    println!("  (their anchor resolves, so they are waiting for a moment this probe does not stage)");

    println!();
    println!("SIMULATED at {ALT_MAX_ITEMS} items, character budget unchanged:");
    println!("  reachable                                        : {}", reachable_at_alt.len());
    println!("  still structurally invisible                     : {}", invisible_at_alt.len());
    println!("  items shown summed over every anchor (was {})", alt_shown_total);

    worst.sort_by(|a, b| b.0.cmp(&a.0));
    println!("\nthe most crowded anchors:");
    for (all, shown, project, path) in worst.iter().take(10) {
        println!("  {all:>3} apply, {shown} shown  [{project}] {path}");
    }

    let mut by_id: HashMap<&str, ()> = Default::default();
    for id in &invisible {
        by_id.insert(id.as_str(), ());
    }
    std::fs::write("crowding-invisible.json", serde_json::to_string_pretty(&invisible)?)?;
    println!("\nwrote crowding-invisible.json ({} id(s))", invisible.len());
    Ok(())
}
