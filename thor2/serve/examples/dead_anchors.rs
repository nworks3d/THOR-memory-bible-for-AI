//! Read-only census of the anchors `doctor`'s own decay line counts as dead,
//! split by what it would actually take to fix each one.
//!
//! WHY A SECOND TOOL NEXT TO `stale_anchors`. That one answers "how many
//! anchors resolve", against ONE root, using tail matching. This one answers
//! the question a repair needs: for each anchor `ops::health::decay_line`
//! counts as dead - same rule, same per-project checkout resolution, so the
//! totals agree with `doctor` exactly - is the file really gone, or is it
//! sitting right there under a longer path because the anchor was written
//! without its prefix? Those need opposite treatments, and the count alone
//! cannot tell them apart.
//!
//! The measure this reproduces is deliberately strict: `<checkout>/<value>`
//! must exist, joined directly. An anchor written as a partial path is
//! therefore counted dead even when the file exists further down the tree.
//! That is not a flaw to work around here - a partial anchor is genuinely
//! weaker, because resolution has to guess - but it does mean a large share
//! of "dead" anchors are repairable by adding a prefix rather than by
//! retracting anything.
//!
//! Read-only throughout: `open_existing` (never creates), SELECT-only reads,
//! and `read_dir` on the checkouts. Point DEAD_ANCHOR_DB at a COPY anyway.
//!
//!   DEAD_ANCHOR_DB=<copy of thor.db> DEAD_ANCHOR_CHECKOUTS=<dir of checkouts> \
//!     cargo run --release -p serve --example dead_anchors

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

/// Every file under `root`, as its path relative to `root` with forward
/// slashes, lowercased. Built once per checkout.
fn relative_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Build output and version control carry tens of thousands of
            // files that no anchor is ever about, and walking them dwarfs
            // the rest of this tool's runtime.
            // `.claude` holds leftover git worktrees: whole stale copies of
            // the tree, at whatever layout they were cut at. Indexing them
            // makes every file that has since MOVED look like it is still in
            // its old place, and a repair built on that would point live
            // anchors into a throwaway checkout. Caught 2026-08-07 when the
            // census proposed repointing docs/OPTIONAL-FEATURES.md at a
            // worktree copy instead of the docs/1.0/ it had just moved to.
            if name == ".git" || name == ".claude" || name == "target" || name == "node_modules" {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() => {
                    if let Ok(rel) = path.strip_prefix(root) {
                        out.push(rel.to_string_lossy().replace('\\', "/").to_lowercase());
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("DEAD_ANCHOR_DB").expect("set DEAD_ANCHOR_DB - point it at a COPY"));
    let checkouts =
        PathBuf::from(std::env::var("DEAD_ANCHOR_CHECKOUTS").expect("set DEAD_ANCHOR_CHECKOUTS"));

    let store = EventStore::open_existing(&db)?;

    // Same project resolution as `ops::health::decay_line`, so the totals
    // below can be compared with `doctor` directly.
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

    let mut listings: BTreeMap<String, Vec<String>> = Default::default();
    for (key, root) in &roots {
        listings.insert(key.clone(), relative_files(root));
    }

    let items = serve::live::live_items(&store);
    let (mut fixable, mut gone) = (Vec::new(), Vec::new());

    for li in items.iter().filter(|li| li.item.kind.can_fire()) {
        let Some(project) = li.item.project.as_deref() else { continue };
        let Some(base) = roots.get(project) else { continue };
        for binding in &li.item.bindings {
            let model::item::Binding::Target { kind: model::item::TargetKind::Path, value } = binding
            else {
                continue;
            };
            if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                continue;
            }
            let normalised = value.replace('\\', "/");
            if base.join(&normalised).exists() {
                continue;
            }

            // Dead by doctor's measure. Is the file nevertheless in the tree,
            // under a longer path? Tail match on whole components, so
            // "gate.rs" never matches "delegate.rs".
            let needle = normalised.to_lowercase();
            let empty = Vec::new();
            let candidates: Vec<&String> = listings
                .get(project)
                .unwrap_or(&empty)
                .iter()
                .filter(|real| *real == &needle || real.ends_with(&format!("/{needle}")))
                .collect();

            let row = serde_json::json!({
                "id": li.item.id,
                "project": project,
                "old": normalised,
                "candidates": candidates,
                "kind": format!("{:?}", li.item.kind),
                "text": li.item.text,
                "other_bindings": li.item.bindings.len() - 1,
                "has_check": li.item.check.is_some(),
            });
            if candidates.len() == 1 {
                fixable.push(serde_json::json!({
                    "id": li.item.id,
                    "project": project,
                    "old": normalised,
                    "new": candidates[0],
                }));
            } else {
                gone.push(row);
            }
        }
    }

    println!("dead anchors: {}", fixable.len() + gone.len());
    println!("  repairable by adding a prefix (exactly one candidate): {}", fixable.len());
    println!("  no candidate, or more than one:                        {}", gone.len());
    std::fs::write("dead-anchors-fixable.json", serde_json::to_string_pretty(&fixable)?)?;
    std::fs::write("dead-anchors-gone.json", serde_json::to_string_pretty(&gone)?)?;
    println!("\nwrote dead-anchors-fixable.json and dead-anchors-gone.json");
    Ok(())
}
