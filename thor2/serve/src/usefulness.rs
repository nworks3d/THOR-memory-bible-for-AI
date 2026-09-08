//! The query side of "was this item ever marked useful" - a plain fold over
//! `ItemMarkedUseful` events in the log, the same shape `serving::serving_stats`
//! already uses for `ItemServed`. Existence only: decay
//! (`crate::decay::DecayContext`) only ever asks "ever, yes or no", never "how
//! many times" - a mark is a permanent, one-way cancellation of decay for that
//! item, not a score.

use std::collections::{HashMap, HashSet};
use thor_core::event_store::{EventKind, EventStore};

/// Every entity id with at least one `ItemMarkedUseful` event in the log.
/// Fails open like every other reader on this boundary (see
/// `serving::serving_stats`): a broken log yields an empty set rather than an
/// error, since this backs a serve-path decision, not a write path.
///
/// Reads `EventStore::event_kinds` (kind + entity_id only), not
/// `get_all_events`: this fold never looks at a body, a hash, or an actor, so
/// there is no reason to pay for a fully materialized `Event` - body and
/// body_ch included - on every row of the whole log to get there.
pub fn ever_marked_useful(store: &EventStore) -> HashSet<String> {
    let Ok(events) = store.event_kinds() else { return HashSet::new() };
    events
        .into_iter()
        .filter(|(kind, _)| *kind == EventKind::ItemMarkedUseful)
        .map(|(_, entity_id)| entity_id)
        .collect()
}

/// How many times each entity has been called noise, folded from
/// `ItemMarkedNoise` events. A COUNT, not existence: one stray judgement must
/// not retire a rule, a repeated one should - see `crate::decay` for the
/// threshold and where it comes from.
///
/// Fails open like every other reader on this boundary: a broken log yields
/// an empty map, so nothing is ever retired because the log could not be
/// read. Reads `EventStore::event_kinds`, same reasoning as
/// `ever_marked_useful` above.
pub fn noise_counts(store: &EventStore) -> HashMap<String, usize> {
    let Ok(events) = store.event_kinds() else { return HashMap::new() };
    let mut out: HashMap<String, usize> = HashMap::new();
    for (kind, entity_id) in events {
        if kind == EventKind::ItemMarkedNoise {
            *out.entry(entity_id).or_default() += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_never_marked_is_absent_from_the_set() {
        let store = EventStore::in_memory().unwrap();
        assert!(ever_marked_useful(&store).is_empty());
    }

    #[test]
    fn a_marked_item_is_present_in_the_set() {
        let mut store = EventStore::in_memory().unwrap();
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "x1").unwrap();
        let marked = ever_marked_useful(&store);
        assert!(marked.contains("x1"));
    }

    #[test]
    fn marking_the_same_item_twice_still_yields_one_membership() {
        let mut store = EventStore::in_memory().unwrap();
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "x1").unwrap();
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-03T00:00:00Z", "x1").unwrap();
        let marked = ever_marked_useful(&store);
        assert_eq!(marked.len(), 1);
        assert!(marked.contains("x1"));
    }

    #[test]
    fn marking_one_item_never_marks_another() {
        let mut store = EventStore::in_memory().unwrap();
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "x1").unwrap();
        let marked = ever_marked_useful(&store);
        assert!(!marked.contains("x2"));
    }
}

/// How many times an item must have fired SINCE its last verdict before
/// `bin/serve.rs`'s Stop-hook `judgement_debt` holds the turn for another -
/// moved here (2026-09-08, unchanged in value) so `judgement_debt_counts`
/// below can share the exact same number `doctor` reports against instead of
/// a second copy silently drifting from it. High on purpose: this is about
/// the handful of rules that are in front of a reader constantly, not about
/// everything served.
///
/// "SINCE its last verdict", not "with no verdict ever", and the difference
/// is the whole point. A lifetime judged-set meant one answer settled an
/// item for good, which is the same defect `decay::is_stale` carried until
/// 2026-08-08: a verdict given once, about an item that has since drifted,
/// outranked a reader who would answer differently today. It also made the
/// cheap answer the damaging one, because the debt asks first about the
/// items that fire most - exactly the ones whose bindings are worth
/// revisiting. Forty more firings is a long way to earn a second question.
pub const JUDGEMENT_DEBT_AFTER: usize = 40;

