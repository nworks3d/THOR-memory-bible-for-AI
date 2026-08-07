//! One-time migration: turn a JSONL export of items into a filled store, one
//! `model::store::declare` call per line, never around it. CONTRACT.md's R1
//! ("a declaration that cannot be honoured is refused, not stored") and R5
//! ("write and declare is never silent") both apply here exactly as they do
//! to any other writer - a migration is not a special door into the store.
//!
//! `model::store::declare` has no duplicate check of its own (unlike
//! `EventStore::append_created_unique`): it will happily append a second
//! `fact_created` for an id that already has one, which would fork that
//! entity's head-set. This module is what keeps a rerun idempotent instead -
//! `already_migrated` skips any id that already has at least one event
//! before ever offering it to the gate again.

use model::item::Item;
use model::store::{self, WriteError};
use model::Refusal;
use std::collections::BTreeMap;
use thor_core::event_store::EventStore;

/// Counts for one migration run over one JSONL text. On a FRESH store
/// `already_present` is always 0, so `items_in == written + refused_total() +
/// malformed` there - the invariant a caller should check after every run.
/// `already_present` only ever grows on a rerun against a store that already
/// holds some of these items (see `already_migrated`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub items_in: usize,
    pub written: usize,
    pub already_present: usize,
    pub malformed: usize,
    /// Refusal ground label (see `classify_refusal`) to how many items were
    /// refused for that reason. A `BTreeMap` so two runs print their ground
    /// breakdown in the same order, never hashmap order.
    pub refused: BTreeMap<String, usize>,
    /// A `WriteError` other than a plain gate refusal (serialisation or the
    /// underlying log append itself failing). Never expected in practice;
    /// carried as text so a caller can report it loudly rather than folding
    /// it into "refused" as if the gate had made a decision about it.
    pub hard_errors: Vec<String>,
}

impl MigrationReport {
    pub fn refused_total(&self) -> usize {
        self.refused.values().sum()
    }

    /// The bookkeeping identity a fresh-store run must satisfy: every line in
    /// equals every line accounted for. Exposed so both the CLI and the
    /// tests below check it the same way.
    pub fn accounted_for(&self) -> usize {
        self.written + self.refused_total() + self.malformed + self.already_present
    }
}

/// Classify a refusal's `problem` text into a stable, private-data-free
/// ground label. Every arm matches on the FIXED template wording
/// `model::gate::declare`/`check_target_binding` write (see model/src/gate.rs),
/// never on the value interpolated into it - a target refusal's `problem`
/// embeds the actual (possibly private) path/host/route value, and that value
/// must never end up in a count label, a log line, or this migration's report.
fn classify_refusal(refusal: &Refusal) -> &'static str {
    let p = refusal.problem.as_str();
    if p.contains("Rule with no binding") {
        "rule_with_no_binding"
    } else if p.contains("carries an expires date") {
        "expires_on_a_kind_that_never_expires"
    } else if p.contains("archive material may never reach a gate") {
        "archive_kind_with_a_binding"
    } else if p.contains("character limit for a") {
        "text_over_char_limit"
    } else if p.contains("Lookup has no key") {
        "lookup_without_key"
    } else if p.contains("has no falsifier") {
        "rule_or_orientation_without_falsifier"
    } else if p.contains("empty or whitespace-only value") {
        "target_empty_value"
    } else if p.contains("glob wildcard") {
        "target_glob_wildcard"
    } else if p.contains("current or parent directory") {
        "target_dot_or_dotdot"
    } else if p.contains("bare role name") {
        "target_bare_role_name"
    } else if p.contains("bare host name") {
        "target_bare_host_name"
    } else if p.contains("root path") {
        "target_root_route"
    } else {
        // Every ground `model::gate::declare` can return is matched above
        // (see its own doc comment: "a refusal reason with no test does not
        // exist"). Landing here means the gate grew a new refusal this
        // module does not yet know about - reported as its own bucket rather
        // than silently merged into an existing one.
        "unclassified_refusal"
    }
}

/// Whether `id` already has at least one event in the store - the
/// idempotency guard a rerun relies on. Deliberately broader than "has a
/// live head": even a diverged or retracted entity counts as "already
/// touched", because writing another `fact_created` on top of it would still
/// be the double-write this function exists to prevent.
fn already_migrated(store: &EventStore, id: &str) -> bool {
    store.get_events_by_entity(id).map(|events| !events.is_empty()).unwrap_or(false)
}

/// Migrate one non-blank JSONL line. Never calls the gate for an id that is
/// already present (see `already_migrated`); otherwise this is a plain
/// `model::store::declare` call, so a refusal here is the gate's own
/// decision, not something this function ever second-guesses.
fn migrate_one(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    raw_line: &str,
    report: &mut MigrationReport,
) {
    let item: Item = match serde_json::from_str(raw_line) {
        Ok(item) => item,
        Err(_) => {
            report.malformed += 1;
            return;
        }
    };
    if already_migrated(store, &item.id) {
        report.already_present += 1;
        return;
    }
    match store::declare(store, session_id, lineage_id, actor, &item) {
        Ok(_) => report.written += 1,
        Err(WriteError::Refused(refusal)) => {
            *report.refused.entry(classify_refusal(&refusal).to_string()).or_insert(0) += 1;
        }
        Err(other) => report.hard_errors.push(other.to_string()),
    }
}

