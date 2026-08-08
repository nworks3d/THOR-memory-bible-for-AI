//! Which facts of one project could plausibly carry a proof, and which could
//! not - read-only, and deliberately unwilling to guess.
//!
//! WHY THE SHORTLIST MATTERS MORE THAN THE COUNT. Only a rule whose check
//! runs and holds may ever block a wrong write, so raising proof coverage is
//! the only thing that widens what this memory can actually stop. But a
//! guessed proof is worse than none: it puts a rule back among the ones
//! allowed to block, on the strength of something nobody chose. So this
//! never proposes a literal. It answers one narrower question - is there a
//! real file this fact is already anchored to, which somebody could read and
//! decide about - and separates the rest out honestly.
//!
//! FOUR BUCKETS, in this order (first match wins):
//!   has-proof     already carries a check. Nothing to do.
//!   anchored      no check, but anchored at a path that EXISTS right now.
//!                 These are the shortlist: a person can open that file.
//!   anchor-gone   no check, anchored at a path that is not there. The anchor
//!                 is the problem, not the missing proof - fix that first.
//!   unanchorable  no check and no path anchor at all: bound to a moment, to
//!                 a command, or to nothing that lives in a file. Most of
//!                 these will never carry a proof and that is the design, not
//!                 a backlog.
//!
//! Read-only throughout: `open_existing` (never creates), SELECT-only reads,
//! and `exists()` on the checkout. Point PROOF_DB at a COPY anyway.
//!
//!   PROOF_DB=<copy> PROOF_PROJECT=<key> PROOF_ROOT=<checkout> \
//!     cargo run --release -p serve --example proof_candidates

use model::item::{Binding, TargetKind};
use std::path::PathBuf;
use thor_core::event_store::EventStore;

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("PROOF_DB").expect("set PROOF_DB - point it at a COPY"));
    let project = std::env::var("PROOF_PROJECT").expect("set PROOF_PROJECT");
    let root = PathBuf::from(std::env::var("PROOF_ROOT").expect("set PROOF_ROOT"));

    let store = EventStore::open_existing(&db)?;
    let (mut has_proof, mut unanchorable, mut outside) = (0usize, 0usize, 0usize);
    let (mut anchored, mut anchor_gone) = (Vec::new(), Vec::new());

    /// An anchor that names somewhere else on the machine entirely (a drive
    /// letter, a UNC share). It may well exist, but not INSIDE the checkout,
    /// and a check resolves relative to the root and refuses to escape it -
    /// so a proof on one is impossible, not merely absent. The health check
    /// skips these for the same reason; counting them as candidates put 15
    /// NAS-share paths on a shortlist nobody could ever act on.
    fn points_outside(value: &str) -> bool {
        value.contains(':') || value.starts_with('/') || value.starts_with("\\\\")
    }

    for li in serve::live::live_items(&store).iter().filter(|li| li.item.kind.can_fire()) {
        if li.item.project.as_deref() != Some(project.as_str()) {
            continue;
        }
        if li.item.check.is_some() {
            has_proof += 1;
            continue;
        }
        let paths: Vec<&String> = li
            .item
            .bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Target { kind: TargetKind::Path, value } => Some(value),
                _ => None,
            })
            .collect();
        if paths.is_empty() {
            unanchorable += 1;
            continue;
        }
        if paths.iter().all(|p| points_outside(p)) {
            outside += 1;
            continue;
        }
        let paths: Vec<&String> = paths.into_iter().filter(|p| !points_outside(p)).collect();
        // `exists`, not `is_file`: an anchor may legitimately name a
        // DIRECTORY, and calling those dead would contradict the health
        // check, which uses exists() too. Caught by the two disagreeing.
        let live: Vec<&&String> = paths.iter().filter(|p| root.join(p.replace('\\', "/")).exists()).collect();
        let row = serde_json::json!({
            "id": li.id,
            "project": project,
            "path": live.first().map(|p| p.as_str()).unwrap_or(paths[0].as_str()),
            "text": li.item.text,
            "falsifier": li.item.falsifier,
        });
        if live.is_empty() {
            anchor_gone.push(row);
        } else {
            anchored.push(row);
        }
    }

    let total = has_proof + anchored.len() + anchor_gone.len() + unanchorable + outside;
    println!("{project}: {total} fireable item(s)");
    println!("  already carry a proof : {has_proof}");
    println!("  SHORTLIST (anchored at a file that exists, no proof yet) : {}", anchored.len());
    println!("  anchor points at nothing, fix the anchor first           : {}", anchor_gone.len());
    println!("  no path anchor at all, most will never carry a proof     : {unanchorable}");
    println!("  anchored OUTSIDE the checkout, a proof is impossible     : {outside}");

    std::fs::write("proof-shortlist.json", serde_json::to_string_pretty(&anchored)?)?;
    println!("\nwrote proof-shortlist.json ({} item(s)) - nothing here proposes a literal", anchored.len());
    Ok(())
}