/// How many times each item has fired since its own last verdict - a mark of
/// usefulness OR noise resets its running count back to zero, same as a
/// verdict resets `judgement_debt`'s own question about that item. Extracted
/// (2026-09-08) from `bin/serve.rs`'s `judgement_debt`, which used to fold
/// this inline: that left `ops::health` with no honest way to report the
/// same backlog the Stop hook actually acts on short of hand-rolling a
/// second copy of this exact fold and risking it drift from the real one.
/// One fold, two callers now (`judgement_debt` and `judgement_debt_counts`
/// below).
pub fn served_since_last_verdict(store: &EventStore) -> HashMap<String, usize> {
    let Ok(events) = store.event_kinds() else { return HashMap::new() };
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (kind, id) in events {
        match kind {
            EventKind::ItemServed => *counts.entry(id).or_default() += 1,
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise => {
                counts.insert(id, 0);
            }
            _ => {}
        }
    }
    counts
}

/// (store-wide count of items in judgement debt, how many of those this
/// checkout's own project accounts for) - backs a `doctor` line naming both
/// numbers.
///
/// WHY THIS EXISTS SEPARATELY FROM `bin/serve.rs`'s OWN `judgement_debt`.
/// That function additionally scopes to one live session's own
/// `EventStore::served_ids_in_session` - "did the reader being asked ever
/// see this fire" - which only has a truthful answer inside a real session.
/// `doctor` runs cold, outside any session, so it cannot ask that question
/// at all; the honest thing it CAN still report is how large the backlog is
/// store-wide, and how much of it this checkout's own project would ever be
/// asked about (`project::applies_to`: that project by name, or no project
/// at all - global). Scoping doctor's own count to "this session" would not
/// make it more honest, it would make it silent - there is no session here
/// to scope to.
///
/// PINNED ITEMS ARE INCLUDED, unlike the older `ops::health::unjudged_line`:
/// `judgement_debt` itself stopped excluding them (see that function's own
/// "A PINNED ITEM GETS ONE VERDICT, NOT A STANDING EXEMPTION" doc comment) -
/// a doctor line still built on the old exclusion would silently disagree
/// with the mechanism it exists to report on, the exact drift this function
/// exists to close by sharing `served_since_last_verdict` instead of
/// re-deriving its own count.
pub fn judgement_debt_counts(store: &EventStore, checkout_project: Option<&str>) -> (usize, usize) {
    let served = served_since_last_verdict(store);
    let live: HashMap<String, Option<String>> =
        crate::live::live_items(store).into_iter().map(|li| (li.id, li.item.project)).collect();
    let owed: Vec<&String> =
        served.iter().filter(|(id, n)| **n >= JUDGEMENT_DEBT_AFTER && live.contains_key(*id)).map(|(id, _)| id).collect();
    let total = owed.len();
    let in_project =
        owed.iter().filter(|id| crate::project::applies_to(live[id.as_str()].as_deref(), checkout_project)).count();
    (total, in_project)
}

#[cfg(test)]
mod judgement_debt_counting_tests {
    use super::*;
    use model::item::{Binding, Item, Kind};