/// Run the whole migration: every non-blank line of `jsonl_text`, in file
/// order, through `migrate_one`. The store is the only mutable state, so
/// calling this twice against two independent fresh stores must produce
/// identical reports, and calling it twice against the SAME store must leave
/// the second call's `written` at 0 (see this module's own tests).
pub fn run(
    store: &mut EventStore,
    jsonl_text: &str,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
) -> MigrationReport {
    let mut report = MigrationReport::default();
    for line in jsonl_text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        report.items_in += 1;
        migrate_one(store, session_id, lineage_id, actor, line, &mut report);
    }
    report
}

/// After a migration, how many distinct `source:<id>` tags are represented
/// among the store's LIVE items - read back from the store itself (via
/// `crate::live::live_items`, the one reader every other surface already
/// uses), never from this run's own bookkeeping: bookkeeping cannot see what
/// an earlier, separate process invocation already wrote, and a refused item
/// was never written, so it can never count as represented.
pub fn source_facts_represented(store: &EventStore) -> usize {
    let mut sources = std::collections::HashSet::new();
    for live in crate::live::live_items(store) {
        for tag in &live.item.tags {
            if let Some(source) = tag.strip_prefix("source:") {
                sources.insert(source.to_string());
            }
        }
    }
    sources.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Kind, TargetKind};

    // Every fixture below is invented for this test module: none of it is
    // drawn from any real store.

    fn json_line(item: &Item) -> String {
        serde_json::to_string(item).unwrap()
    }

    fn good_rule(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "a synthetic fixture rule, bound always".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec!["source:fixture-a".to_string()],
            expires: None,
            key: None,
            falsifier: Some("this synthetic fixture rule turns out to be wrong".to_string()),
            check: None,
        }
    }

    fn bad_rule_no_binding(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "a synthetic fixture rule with no binding at all".to_string(),
            bindings: vec![],
            severity: None,
            project: None,
            tags: vec!["source:fixture-b".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    fn good_report(id: &str, source: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: "a synthetic fixture report, archive only".to_string(),
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

    // --------------------------------------------------------- gate is never bypassed

    #[test]
    fn a_gate_refused_item_is_counted_refused_and_never_written() {
        let mut store = EventStore::in_memory().unwrap();
        let text = json_line(&bad_rule_no_binding("mig-refuse-1"));
        let report = run(&mut store, &text, "s", "l", "migration-test");
        assert_eq!(report.written, 0, "a refused item must never count as written");
        assert_eq!(report.refused_total(), 1);
        assert_eq!(report.refused.get("rule_with_no_binding"), Some(&1));
    }

    #[test]
    fn a_refused_item_never_appears_in_the_store() {
        // Named after the exact defect R1 exists to prevent: the migration
        // must not paper over a gate refusal by writing the item anyway.
        let mut store = EventStore::in_memory().unwrap();
        let id = "mig-refuse-2";
        let text = json_line(&bad_rule_no_binding(id));
        run(&mut store, &text, "s", "l", "migration-test");
        let events = store.get_events_by_entity(id).unwrap();
        assert!(events.is_empty(), "a refused item's id must have zero events in the store");
    }

    #[test]
    fn a_good_item_is_written_and_counted_written_only() {
        let mut store = EventStore::in_memory().unwrap();
        let text = json_line(&good_rule("mig-ok-1"));
        let report = run(&mut store, &text, "s", "l", "migration-test");
        assert_eq!(report.written, 1);
        assert_eq!(report.refused_total(), 0);
        assert_eq!(store.get_events_by_entity("mig-ok-1").unwrap().len(), 1);
    }

    // --------------------------------------------------------- the three counts sum

    #[test]
    fn on_a_fresh_store_items_in_equals_written_plus_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let text = format!("{}\n{}\n{}", json_line(&good_rule("mig-sum-1")), json_line(&bad_rule_no_binding("mig-sum-2")), json_line(&good_report("mig-sum-3", "fixture-c")));
        let report = run(&mut store, &text, "s", "l", "migration-test");
        assert_eq!(report.items_in, 3);
        assert_eq!(report.already_present, 0, "nothing pre-exists on a fresh store");
        assert_eq!(report.malformed, 0);
        assert_eq!(report.items_in, report.written + report.refused_total());
        assert_eq!(report.accounted_for(), report.items_in);
    }

    #[test]
    fn a_malformed_line_is_counted_and_never_panics() {
        let mut store = EventStore::in_memory().unwrap();
        let text = "not valid json at all";
        let report = run(&mut store, text, "s", "l", "migration-test");
        assert_eq!(report.items_in, 1);
        assert_eq!(report.malformed, 1);
        assert_eq!(report.accounted_for(), report.items_in);
    }

    #[test]
    fn blank_lines_are_never_counted_as_items() {
        let mut store = EventStore::in_memory().unwrap();
        let text = format!("\n\n{}\n\n", json_line(&good_rule("mig-blank-1")));
        let report = run(&mut store, &text, "s", "l", "migration-test");
        assert_eq!(report.items_in, 1, "blank lines must not inflate items_in");
    }

    // --------------------------------------------------------- idempotency: no double write

    #[test]
    fn a_second_run_on_the_same_store_writes_nothing_new() {
        // Named after the defect it prevents: model::store::declare has no
        // duplicate check of its own, so without already_migrated a rerun
        // would append a second fact_created and fork the entity's head-set.
        let mut store = EventStore::in_memory().unwrap();
        let text = json_line(&good_rule("mig-rerun-1"));

        let first = run(&mut store, &text, "s", "l", "migration-test");
        assert_eq!(first.written, 1);
        let events_after_first = store.get_events_by_entity("mig-rerun-1").unwrap().len();
        assert_eq!(events_after_first, 1);

        let second = run(&mut store, &text, "s", "l", "migration-test");
        assert_eq!(second.written, 0, "a rerun over the same store must write nothing new");
        assert_eq!(second.already_present, 1);
        let events_after_second = store.get_events_by_entity("mig-rerun-1").unwrap().len();
        assert_eq!(events_after_second, 1, "the entity must not gain a second event");
    }

    #[test]
    fn a_second_run_still_refuses_a_bad_item_the_same_way_every_time() {
        // A refused item is never written, so it is never "already present"
        // either - a rerun must offer it to the gate again and get the same
        // answer, not silently start treating it as done.
        let mut store = EventStore::in_memory().unwrap();
        let text = json_line(&bad_rule_no_binding("mig-rerun-refuse-1"));

        let first = run(&mut store, &text, "s", "l", "migration-test");
        let second = run(&mut store, &text, "s", "l", "migration-test");

        assert_eq!(first.refused_total(), 1);
        assert_eq!(second.refused_total(), 1);
        assert_eq!(second.already_present, 0);
        assert!(store.get_events_by_entity("mig-rerun-refuse-1").unwrap().is_empty());
    }

    // --------------------------------------------------------- two fresh runs agree

    #[test]
    fn two_fresh_stores_given_the_same_input_produce_identical_reports() {
        let text = format!(
            "{}\n{}\n{}",
            json_line(&good_rule("mig-fresh-a-1")),
            json_line(&bad_rule_no_binding("mig-fresh-a-2")),
            json_line(&good_report("mig-fresh-a-3", "fixture-d"))
        );

        let mut store_a = EventStore::in_memory().unwrap();
        let report_a = run(&mut store_a, &text, "s", "l", "migration-test");

        let mut store_b = EventStore::in_memory().unwrap();
        let report_b = run(&mut store_b, &text, "s", "l", "migration-test");

        assert_eq!(report_a, report_b);
    }

    // --------------------------------------------------------- source-fact representation

    #[test]
    fn source_facts_represented_counts_only_written_items() {
        let mut store = EventStore::in_memory().unwrap();
        // Two items share one source tag: one accepted, one refused. The
        // source must still count as represented, because at least one
        // derived item for it landed in the store.
        let mut bad = bad_rule_no_binding("mig-src-2");
        bad.tags = vec!["source:fixture-a".to_string()]; // same source as good_rule above
        let text = format!("{}\n{}", json_line(&good_rule("mig-src-1")), json_line(&bad));
        run(&mut store, &text, "s", "l", "migration-test");
        assert_eq!(source_facts_represented(&store), 1);
    }

    #[test]
    fn a_source_whose_only_item_was_refused_is_not_represented() {
        let mut store = EventStore::in_memory().unwrap();
        let text = json_line(&bad_rule_no_binding("mig-src-lonely-1"));
        run(&mut store, &text, "s", "l", "migration-test");
        assert_eq!(source_facts_represented(&store), 0, "a refused item was never written, so its source is not represented");
    }

    // --------------------------------------------------------- gate coverage sanity

    #[test]
    fn a_target_binding_ground_is_classified_and_the_item_is_not_written() {
        let mut store = EventStore::in_memory().unwrap();
        let mut bad_target = good_rule("mig-target-1");
        bad_target.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "*.rs".to_string() }];
        let report = run(&mut store, &json_line(&bad_target), "s", "l", "migration-test");
        assert_eq!(report.refused.get("target_glob_wildcard"), Some(&1));
        assert!(store.get_events_by_entity("mig-target-1").unwrap().is_empty());
    }

    #[test]
    fn a_missing_falsifier_ground_is_classified_and_the_item_is_not_written() {
        let mut store = EventStore::in_memory().unwrap();
        let mut no_falsifier = good_rule("mig-falsifier-1");
        no_falsifier.falsifier = None;
        let report = run(&mut store, &json_line(&no_falsifier), "s", "l", "migration-test");
        assert_eq!(report.refused.get("rule_or_orientation_without_falsifier"), Some(&1));
        assert!(store.get_events_by_entity("mig-falsifier-1").unwrap().is_empty());
    }
}
