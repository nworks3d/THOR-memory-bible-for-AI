//! Apply the OLD system's pins onto this store. In 1.0 a standing rule was
//! never part of the fact body itself - it was a separate sidecar list of
//! fact ids (see `thor_core::ledger`), so the mechanical JSONL migration
//! (`crate::migrate`), which only ever reads fact bodies, could never have
//! carried that layer over. This module is the one-time repair: for every
//! pinned old id, find the live item(s) this store tagged with
//! `source:<old-id>` (the migration's own provenance tag - see
//! `crate::migrate::source_facts_represented`) and give each one the
//! `Always` binding, through the real write gate
//! (`model::store::revise`), never around it.
//!
//! A pin names an old FACT, but one old fact can have produced several new
//! typed items sharing the same `source:` tag - a `Report` (the archive copy
//! of the old note, verbatim) alongside one or more `Rule`/`Orientation`
//! items (what 2.0 actually serves). Only the latter can ever carry a
//! binding at all (`model::item::Kind::can_fire`; the write gate already
//! refuses a binding on archive material) - a matched `Report` is skipped
//! by design, not an error, exactly as every other serve-path decision
//! already treats archive kinds.

use crate::live::{live_items, LiveItem};
use model::item::{Binding, Item};
use model::store::{self, WriteError};
use thor_core::event_store::EventStore;

/// Outcome of one `apply_pins` run. Every pin is accounted for in exactly one
/// of `pins_linked`/`unmatched_pins`, so `pins_linked + unmatched_pins.len() ==
/// pins_total` always holds - the same bookkeeping-identity discipline
/// `serve::migrate::MigrationReport` already uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinReport {
    pub pins_total: usize,
    /// Pins for which at least one live item's `source:` tag matched.
    pub pins_linked: usize,
    /// Pins for which NO live item's `source:` tag matched at all - a
    /// RESULT to report, never guessed at or silently dropped.
    pub unmatched_pins: Vec<String>,
    /// Matched items that received a fresh `Always` binding this run.
    pub items_bound: usize,
    /// Matched items that already carried `Always` (a rerun is idempotent:
    /// nothing is re-applied, nothing is double-counted as newly bound).
    pub items_already_always: usize,
    /// Matched items whose kind can never carry any binding (the archive
    /// `Report` copy the migration also writes for the same source) -
    /// skipped by design; never even offered to the write gate.
    pub items_not_fireable: usize,
    /// A `WriteError` the gate returned for an item that WAS fireable. Not
    /// expected in practice (a live Rule/Orientation the gate already
    /// accepted can only be refused here if its bindings/text changed since),
    /// carried loudly rather than folded into any other count - R5's "write
    /// and declare is never silent" applies here exactly as it does to
    /// `serve::migrate`'s own `hard_errors`.
    pub hard_errors: Vec<String>,
}

fn already_bound_always(item: &Item) -> bool {
    item.bindings.iter().any(|b| matches!(b, Binding::Always))
}

/// Every live item whose tags carry an exact `source:<old_id>` match - the
/// same plain string equality the migration's own tag scheme is written in
/// (`source:` plus the old id, verbatim, nothing fuzzy or prefixed).
fn matches_for(candidates: &[LiveItem], old_id: &str) -> Vec<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.item.tags.iter().any(|t| t.strip_prefix("source:") == Some(old_id)))
        .map(|(i, _)| i)
        .collect()
}