    fn declare(store: &mut EventStore, id: &str, project: Option<&str>) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("something worth knowing about {id}"),
            bindings: vec![Binding::Moment(intent::Action::Commit)],
            severity: None,
            project: project.map(str::to_string),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        };
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
    }

    fn serve_n(store: &mut EventStore, id: &str, times: usize) {
        for _ in 0..times {
            crate::deliver::record_delivery(store, "s", "s", "t", "2026-09-08T00:00:00Z", &[id.to_string()]);
        }
    }

    #[test]
    fn an_empty_store_owes_nothing_anywhere() {
        let store = EventStore::in_memory().unwrap();
        assert_eq!(judgement_debt_counts(&store, None), (0, 0));
        assert_eq!(judgement_debt_counts(&store, Some("thor")), (0, 0));
    }

    #[test]
    fn firing_under_the_threshold_owes_nothing() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "rarely", None);
        serve_n(&mut store, "rarely", JUDGEMENT_DEBT_AFTER - 1);
        assert_eq!(judgement_debt_counts(&store, None), (0, 0));
    }

    #[test]
    fn a_global_item_counts_toward_every_project() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "global-owed", None);
        serve_n(&mut store, "global-owed", JUDGEMENT_DEBT_AFTER);
        assert_eq!(judgement_debt_counts(&store, Some("thor")), (1, 1));
        assert_eq!(judgement_debt_counts(&store, Some("acme")), (1, 1));
        assert_eq!(judgement_debt_counts(&store, None), (1, 1));
    }

    #[test]
    fn a_project_scoped_item_counts_only_for_its_own_checkout() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "acme-owed", Some("acme"));
        serve_n(&mut store, "acme-owed", JUDGEMENT_DEBT_AFTER);
        assert_eq!(
            judgement_debt_counts(&store, Some("thor")),
            (1, 0),
            "store-wide sees it, a different checkout's project does not"
        );
        assert_eq!(judgement_debt_counts(&store, Some("acme")), (1, 1));
    }

    #[test]
    fn a_retracted_item_never_inflates_either_count() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "gone", None);
        serve_n(&mut store, "gone", JUDGEMENT_DEBT_AFTER);
        assert_eq!(judgement_debt_counts(&store, None), (1, 1), "fixture sanity: owed while live");
        model::store::retract(&mut store, "t", "t", "t", "gone", "no longer needed").unwrap();
        assert_eq!(judgement_debt_counts(&store, None), (0, 0), "a dead item is not a debt");
    }

    #[test]
    fn a_verdict_pays_down_the_store_wide_count() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "settled", None);
        serve_n(&mut store, "settled", JUDGEMENT_DEBT_AFTER);
        assert_eq!(judgement_debt_counts(&store, None).0, 1);
        crate::mark::record_useful(&mut store, "s", "s", "t", "2026-09-08T00:00:00Z", "settled").unwrap();
        assert_eq!(judgement_debt_counts(&store, None).0, 0);
    }
}

/// Noise judgements recorded SINCE each item's most recent mark of
/// usefulness, rather than over its whole life.
///
/// WHY THE "EVER" FORM WAS WRONG. `ever_marked_useful` made a single useful
/// verdict permanent: no amount of later evidence could ever retire that
/// item again, and there is no operation anywhere in this system that undoes
/// one. Two independent reviews found the same consequence on 2026-08-08:
/// the judgement debt asks first about the items served most often, which
/// are exactly the ones already winning every place, so the only maintenance
/// loop in the system was quietly making the dominant items immortal - and
/// the cheap answer (plain `mark`, one call) was the one that did it, while
/// the honest answer needs two.
///
/// The rule now is simply that the LATEST verdict counts. A useful mark
/// still protects, and one stray noise still decides nothing, because
/// retiring still needs two. What it no longer does is outrank a reader who
/// changed their mind later, with better information.
pub fn noise_since_last_useful(store: &EventStore) -> HashMap<String, usize> {
    let Ok(events) = store.event_kinds() else { return HashMap::new() };
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (kind, id) in events {
        match kind {
            EventKind::ItemMarkedUseful => {
                counts.insert(id, 0);
            }
            EventKind::ItemMarkedNoise => *counts.entry(id).or_default() += 1,
            _ => {}
        }
    }
    counts
}
