//! The `audit` query: every live item whose `Kind::can_fire()` is true
//! (`Rule`/`Orientation` today - see `model::item::Kind::can_fire`, the ONE
//! definition of which kinds can ever reach a gate), with how often it fired
//! and when it last fired. "Declared but never delivered" is exactly the rows
//! with `times_served == 0` - a query over the log, not a separate ledger.

use crate::live::live_items;
use crate::serving::{serving_stats, ServingStats};
use model::item::Item;
use thor_core::event_store::EventStore;

pub struct AuditRow {
    pub id: String,
    pub item: Item,
    pub stats: ServingStats,
}

/// Every live item whose kind can fire, each paired with its serving stats
/// (zeroed when it was never served). Sorted by id for a stable, diffable
/// report.
pub fn audit_rows(store: &EventStore) -> Vec<AuditRow> {
    let stats = serving_stats(store);
    let mut rows: Vec<AuditRow> = live_items(store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire())
        .map(|li| AuditRow {
            stats: stats.get(&li.id).cloned().unwrap_or_default(),
            id: li.id,
            item: li.item,
        })
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// Just the ids never delivered - "wel gedeclareerd, nooit geleverd" as a
/// direct query rather than something read off the full report by eye.
pub fn never_delivered(store: &EventStore) -> Vec<String> {
    audit_rows(store)
        .into_iter()
        .filter(|r| r.stats.times_served == 0)
        .map(|r| r.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind};
    use model::store;

    fn item(id: &str, kind: Kind) -> Item {
        Item {
            id: id.to_string(),
            kind,
            text: "do the thing".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this item turns out to be wrong for this synthetic fixture".to_string()),
            check: None,
        }
    }

    #[test]
    fn a_declared_item_never_served_is_a_query_result() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("never", Kind::Rule)).unwrap();
        assert_eq!(never_delivered(&db), vec!["never".to_string()]);
    }

    #[test]
    fn a_served_item_is_not_in_never_delivered() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("fired", Kind::Rule)).unwrap();
        crate::deliver::record_delivery(&mut db, "s", "l", "serve", "2026-08-02T00:00:00Z", &["fired".to_string()]);
        assert!(never_delivered(&db).is_empty());
    }

    #[test]
    fn a_report_or_lookup_never_appears_in_the_audit_rows() {
        // audit is scoped to what rank::select could ever have served - a
        // Report/Lookup being "never delivered" is not a defect to report,
        // it is the design (see rank::eligible / Kind::can_fire).
        let mut db = EventStore::in_memory().unwrap();
        let mut report = item("r1", Kind::Report);
        report.bindings = vec![];
        store::declare(&mut db, "s", "l", "a", &report).unwrap();
        let mut lookup = item("l1", Kind::Lookup);
        lookup.bindings = vec![];
        lookup.key = Some("k".to_string());
        store::declare(&mut db, "s", "l", "a", &lookup).unwrap();
        assert!(audit_rows(&db).is_empty());
    }

    #[test]
    fn a_chunk_never_appears_in_the_audit_rows() {
        // Same guarantee, for the new archive kind: a Chunk is never "never
        // delivered" in the sense audit reports - it was never eligible to
        // be delivered in the first place, so it never becomes a row at all.
        let mut db = EventStore::in_memory().unwrap();
        let mut chunk = item("c1", Kind::Chunk);
        chunk.bindings = vec![];
        store::declare(&mut db, "s", "l", "a", &chunk).unwrap();
        assert!(audit_rows(&db).is_empty());
    }

    #[test]
    fn audit_rows_are_ordered_by_id() {
        let mut db = EventStore::in_memory().unwrap();
        // Distinct text per item (the near-duplicate gate refuses a second
        // live item of the same kind with the same or near-same text) and
        // deliberately NOT in id order: declared z, a, m, and each item's
        // own text sorts z, a, m too (alphabetically), so a row order that
        // matched TEXT order would read "z, a, m" here, not "a, m, z" - only
        // a sort keyed on id produces the order this test asserts.
        let texts = [("z", "back up the database nightly"), ("a", "rotate the api keys quarterly"), ("m", "run linting before every commit")];
        for (id, text) in texts {
            let mut it = item(id, Kind::Rule);
            it.text = text.to_string();
            store::declare(&mut db, "s", "l", "a", &it).unwrap();
        }
        let rows = audit_rows(&db);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "m", "z"]);
    }
}