/// Apply every pin in `old_ids` to `store`: give every live item tagged
/// `source:<old_id>` the `Always` binding, through `model::store::revise`.
/// Reads live items ONCE up front (`crate::live::live_items`, the one reader
/// every other surface uses) rather than re-deriving liveness per pin.
pub fn apply_pins(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    old_ids: &[String],
) -> PinReport {
    let mut report = PinReport { pins_total: old_ids.len(), ..Default::default() };
    let candidates = live_items(store);

    for old_id in old_ids {
        let idxs = matches_for(&candidates, old_id);
        if idxs.is_empty() {
            report.unmatched_pins.push(old_id.clone());
            continue;
        }
        report.pins_linked += 1;

        for idx in idxs {
            let live = &candidates[idx];
            if !live.item.kind.can_fire() {
                report.items_not_fireable += 1;
                continue;
            }
            if already_bound_always(&live.item) {
                report.items_already_always += 1;
                continue;
            }
            let mut updated = live.item.clone();
            updated.bindings.push(Binding::Always);
            match store::revise(store, session_id, lineage_id, actor, &live.item, &updated) {
                Ok(_) => report.items_bound += 1,
                Err(WriteError::Refused(refusal)) => {
                    report.hard_errors.push(format!("{}: refused: {refusal}", live.id))
                }
                Err(other) => report.hard_errors.push(format!("{}: {other}", live.id)),
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Kind, Severity};
    use model::store as model_store;

    // Every fixture below is invented for this test module: none of it is
    // drawn from any real store.

    fn rule_with_source(id: &str, source: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("a synthetic fixture rule for {id}"),
            // A migrated Rule always carries at least one real binding
            // already (the gate refuses one with none at all) - Always is
            // exactly what this run is meant to ADD, so the fixture starts
            // with a plain Moment binding instead, like a real migrated item
            // would.
            bindings: vec![Binding::Moment(intent::Action::Configure)],
            severity: Some(Severity::HouseStyle),
            project: None,
            tags: vec![format!("source:{source}")],
            expires: None,
            key: None,
            falsifier: Some("this synthetic fixture rule turns out to be wrong".to_string()),
            check: None,
        }
    }

    fn report_with_source(id: &str, source: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: format!("a synthetic fixture report for {id}"),
            bindings: vec![],
            severity: None,
            project: None,
            tags: vec![format!("source:{source}")],
            expires: Some("2027-01-01".to_string()),
            key: None,
            falsifier: None,
            check: None,
        }
    }

    #[test]
    fn a_matched_rule_receives_always_through_the_real_write_gate() {
        let mut db = EventStore::in_memory().unwrap();
        model_store::declare(&mut db, "s", "l", "a", &rule_with_source("r1", "old-1")).unwrap();

        let report = apply_pins(&mut db, "s", "l", "a", &["old-1".to_string()]);
        assert_eq!(report.pins_total, 1);
        assert_eq!(report.pins_linked, 1);
        assert!(report.unmatched_pins.is_empty());
        assert_eq!(report.items_bound, 1);
        assert!(report.hard_errors.is_empty());

        let after = model_store::show(&db, "r1").unwrap();
        assert!(after.bindings.contains(&Binding::Always), "the item must carry Always after the run");
    }

    #[test]
    fn a_pin_that_cannot_be_linked_is_counted_and_never_guessed() {
        // Named after the exact defect the brief forbids: a pin with no
        // matching source tag anywhere in the store must be reported as
        // unmatched, never silently dropped and never fuzzily linked to an
        // unrelated item.
        let mut db = EventStore::in_memory().unwrap();
        model_store::declare(&mut db, "s", "l", "a", &rule_with_source("r1", "old-1")).unwrap();

        let report = apply_pins(&mut db, "s", "l", "a", &["old-1".to_string(), "old-ghost".to_string()]);
        assert_eq!(report.pins_total, 2);
        assert_eq!(report.pins_linked, 1);
        assert_eq!(report.unmatched_pins, vec!["old-ghost".to_string()]);
        assert_eq!(report.items_bound, 1, "the real pin must still be applied even though the other one misses");
    }

    #[test]
    fn a_matched_report_is_skipped_not_offered_to_the_gate() {
        // The archive copy the migration writes alongside a typed item shares
        // the same source tag but can never carry a binding at all - it must
        // be counted separately, never as a hard error and never silently
        // folded into "linked" as if it had been bound.
        let mut db = EventStore::in_memory().unwrap();
        model_store::declare(&mut db, "s", "l", "a", &report_with_source("rep1", "old-2")).unwrap();

        let report = apply_pins(&mut db, "s", "l", "a", &["old-2".to_string()]);
        assert_eq!(report.pins_linked, 1, "the pin DID find a matching item, even though it is archive");
        assert_eq!(report.items_bound, 0);
        assert_eq!(report.items_not_fireable, 1);
        assert!(report.hard_errors.is_empty(), "skipping an archive match is not an error");

        let after = model_store::show(&db, "rep1").unwrap();
        assert!(after.bindings.is_empty(), "a Report must never gain a binding");
    }

    #[test]
    fn one_pin_can_bind_several_items_sharing_its_source_tag() {
        let mut db = EventStore::in_memory().unwrap();
        model_store::declare(&mut db, "s", "l", "a", &rule_with_source("r1", "old-3")).unwrap();
        model_store::declare(&mut db, "s", "l", "a", &rule_with_source("r2", "old-3")).unwrap();
        model_store::declare(&mut db, "s", "l", "a", &report_with_source("rep1", "old-3")).unwrap();

        let report = apply_pins(&mut db, "s", "l", "a", &["old-3".to_string()]);
        assert_eq!(report.pins_linked, 1);
        assert_eq!(report.items_bound, 2, "both rule items derived from the same old fact must be bound");
        assert_eq!(report.items_not_fireable, 1);
    }

    #[test]
    fn a_rerun_is_idempotent_and_never_double_counts() {
        let mut db = EventStore::in_memory().unwrap();
        model_store::declare(&mut db, "s", "l", "a", &rule_with_source("r1", "old-1")).unwrap();

        let first = apply_pins(&mut db, "s", "l", "a", &["old-1".to_string()]);
        assert_eq!(first.items_bound, 1);

        let second = apply_pins(&mut db, "s", "l", "a", &["old-1".to_string()]);
        assert_eq!(second.pins_linked, 1);
        assert_eq!(second.items_bound, 0, "a rerun must not re-apply Always to an item that already has it");
        assert_eq!(second.items_already_always, 1);

        let after = model_store::show(&db, "r1").unwrap();
        let always_count = after.bindings.iter().filter(|b| matches!(b, Binding::Always)).count();
        assert_eq!(always_count, 1, "Always must never be duplicated onto the same item's bindings");
    }

    #[test]
    fn every_pin_is_accounted_for_exactly_once() {
        let mut db = EventStore::in_memory().unwrap();
        model_store::declare(&mut db, "s", "l", "a", &rule_with_source("r1", "old-1")).unwrap();

        let pins = vec!["old-1".to_string(), "old-2".to_string(), "old-3".to_string()];
        let report = apply_pins(&mut db, "s", "l", "a", &pins);
        assert_eq!(report.pins_linked + report.unmatched_pins.len(), report.pins_total);
    }
}
