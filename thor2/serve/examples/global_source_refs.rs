//! Read-only census: live GLOBAL Rule/Orientation items that name a source
//! file, either in their text or in a Path anchor.
//!
//! The write gate refuses these since 2026-08-07 (ground 19), but only for
//! NEW writes - everything declared before that is grandfathered in and still
//! served in every project on the machine. This counts what is left.
//!
//!   GLOBAL_REF_DB=<copy> cargo run --release -p serve --example global_source_refs

use std::path::PathBuf;
use thor_core::event_store::EventStore;

const SOURCE_EXT: &[&str] = &[".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".rb",
    ".php", ".c", ".h", ".cpp", ".hpp", ".cs", ".swift", ".kt", ".sh", ".ps1", ".sql", ".vue", ".lua"];

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("GLOBAL_REF_DB").expect("set GLOBAL_REF_DB"));
    let store = EventStore::open_existing(&db)?;

    let mut hits = Vec::new();
    let mut global_fireable = 0usize;
    let mut anchored = 0usize;
    for li in serve::live::live_items(&store).iter().filter(|li| li.item.kind.can_fire()) {
        if li.item.project.is_some() {
            continue;
        }
        global_fireable += 1;
        if li.item.bindings.iter().any(|b| matches!(b,
            model::item::Binding::Target { kind: model::item::TargetKind::Path, value }
            if SOURCE_EXT.iter().any(|e| value.to_lowercase().ends_with(e))))
        {
            anchored += 1;
        }
        // The gate's own predicate, so this counts exactly what it refuses.
        let mut probe = li.item.clone();
        probe.id = format!("{}-probe", probe.id);
        if let Err(refusal) = model::gate::declare(&probe) {
            if refusal.problem.contains("source file") || refusal.fix.contains("project") {
                hits.push((li.item.id.clone(), refusal.problem.clone()));
            }
        }
    }

    println!("live global rules/orientations: {global_fireable}");
    println!("of those, anchored at a source-file PATH: {anchored}");
    println!("of those, the gate would refuse today: {}", hits.len());
    for (id, why) in hits.iter().take(25) {
        println!("  {id}  {why}");
    }
    if hits.len() > 25 {
        println!("  ... and {} more", hits.len() - 25);
    }
    Ok(())
}
