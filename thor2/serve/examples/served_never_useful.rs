//! Read-only census: prints the exact population `serve::status::store_status`
//! folds into `served_never_marked` - fireable items (`Kind::can_fire`, i.e.
//! `Rule`/`Orientation`) served at least `decay::STALE_AFTER_SERVINGS` times
//! and never once marked useful. On the real store that population is 121
//! items (see `serve::status`'s own doc comment on `ServedNeverMarked`); a
//! bare count is not enough for the review that has to happen next - each of
//! the 121 is where a rule that keeps firing without ever helping would hide,
//! and they need to be looked at one by one. This prints the population
//! itself, one line per item, sorted so the heaviest servers come first.
//!
//! THE DEFINITION IS REUSED, NOT RE-DERIVED. `main` below computes exactly
//! what `serve::status::store_status` computes for `served_never_marked_count`
//! (`serve/src/status.rs`): start from `serve::audit::audit_rows` (the same
//! fireable-item rows `status` itself starts from, each already paired with
//! its `ServingStats`), keep a row when `stats.times_served >=
//! serve::decay::STALE_AFTER_SERVINGS` AND `serve::decay::DecayContext::
//! was_ever_marked_useful` is false for it. Same function, same constant,
//! same two filters, in the same order - never a separate reimplementation
//! that could quietly drift from what `status` actually counts.
//!
//! Read-only. SNU_DB must point at a COPY of a store, never the live one.
//! Opens through `EventStore::open_existing`, the same constructor
//! `stale_anchors.rs` (and the inspection commands - doctor, fsck, status) use
//! precisely because it does no schema work and no FTS heal on open (see that
//! constructor's own doc comment in `core/src/event_store.rs`) - the closest
//! thing this crate exposes to a read-only open. Every store read after that
//! (`audit::audit_rows`, `decay::DecayContext::load`) is a SELECT-only reader
//! too.
//!
//! SNU_DB never falls back to a default path: unset, this prints why and
//! exits before opening anything - no store open, no read of any kind.
//!
//! BINDING SORT. An item's `bindings` is a list, not a single value, and a
//! real item can carry more than one sort at once (for example `Always` and a
//! `Moment` together - see `model::item::Item`'s own sample fixture). The
//! printed `binding` field is every distinct sort the item carries - `always`,
//! `moment`, `target`, joined with `+` when more than one applies, or `none`
//! when the item has no binding at all (never expected from `gate::declare`
//! for a live Rule/Orientation, but a pre-migration item can still carry an
//! empty list, so this never assumes otherwise). The summary's "by binding
//! sort" breakdown groups on this exact joined label, so - like the "by kind"
//! breakdown - every item lands in exactly one bucket and the buckets sum to
//! the population.
//!
//! Run (from the `thor2` workspace root):
//!   SNU_DB=<path to a COPY of a store> cargo run -p serve --example served_never_useful

use model::item::{Binding, Kind};
use serve::audit::audit_rows;
use serve::decay::{DecayContext, STALE_AFTER_SERVINGS};
use std::collections::HashMap;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// One item in the served-never-marked population, exactly the fields this
/// tool prints - reduced from `AuditRow` (`serve::audit::AuditRow`) right
/// after the same two filters `status` applies.
struct Row {
    times_served: usize,
    kind: Kind,
    id: String,
    binding_sort: String,
    text: String,
}

/// Every distinct binding sort `bindings` carries, joined for a one-line
/// report - see this file's own doc comment ("BINDING SORT") for why an item
/// can carry more than one, and why `none` is handled rather than assumed
/// impossible.
fn binding_sort(bindings: &[Binding]) -> String {
    let mut sorts: Vec<&str> = Vec::new();
    if bindings.iter().any(|b| matches!(b, Binding::Always)) {
        sorts.push("always");
    }
    if bindings.iter().any(|b| matches!(b, Binding::Moment(_))) {
        sorts.push("moment");
    }
    if bindings.iter().any(|b| matches!(b, Binding::Target { .. })) {
        sorts.push("target");
    }
    if sorts.is_empty() {
        "none".to_string()
    } else {
        sorts.join("+")
    }
}

/// Count occurrences of each label and return them sorted by label, so a
/// summary line is deterministic across runs rather than following whatever
/// order a hash map happened to iterate in.
fn sorted_counts(labels: impl Iterator<Item = String>) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for label in labels {
        *counts.entry(label).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn main() -> anyhow::Result<()> {
    let Ok(db_path) = std::env::var("SNU_DB") else {
        println!(
            "SNU_DB is not set. Set it to the path of a COPY of a store (never the live \
             thor.db) - this tool refuses to run without one and never falls back to a \
             default path."
        );
        return Ok(());
    };
    let db = PathBuf::from(db_path);
    let store = EventStore::open_existing(&db)?;

    let decay = DecayContext::load(&store);
    let mut population: Vec<Row> = audit_rows(&store)
        .into_iter()
        .filter(|r| r.stats.times_served >= STALE_AFTER_SERVINGS)
        .filter(|r| !decay.was_ever_marked_useful(&r.id))
        .map(|r| Row {
            times_served: r.stats.times_served,
            kind: r.item.kind,
            id: r.id,
            binding_sort: binding_sort(&r.item.bindings),
            text: r.item.text,
        })
        .collect();

    // Descending by times served. Vec::sort_by is stable and audit_rows
    // already hands rows in id-ascending order, so ties keep that order
    // rather than an arbitrary one.
    population.sort_by(|a, b| b.times_served.cmp(&a.times_served));

    println!("population: {}", population.len());

    let by_kind = sorted_counts(population.iter().map(|r| format!("{:?}", r.kind)));
    println!(
        "by kind: {}",
        by_kind.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(", ")
    );

    let by_binding = sorted_counts(population.iter().map(|r| r.binding_sort.clone()));
    println!(
        "by binding sort: {}",
        by_binding.iter().map(|(s, n)| format!("{s}={n}")).collect::<Vec<_>>().join(", ")
    );

    println!();
    for row in &population {
        let snippet: String =
            row.text.chars().take(100).collect::<String>().replace(['\n', '\r'], " ");
        println!(
            "times_served={} kind={:?} id={} binding={} text={}",
            row.times_served, row.kind, row.id, row.binding_sort, snippet
        );
    }

    Ok(())
}
