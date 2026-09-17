//! The query side of "was this item ever marked useful" - a plain fold over
//! `ItemMarkedUseful` events in the log, the same shape `serving::serving_stats`
//! already uses for `ItemServed`. Existence only: decay
//! (`crate::decay::DecayContext`) only ever asks "ever, yes or no", never "how
//! many times" - a mark is a permanent, one-way cancellation of decay for that
//! item, not a score.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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

/// Whether `bindings` carries the `Always` binding - THE one definition of
/// "pinned" every judgement-debt surface now shares, so the four places that
/// ask "is this owed a verdict" (the Stop-time ask, doctor's two judgement-
/// debt lines, and the evaluation debt's own owed count) can never drift
/// apart about what "pinned" means. `owed_items` below routes three of the
/// four through this directly; `bin/serve.rs`'s own `judgement_debt` (the
/// Stop-time ask) calls it too, since it cannot call `owed_items` itself -
/// see that function's own doc comment for why its session-scoped filters
/// keep it a separate fold - but must still exclude the identical set.
///
/// The identical literal test already lives, unshared, in
/// `serve::decay::DecayContext::is_stale`, `ops::health::unjudged_line`/
/// `pinned_line`, and the `pin`/`unpin` tools (`mcp::lib`); none of those are
/// part of the judgement debt this closes, so none of them are routed
/// through this function today - left as they are, on purpose, rather than
/// widening this change to a repository-wide rename.
pub fn is_pinned(bindings: &[model::item::Binding]) -> bool {
    bindings.iter().any(|b| matches!(b, model::item::Binding::Always))
}

/// One live, non-pinned item over the judgement-debt threshold, before any
/// checkout scoping - the single fold `judgement_debt_counts` and
/// `judgement_debt_named` below both build on, so a store-wide count and a
/// checkout's own named list can never quietly disagree about what "owed"
/// means the way two independent folds could drift apart.
struct Owed {
    id: String,
    count: usize,
    project: Option<String>,
    kind: model::item::Kind,
    bindings: Vec<model::item::Binding>,
}

/// Every live, non-pinned item served `JUDGEMENT_DEBT_AFTER`+ times since its
/// own last verdict, unscoped by project. Extracted (2026-09-12) out of
/// `judgement_debt_counts`'s own body so `judgement_debt_named` can share the
/// exact same "owed" definition instead of re-deriving it and risking a
/// second copy that silently drifts from the first - the same reasoning
/// `served_since_last_verdict`'s own doc comment already gives for why it is
/// one fold with two callers rather than two folds.
///
/// PINNED (`Always`-bound) ITEMS EXCLUDED HERE, since 2026-09-12 - reversed
/// back from the brief window (2026-09-08 to today) where `judgement_debt`
/// counted them on purpose (see that function's own doc comment for the full
/// history of both defects). `serve::decay::DecayContext::is_stale` already
/// ignores a verdict on an `Always`-bound item outright ("an Always-bound
/// item is NEVER stale... it is what Always means"), so a verdict recorded
/// here would change nothing anywhere in the system: asking for one, and
/// spending a whole evaluation judging one, is pure waste. Measured the day
/// this reversed: the named list carried more than thirty Always-bound
/// items, two of them pinned by the owner on purpose, and an evaluation was
/// spent judging every one of them for nothing.
fn owed_items(store: &EventStore) -> Vec<Owed> {
    let served = served_since_last_verdict(store);
    let live = crate::live::live_items(store);
    let live_by_id: HashMap<&str, &crate::live::LiveItem> = live.iter().map(|li| (li.id.as_str(), li)).collect();
    served
        .into_iter()
        .filter(|(_, count)| *count >= JUDGEMENT_DEBT_AFTER)
        .filter_map(|(id, count)| {
            let li = live_by_id.get(id.as_str())?;
            Some(Owed { id, count, project: li.item.project.clone(), kind: li.item.kind, bindings: li.item.bindings.clone() })
        })
        .filter(|o| !is_pinned(&o.bindings))
        .collect()
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
/// PINNED ITEMS ARE EXCLUDED, the same as `ops::health::unjudged_line`
/// already excludes them - the two disagreeing, for the few days
/// `judgement_debt` (`bin/serve.rs`) counted them, was exactly the drift
/// `owed_items` now exists to close: this count and `judgement_debt_named`
/// below both read the exclusion from that one shared fold, so a doctor line
/// can never again silently disagree with the mechanism it reports on.
pub fn judgement_debt_counts(store: &EventStore, checkout_project: Option<&str>) -> (usize, usize) {
    let owed = owed_items(store);
    let total = owed.len();
    let in_project =
        owed.iter().filter(|o| crate::project::applies_to(o.project.as_deref(), checkout_project)).count();
    (total, in_project)
}

/// One item over the judgement-debt threshold, NAMED rather than merely
/// counted: its id, how many times it fired since its own last verdict, its
/// kind, and the bindings that say WHERE it fires - the place an evaluation
/// must judge "did it belong there" against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgementDebtItem {
    pub id: String,
    pub count: usize,
    pub kind: model::item::Kind,
    pub bindings: Vec<model::item::Binding>,
}

/// THE GAP THIS CLOSES. `judgement_debt_counts` above tells `doctor` how big
/// the backlog is, but never which items make it up - an end-of-session
/// evaluation meant to settle "did it belong where it fired" had only a
/// number to work from, never a list to walk, and the only other route was
/// reading the store by hand (see `ops::health::judgement_debt_line`'s own
/// doc comment for the rest of that history).
///
/// Every item over the threshold that applies to `checkout_project` - its own
/// project, or global - by the exact same `project::applies_to` rule
/// `judgement_debt_counts` already uses for its `in_project` half, so the two
/// can never disagree about which items are this checkout's business. AN ITEM
/// OWED ONLY TO ANOTHER PROJECT IS EXCLUDED HERE, not merely left uncounted:
/// `ops::health::judgement_debt_line` can only ever show one checkout's own
/// worklist, and naming another project's items would send this checkout's
/// owner looking for a place outside his own working copy.
///
/// SORTED BY COUNT DESCENDING, ties broken by id for a deterministic order
/// across runs, so the busiest item - the one a single verdict pays down the
/// most - is always named first.
pub fn judgement_debt_named(store: &EventStore, checkout_project: Option<&str>) -> Vec<JudgementDebtItem> {
    let mut named: Vec<JudgementDebtItem> = owed_items(store)
        .into_iter()
        .filter(|o| crate::project::applies_to(o.project.as_deref(), checkout_project))
        .map(|o| JudgementDebtItem { id: o.id, count: o.count, kind: o.kind, bindings: o.bindings })
        .collect();
    named.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.id.cmp(&b.id)));
    named
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

    // ------------------------------------------------- judgement_debt_named

    /// Exactly the items over the threshold that apply to this checkout (its
    /// own project, plus global), and nothing under the threshold - sorted by
    /// count descending so the busiest debt leads.
    #[test]
    fn named_list_contains_exactly_the_items_over_threshold_for_this_checkout_sorted_by_count_descending() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "rarely", None);
        serve_n(&mut store, "rarely", JUDGEMENT_DEBT_AFTER - 1);
        declare(&mut store, "quiet-debt", None);
        serve_n(&mut store, "quiet-debt", JUDGEMENT_DEBT_AFTER);
        declare(&mut store, "loud-debt", Some("thor"));
        serve_n(&mut store, "loud-debt", JUDGEMENT_DEBT_AFTER + 5);

        let named = judgement_debt_named(&store, Some("thor"));
        let ids: Vec<&str> = named.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["loud-debt", "quiet-debt"], "under-threshold item must be absent: {ids:?}");
        assert_eq!(named[0].count, JUDGEMENT_DEBT_AFTER + 5);
        assert_eq!(named[1].count, JUDGEMENT_DEBT_AFTER);
    }

    /// The same exclusion `a_project_scoped_item_counts_only_for_its_own_checkout`
    /// proves for the count - here proved for the named list: an item owed
    /// only to a DIFFERENT project must not be named for this checkout, even
    /// though it is still real store-wide debt.
    #[test]
    fn named_list_excludes_an_item_owed_only_to_another_project() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "acme-owed", Some("acme"));
        serve_n(&mut store, "acme-owed", JUDGEMENT_DEBT_AFTER);

        let for_thor = judgement_debt_named(&store, Some("thor"));
        assert!(for_thor.is_empty(), "a different checkout's project must not see it named: {for_thor:?}");

        let for_acme = judgement_debt_named(&store, Some("acme"));
        assert_eq!(for_acme.len(), 1);
        assert_eq!(for_acme[0].id, "acme-owed");
    }

    // ------------------------------------------------- pinned exclusion

    fn declare_pinned(store: &mut EventStore, id: &str, project: Option<&str>) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("something worth knowing about {id}"),
            bindings: vec![Binding::Always],
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

    /// THE DEFECT THIS PREVENTS. An Always-bound item's own verdicts are
    /// already inert - `decay::DecayContext::is_stale` never reads them for
    /// a pinned item ("an Always-bound item is NEVER stale... it is what
    /// Always means") - so asking for one, and spending a whole evaluation
    /// judging one, is pure waste. Measured 2026-09-12: an evaluation was
    /// spent judging more than thirty of these for nothing, two of them
    /// pinned by the owner on purpose.
    #[test]
    fn an_always_bound_item_served_past_the_threshold_is_never_owed() {
        let mut store = EventStore::in_memory().unwrap();
        declare_pinned(&mut store, "pinned-owed", None);
        serve_n(&mut store, "pinned-owed", JUDGEMENT_DEBT_AFTER);
        assert_eq!(judgement_debt_counts(&store, None), (0, 0));
    }

    /// The control: change only the binding, keep everything else about the
    /// fixture identical, and the same shape of item becomes owed - proving
    /// the exclusion above is about the binding, not some other accident of
    /// the fixture.
    #[test]
    fn the_identical_item_without_always_is_owed() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "not-pinned-owed", None);
        serve_n(&mut store, "not-pinned-owed", JUDGEMENT_DEBT_AFTER);
        assert_eq!(judgement_debt_counts(&store, None), (1, 1));
    }

    #[test]
    fn the_named_list_never_contains_an_always_bound_item() {
        let mut store = EventStore::in_memory().unwrap();
        declare_pinned(&mut store, "pinned-named", None);
        serve_n(&mut store, "pinned-named", JUDGEMENT_DEBT_AFTER + 5);
        declare(&mut store, "trigger-named", None);
        serve_n(&mut store, "trigger-named", JUDGEMENT_DEBT_AFTER);

        let named = judgement_debt_named(&store, None);
        let ids: Vec<&str> = named.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["trigger-named"], "an Always-bound item must never be named: {ids:?}");
    }

    /// The evaluation debt's own "items currently owe a verdict" context
    /// count (`bin/serve.rs`'s `evaluation_debt` message) is `judgement_
    /// debt_counts`'s `in_project` half - an all-pinned backlog, however
    /// large, must still read as zero, the same exclusion `an_always_
    /// bound_item_served_past_the_threshold_is_never_owed` above already
    /// proves for a single item.
    #[test]
    fn a_large_all_pinned_backlog_still_reads_as_zero_owed() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..15 {
            let id = format!("pinned-eval-{i}");
            declare_pinned(&mut store, &id, None);
            serve_n(&mut store, &id, JUDGEMENT_DEBT_AFTER);
        }
        let owed_in_project = judgement_debt_counts(&store, None).1;
        assert_eq!(owed_in_project, 0, "an all-pinned backlog must never feed the evaluation debt's own context count");
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

// ------------------------------------------------------------------------
// The evaluation debt (2026-09-12, trigger rewritten twice on 2026-09-16):
// `bin/serve.rs`'s Stop hook, once per session, asks the owner to run the
// WHOLE end-of-session evaluation (`ops::install::seed_eval_command`'s
// routine) rather than judging one more item, once this project has gone
// long enough without one. See `bin/serve.rs`'s own `evaluation_debt` for
// the Stop-hook wiring; everything below is the pure/reusable half, kept
// here for the same reason `JUDGEMENT_DEBT_AFTER` and `judgement_debt_counts`
// already are: `doctor` (`ops::health::judgement_debt_line`) needs the exact
// same numbers and the exact same sidecar, never a second copy that can
// silently drift from what the hook actually acts on.
//
// THE FIRST REWRITE (earlier the same day) replaced a single global "newest
// verdict" clock - reset by a verdict on ANY item that applied to a
// checkout, global items included, so it went days without ever firing on a
// copy of the owner's own store - with a per-project sidecar tracking how
// long a backlog of ten-or-more items had sat continuously at or over that
// ceiling, and whether an evaluation report had been seen since.
//
// THE SECOND REWRITE, THIS ONE, drops the ceiling entirely - decision by the
// owner, 2026-09-16. A count of items owed was never actually the thing he
// wanted to gate on: a project with a small, well-kept backlog could go
// without an evaluation forever, and a project that crossed ten items for a
// single busy hour got asked about regardless of how little time had
// actually been spent there. What he wants instead is simpler - the
// evaluation asked for once a day, per project a session actually works in,
// once that session has put in enough time there for the ask to be worth
// answering (half an hour was tried and rejected as too little; an hour
// stands). Two facts kept per project in the sidecar now drive the "once a
// day" half: `tracking_since` (when this project was first seen at a
// main-session Stop at all - set once, never cleared, so a quiet day no
// longer resets anything the way falling back under the old ceiling used
// to) and `last_evaluation_seen` (unchanged - see `evaluation_report_ids`
// below). A third fact, read fresh from the session rather than the
// sidecar, drives the "enough time" half: how long THIS session has worked
// in the project, from the earliest `item_served` event it recorded here to
// now (`EVAL_MIN_SESSION_MINUTES` below) - a session that only just arrived
// must not be told to stop and evaluate before it has done anything here
// worth evaluating. The item count is still shown in the Stop hook's own
// message, for context - never as a condition any more.
//
// A THIRD REWRITE, THE SAME DAY (`no evaluation is asked outside a
// project, where no report can be filed`): never for a checkout that
// resolves to NO project at all, regardless of how stale or well-worked it
// looks. Filing the one thing that silences this debt - a Report tagged
// `evaluation-report` - is a write `model::gate::declare` judges like any
// other, and ground 21 (`NO_SCOPE_PROBLEM`) refuses to declare a Report, or
// any other archive-kind item, with no project at all. A checkout with no
// project could never file the report that would silence this ask, so
// asking it at all would hold a turn once a day, forever, with no honest
// way out. `bin/serve.rs`'s `evaluation_debt` and its own call site both
// gate on this independently (see that function's own doc comment);
// `update_eval_debt_state` below is skipped at the same call site too, so a
// no-project checkout's sidecar entry (keyed `""`, see `project_key`)
// never grows a `tracking_since` clock nothing could ever act on. A `""`
// entry already sitting in a sidecar written before this existed - the
// owner's own live sidecar held exactly one, started 2026-09-16 21:56,
// from before this gate existed - is left exactly as it is: inert, never
// read for a no-project checkout any more, never deleted either.
//
// A FOURTH REWRITE, THE SAME DAY (owner's decision, 2026-09-16: he does not
// want to ever have to run an evaluation himself, so a debt that could still
// be waited out - once per session, or once every 24 rolling hours from
// whichever of `tracking_since`/`last_evaluation_seen` was later - was not
// enough). The rule is now "once a day, per project, and it does not let
// go": once this session has worked here for `EVAL_MIN_SESSION_MINUTES`,
// the obligation holds for as long as no evaluation report for this project
// has been FIRST SEEN on the current UTC CALENDAR DAY (`eval_done_today`,
// `crate::time::same_utc_day`) - a report seen yesterday no longer buys
// today's silence, the way a report seen 23 hours ago used to.
// `tracking_since` drops out of the predicate entirely: still written, on
// the very first Stop a project is ever seen at (`update_eval_debt_state`'s
// own `get_or_insert`, unchanged), and an old sidecar still reads it under
// either its current name or its pre-2026-09-16 `over_ceiling_since` alias
// - just never again compared against "now" to decide anything. The
// once-per-SESSION gate (`bin/serve.rs`'s old `eval_debt_asked_path`/
// `eval_debt_not_yet_asked_this_session`/`record_eval_debt_asked`, a
// session-keyed sidecar file distinct from this one, all three retired by
// this rewrite) is gone too: the debt now blocks the first Stop of EVERY
// turn for as long as it holds, with Claude Code's own `stop_hook_active`
// (`bin/serve.rs`'s `hook_once`, the `already_fired` branch at the very top
// of its `Stop` arm) the only thing standing between that and blocking a
// retry of the same turn too - this debt adds no second copy of that
// safety, it relies on the one already there, same as every other debt in
// that function. Every ask is now counted on THIS PROJECT's own sidecar
// entry instead of a session's (`asked_count`, `first_asked_since_report`
// below) - "once per session" stopped meaning anything once the debt could
// fire more than once in one, so what a reader needs instead is how many
// times, and since when, this project has gone unanswered; both reset to
// zero/`None` the moment `update_eval_debt_state` below next sees a new
// report, the same Stop that already stamps `last_evaluation_seen`.
//
// A FIFTH REWRITE (2026-09-17: "after three hours of work a new evaluation
// is due, covering what happened since the last one and the state of the
// work"). The UTC-calendar-day rule above still decides the FIRST ask of
// the day, but is no longer the only one: the owner found that a report
// filed first thing in the morning bought silence for the rest of a long
// working day, however far the project drifted after it. Two changes.
// First, "how long has this session worked here" stops being a single
// wall-clock measurement from one `item_served` timestamp
// (`session_first_served_in_project`, retired by this rewrite - it lived in
// `bin/serve.rs`) and becomes ACCRUED WORK: every hook event of a session
// (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `Stop` - a subagent's
// own included, see `record_hook_event`'s own doc comment) adds the gap
// since that exact (project, session) pair's own previous event, but only
// when the gap is under `EVAL_PAUSE_MINUTES` - an idle session accrues
// nothing while it waits. `SessionWorkState` carries this running total per
// session, inside the same per-project sidecar entry this section's other
// fields already live in, and resets to zero exactly when `tracking_since`
// never did: a new UTC day, or a new evaluation report seen for the project
// (`session_work_reset_needed`). Second, `eval_debt_owed` now asks a
// different question depending on whether today's report already exists:
// none yet -> the first threshold, `EVAL_FIRST_WORK_MINUTES`, unchanged in
// value from the retired `EVAL_MIN_SESSION_MINUTES`; one already seen today
// -> `EVAL_REPEAT_WORK_MINUTES`, measured from whenever that report reset
// the accrual, so a long day keeps asking every three hours instead of
// going quiet until the next UTC midnight.

/// How many minutes of ACCRUED WORK (see `SessionWorkState`/`accrue_
/// session_work` below - gap-filtered hook events of this session, so an
/// idle pause contributes nothing) this project must see, since the last
/// reset, before the evaluation debt is willing to speak AT ALL FOR THE
/// FIRST TIME TODAY. An hour, not the half hour first tried: the owner
/// rejected half an hour as too little, on 2026-09-16, because it hijacked
/// the very first question of a session before there was anything to
/// evaluate yet.
///
/// RENAMED FROM `EVAL_MIN_SESSION_MINUTES` (2026-09-17, the fifth rewrite -
/// see this section's own doc comment): the value is unchanged, only the
/// name and what it is measured against - a single wall-clock minutes-since
/// measurement gave way to gap-filtered accrued work - so the name now says
/// which of the two thresholds below this one is (the FIRST ask of the day;
/// `EVAL_REPEAT_WORK_MINUTES` is the other).
pub const EVAL_FIRST_WORK_MINUTES: i64 = 60;

/// How many minutes of accrued work, since a report was LAST seen for this
/// project, before a REPEAT evaluation is due the same day - the owner's
/// decision, 2026-09-17: even a project that filed its evaluation this
/// morning can drift for the rest of a long day, so a report already seen
/// today no longer buys silence until midnight, only until this much more
/// work has gone by since it was filed. Three hours: long enough that a
/// normal session's first-of-day report is never immediately followed by a
/// second ask, short enough that a full working day still sees more than
/// one.
pub const EVAL_REPEAT_WORK_MINUTES: i64 = 180;

/// A gap this long or longer between two consecutive hook events of the same
/// (project, session) pair is a pause, not work, and contributes nothing to
/// either threshold above - see `accrue_session_work`. Half an hour: long
/// enough that a normal think-then-type rhythm, or one slow tool call, never
/// reads as a pause, short enough that a lunch break or an overnight gap
/// reliably does.
pub const EVAL_PAUSE_MINUTES: i64 = 30;

/// How far the accrued-work sidecar is allowed to drift from what
/// `record_hook_event` last actually wrote to disk before it bothers
/// writing again - see that function's own doc comment for why a small,
/// sub-threshold gap can safely stay unwritten (the next write telescopes
/// it in exactly) and why a `Stop` event, or a reset that just happened,
/// is never subject to this throttle at all.
const EVAL_WORK_WRITE_THROTTLE_SECS: i64 = 60;

/// THE TIME HALF OF THE PURE PREDICATE, factored out of `eval_debt_owed`
/// below so `doctor` (`ops::health::judgement_debt_line`), which runs cold
/// outside any session and so can never evaluate the minutes-worked half,
/// can still report accurately on the half it CAN evaluate - without
/// duplicating the "is this the current UTC day" rule a second time and
/// risking it drift from the one `eval_debt_owed` actually acts on. Replaces
/// `eval_debt_stale` and the retired `EVAL_DEBT_STALE_HOURS` (2026-09-16,
/// fourth rewrite, this section's own doc comment): a rolling 24 hours
/// measured against the LATER of `tracking_since`/`last_evaluation_seen`
/// gave way to a UTC calendar day measured against `last_evaluation_seen`
/// alone.
///
/// Holds (today's evaluation IS done) when `last_evaluation_seen` is
/// `Some`, AND it falls on the same UTC calendar day as `now_unix`
/// (`crate::time::same_utc_day`). `None` - no evaluation report has ever
/// been seen for this project - reads as "not done today", never as "done
/// forever": there is no instant for a report that was never seen, so there
/// is nothing for `same_utc_day` to agree with.
pub fn eval_done_today(last_evaluation_seen: Option<i64>, now_unix: i64) -> bool {
    last_evaluation_seen.is_some_and(|seen| crate::time::same_utc_day(seen, now_unix))
}

/// THE WHOLE PURE PREDICATE. Rewritten 2026-09-16 to drop `tracking_since`
/// and the 24-hour rolling window entirely in favour of a UTC calendar day
/// (see this section's own doc comment, fourth rewrite, for why); rewritten
/// again 2026-09-17 (fifth rewrite) to ask a DIFFERENT threshold once today's
/// report already exists, rather than staying silent for the rest of the
/// day regardless of how much more work follows it. Every input is already
/// resolved elsewhere (`accrued_minutes` from `SessionWorkState::
/// accrued_secs` via `record_hook_event`, `last_evaluation_seen` from this
/// project's own `ProjectEvalState`, `now_unix` from `crate::time::
/// now_unix`), so this stays nothing but the conditions themselves,
/// unit-testable with plain integers and no store, no clock, no filesystem.
///
/// Holds when `accrued_minutes` has reached the threshold FOR WHICHEVER
/// CASE APPLIES: `EVAL_REPEAT_WORK_MINUTES` when `eval_done_today` above
/// already holds (a report exists for today, so this would be a repeat
/// ask), or `EVAL_FIRST_WORK_MINUTES` when it does not (no report yet
/// today, so this would be the first ask). `accrued_minutes` itself is
/// already measured from whichever reset last applied - a new UTC day or a
/// newly seen report, both handled by `session_work_reset_needed` before
/// this predicate ever runs - so neither branch here needs to look at a
/// reset instant a second time.
///
/// THE REPEAT BRANCH ALSO NEEDS A RISK, since the sixth rewrite (2026-09-17,
/// owner's decision - see this file's own "the repeat's own risk" doc
/// comment): `edits_since_test` reaching `EVAL_REPEAT_MIN_UNTESTED_EDITS`, or
/// `compacted_since_anchor` - either is enough, neither is required of the
/// other. THE FIRST-OF-DAY BRANCH IS UNCHANGED: no risk condition at all,
/// only the accrued-time floor, exactly as it was before this rewrite.
pub fn eval_debt_owed(accrued_minutes: i64, last_evaluation_seen: Option<i64>, now_unix: i64, edits_since_test: u32, compacted_since_anchor: bool) -> bool {
    if eval_done_today(last_evaluation_seen, now_unix) {
        accrued_minutes >= EVAL_REPEAT_WORK_MINUTES && (edits_since_test >= EVAL_REPEAT_MIN_UNTESTED_EDITS || compacted_since_anchor)
    } else {
        accrued_minutes >= EVAL_FIRST_WORK_MINUTES
    }
}

/// Whole days between `then_unix` and `now_unix`, floored, never negative -
/// the one "N day(s) ago" rule both `doctor`'s judgement-debt line and the
/// Stop hook's own evaluation-debt message use for the same instant, so
/// neither ever rounds it differently from the other.
pub fn days_ago(now_unix: i64, then_unix: i64) -> i64 {
    (now_unix - then_unix).max(0) / 86400
}

/// Whole minutes between `then_unix` and `now_unix`, floored, never
/// negative - `days_ago`'s own rule in a different unit. Used to be the
/// evaluation debt's own "how long has this session worked here" clock;
/// retired from that role 2026-09-17 (fifth rewrite, this section's own doc
/// comment) in favour of `SessionWorkState::accrued_secs`, which measures
/// gap-filtered accrued work rather than plain wall-clock elapsed time.
/// Kept as a general-purpose sibling of `days_ago` above.
pub fn minutes_ago(now_unix: i64, then_unix: i64) -> i64 {
    (now_unix - then_unix).max(0) / 60
}

/// The sidecar's own map key for a project - `""` stands for "no project" (a
/// checkout that resolves to no project at all, `None`). Never a real
/// project's own key: `project::resolve_project` never returns `Some("")` -
/// a blank marker line falls through to the git root instead (see that
/// function's own doc comment) - so this can never collide with a real one.
fn project_key(project: Option<&str>) -> &str {
    project.unwrap_or("")
}

// --------------------------------------------------- the repeat's own risk
//
// THE SIXTH REWRITE (2026-09-17, owner's decision): a REPEAT ask (a report
// already exists for today) must no longer fire on accrued time alone.
// Measured on two of the owner's own real sessions: an unconditional repeat
// landed on a calm moment - no code touched since the last test or build
// run, nothing summarized - six times out of ten. The FIRST evaluation of
// the day (no report yet today) is UNCHANGED: it keeps no risk condition at
// all, only the accrued-time floor (`eval_debt_owed` below).
//
// Two risks, either one enough: `SessionWorkState::edits_since_test`
// reaching `EVAL_REPEAT_MIN_UNTESTED_EDITS`, or `SessionWorkState::
// compacted_since_anchor` being true. Both live alongside `accrued_secs` in
// the exact same per-(project, session) struct, and reset at the exact same
// moments (a new UTC day, or a new evaluation report seen for the project -
// `session_work_reset_needed`), since they answer the identical question
// "since the last reset" that `accrued_secs` already does.

/// How many Edit/Write/NotebookEdit calls on a non-doc file, since the last
/// test or build command, are enough to count as a risk on their own - see
/// `HookEventKind::UntestedEdit`/`SessionWorkState::edits_since_test`. Three,
/// not one: a single edit is routine and would make the repeat fire on
/// almost any three-hour stretch of real work, which is exactly the
/// unconditional-repeat defect this rewrite exists to close; three in a row
/// with no test or build run in between is a real pattern, not noise.
pub const EVAL_REPEAT_MIN_UNTESTED_EDITS: u32 = 3;

/// Commands that reset `SessionWorkState::edits_since_test` back to zero when
/// they appear in a Bash/PowerShell call's own command text - matched as a
/// plain substring (`str::contains`), deliberately: a real invocation carries
/// flags, a working directory change, output redirection and the rest around
/// the bare runner name, and a substring match catches all of that without
/// ever having to parse a shell command line. Read generically off whichever
/// tool call carries a `command` field at all (`absent_guard::
/// proposed_command`'s own stance, "nothing here hard-codes that name") - a
/// `Bash` tool and a `PowerShell` tool alike, on whichever platform the
/// session happens to run on.
///
/// A NAMED CONSTANT LIST, not a config file or a per-project setting: THOR
/// itself works across many of the owner's own projects, of several
/// different languages and build tools, so this stays generic rather than
/// tuned to any one of them - unrelated to, and deliberately not shared
/// with, `eval-command.example.md`'s own narrower `allowed-tools` list (the
/// commands THAT file pre-approves for running THIS evaluation routine
/// itself, a Rust workspace's own subset of this list).
pub const EVAL_TEST_RUNNER_COMMANDS: &[&str] =
    &["cargo test", "cargo build", "npm test", "npm run test", "npm run verify", "node --test", "pytest", "python -m pytest", "pio run", "pio test", "make test", "go test", "dotnet test"];

/// File extensions an Edit/Write/NotebookEdit call never counts toward
/// `edits_since_test` for, even with no test or build run since - prose and
/// documentation carry no behaviour a test could ever catch failing, so
/// editing one is not the risk this counter exists to flag.
const EVAL_UNTESTED_EDIT_EXEMPT_EXTENSIONS: &[&str] = &[".md", ".txt", ".rst"];

/// What one hook event means for the two risk counters above - decided once,
/// at the call site in `bin/serve.rs` where the raw JSON payload is still in
/// scope (`classify_hook_event` below), so `accrue_session_work` itself never
/// needs to know Claude Code's own payload shape, only this closed
/// vocabulary. `Copy`: cheap enough, and passed by value everywhere it is
/// used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEventKind {
    /// An Edit, Write or NotebookEdit call on a file whose extension is not
    /// in `EVAL_UNTESTED_EDIT_EXEMPT_EXTENSIONS`.
    UntestedEdit,
    /// A Bash/PowerShell-style call whose own command text contains one of
    /// `EVAL_TEST_RUNNER_COMMANDS`.
    TestRun,
    /// A `SessionStart` whose own `source` field reads `"compact"`.
    CompactStart,
    /// Everything else - contributes to the accrued-work clock only, neither
    /// risk counter moves.
    Other,
}

/// The pure classification `bin/serve.rs` calls once per hook event, straight
/// off the raw payload fields it already has in scope, before ever touching
/// `accrue_session_work`/`record_hook_event` below - see `HookEventKind`'s
/// own doc comment for why this stays a plain function of a few borrowed
/// strings rather than the whole `serde_json::Value` payload: a pure function
/// of plain fields is unit-testable with no fixture JSON at all.
///
/// ORDER OF CHECKS: a compaction is decided first and returns immediately -
/// `event_name`/`source` are meaningless for a PreToolUse-shaped payload, so
/// there is no ambiguity to break here. A command matching a test/build
/// runner wins over the untested-edit check below it on purpose, though in
/// practice the two can never both be true of the same real Claude Code
/// payload anyway (a Bash/PowerShell call carries a `command` and no
/// `file_path`; an Edit/Write/NotebookEdit call carries a `file_path` and no
/// `command` - the identical non-overlap `bin/serve.rs`'s own command-guard
/// doc comment already notes for the same two shapes).
pub fn classify_hook_event(event_name: &str, tool_name: &str, file_path: Option<&str>, command: Option<&str>, source: Option<&str>) -> HookEventKind {
    if event_name == "SessionStart" && source == Some("compact") {
        return HookEventKind::CompactStart;
    }
    if let Some(command) = command {
        if EVAL_TEST_RUNNER_COMMANDS.iter().any(|runner| command.contains(runner)) {
            return HookEventKind::TestRun;
        }
    }
    if matches!(tool_name, "Edit" | "Write" | "NotebookEdit") {
        if let Some(file_path) = file_path {
            if !EVAL_UNTESTED_EDIT_EXEMPT_EXTENSIONS.iter().any(|ext| file_path.ends_with(ext)) {
                return HookEventKind::UntestedEdit;
            }
        }
    }
    HookEventKind::Other
}

#[cfg(test)]
mod classify_hook_event_tests {
    use super::*;

    #[test]
    fn an_edit_on_a_source_file_is_an_untested_edit() {
        assert_eq!(classify_hook_event("PreToolUse", "Edit", Some("src/main.rs"), None, None), HookEventKind::UntestedEdit);
    }

    #[test]
    fn a_write_on_a_source_file_is_an_untested_edit() {
        assert_eq!(classify_hook_event("PreToolUse", "Write", Some("src/new.rs"), None, None), HookEventKind::UntestedEdit);
    }

    #[test]
    fn a_notebook_edit_on_a_notebook_is_an_untested_edit() {
        assert_eq!(classify_hook_event("PreToolUse", "NotebookEdit", Some("analysis.ipynb"), None, None), HookEventKind::UntestedEdit);
    }

    /// Case named in the build brief: a .md edit does not count.
    #[test]
    fn an_edit_on_a_markdown_file_does_not_count() {
        assert_eq!(classify_hook_event("PreToolUse", "Edit", Some("README.md"), None, None), HookEventKind::Other);
    }

    #[test]
    fn an_edit_on_a_txt_or_rst_file_does_not_count_either() {
        assert_eq!(classify_hook_event("PreToolUse", "Edit", Some("notes.txt"), None, None), HookEventKind::Other);
        assert_eq!(classify_hook_event("PreToolUse", "Edit", Some("docs/index.rst"), None, None), HookEventKind::Other);
    }

    #[test]
    fn an_edit_with_no_file_path_at_all_does_not_count() {
        assert_eq!(classify_hook_event("PreToolUse", "Edit", None, None, None), HookEventKind::Other);
    }

    #[test]
    fn a_read_or_bash_call_on_a_source_file_path_is_not_an_edit_at_all() {
        // Only Edit/Write/NotebookEdit ever carry the risk - a tool that
        // merely NAMES a source file (Read, or a Bash call whose command
        // happens to mention one) must never count as touching it.
        assert_eq!(classify_hook_event("PreToolUse", "Read", Some("src/main.rs"), None, None), HookEventKind::Other);
    }

    #[test]
    fn a_bash_command_containing_a_runner_substring_is_a_test_run() {
        assert_eq!(classify_hook_event("PreToolUse", "Bash", None, Some("cargo test --workspace"), None), HookEventKind::TestRun);
    }

    #[test]
    fn a_powershell_command_containing_a_runner_substring_is_a_test_run() {
        assert_eq!(classify_hook_event("PreToolUse", "PowerShell", None, Some("cd thor2; cargo build --release"), None), HookEventKind::TestRun);
    }

    #[test]
    fn every_named_runner_substring_is_recognised() {
        for runner in EVAL_TEST_RUNNER_COMMANDS {
            let command = format!("cd project && {runner} --flag");
            assert_eq!(classify_hook_event("PreToolUse", "Bash", None, Some(&command), None), HookEventKind::TestRun, "{runner}");
        }
    }

    #[test]
    fn a_command_matching_no_runner_is_not_a_test_run() {
        assert_eq!(classify_hook_event("PreToolUse", "Bash", None, Some("git status"), None), HookEventKind::Other);
    }

    #[test]
    fn a_session_start_with_compact_source_is_a_compact_start() {
        assert_eq!(classify_hook_event("SessionStart", "", None, None, Some("compact")), HookEventKind::CompactStart);
    }

    #[test]
    fn a_session_start_with_a_different_source_is_not_a_compact_start() {
        assert_eq!(classify_hook_event("SessionStart", "", None, None, Some("startup")), HookEventKind::Other);
    }

    #[test]
    fn a_session_start_with_no_source_at_all_is_not_a_compact_start() {
        assert_eq!(classify_hook_event("SessionStart", "", None, None, None), HookEventKind::Other);
    }

    #[test]
    fn a_compact_source_on_a_different_event_name_is_never_a_compact_start() {
        // "compact" only means anything as a SessionStart's own source -
        // guards against a coincidental match on some other event's payload.
        assert_eq!(classify_hook_event("PreToolUse", "", None, None, Some("compact")), HookEventKind::Other);
    }
}

/// One session's own running account of ACCRUED WORK inside one project -
/// see this section's own doc comment, fifth rewrite, for the whole story.
/// Lives inside that project's `ProjectEvalState`, keyed by session id
/// (`ProjectEvalState::sessions`), since the obligation this backs is
/// always asked of ONE session at a time, never the project as a whole.
///
/// `#[serde(default)]` on every field, the same stance `ProjectEvalState`
/// itself already takes: a sidecar written before this field existed at all
/// must still parse, reading a session it has never heard of as the
/// all-default "no work accrued yet" state rather than a parse failure.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionWorkState {
    /// When this accrual period started - `None` before this (project,
    /// session) pair has ever seen a hook event. Reset to `Some(now)`
    /// alongside `accrued_secs` by `session_work_reset_needed`/`accrue_
    /// session_work` below; otherwise left exactly as it was, the same
    /// "set once, moves only on a reset" shape `tracking_since` above uses.
    #[serde(default)]
    pub anchor_unix: Option<i64>,
    /// Seconds of work accrued since `anchor_unix` - the sum of every gap
    /// between two consecutive hook events of this exact (project, session)
    /// pair that was itself under `EVAL_PAUSE_MINUTES`. Never decreases
    /// except by a full reset back to zero.
    #[serde(default)]
    pub accrued_secs: i64,
    /// The instant of the most recent hook event this (project, session)
    /// pair was seen at - the basis `accrue_session_work` measures its next
    /// gap from. Distinct from `anchor_unix`: this one moves on EVERY event,
    /// reset or not, while `anchor_unix` only ever moves on a reset.
    #[serde(default)]
    pub last_event_unix: Option<i64>,
    /// How many Edit/Write/NotebookEdit calls on a non-doc file have fired
    /// since `anchor_unix`, minus whichever of those a test or build command
    /// has since cancelled out - see `HookEventKind::UntestedEdit`/`TestRun`
    /// and this section's own "the repeat's own risk" doc comment. Reset to
    /// zero by the same full reset that zeroes `accrued_secs` (a new UTC day,
    /// or a new evaluation report), AND independently by a test/build
    /// command that changes nothing else about this state.
    #[serde(default)]
    pub edits_since_test: u32,
    /// Whether a `SessionStart` with `"source": "compact"` has arrived for
    /// this session since `anchor_unix` - see `HookEventKind::CompactStart`.
    /// Sticky once true (a compaction earlier in this same anchor period is
    /// still a real risk now), reset only by the same full reset that zeroes
    /// `accrued_secs`.
    #[serde(default)]
    pub compacted_since_anchor: bool,
}

/// Whether `session`'s own accrual must restart at `now_unix` rather than
/// fold one more gap onto what it already carries - see this section's own
/// doc comment, fifth rewrite. Three cases, all "start over": no event has
/// ever been seen for this (project, session) pair (`anchor_unix` is
/// `None`); `now_unix` falls on a different UTC calendar day than the
/// anchor (`crate::time::same_utc_day` - the identical rule `eval_done_
/// today` already applies to `last_evaluation_seen`, so a session that
/// works past midnight UTC resets here at exactly the instant a fresh
/// `EVAL_FIRST_WORK_MINUTES` ask becomes possible again); or a newer
/// evaluation report has been seen for the project than this accrual period
/// ever accounted for (`last_evaluation_seen` strictly later than
/// `anchor_unix` - a report seen BEFORE this period began was already the
/// reason for the reset that started it, most recently).
fn session_work_reset_needed(session: &SessionWorkState, now_unix: i64, last_evaluation_seen: Option<i64>) -> bool {
    match session.anchor_unix {
        None => true,
        Some(anchor) => !crate::time::same_utc_day(anchor, now_unix) || last_evaluation_seen.is_some_and(|seen| seen > anchor),
    }
}

/// One hook event's worth of work, folded onto `session`'s own running
/// total - the pure heart of the accrual (see this section's own doc
/// comment, fifth rewrite). A reset (`session_work_reset_needed` above)
/// discards whatever was accrued before it outright and starts a fresh
/// anchor at `now_unix`, contributing no gap of its own - there is no
/// honest "previous event" to measure against once the period it belonged
/// to is over. Otherwise, the gap since `session.last_event_unix` (`now_
/// unix` itself when this pair has an anchor but, oddly, no recorded event -
/// never observed in practice, since a reset always sets both together; the
/// safe "no gap" fallback rather than a panic) is real work when it is
/// under `EVAL_PAUSE_MINUTES` and is added in full; at or over it, it is a
/// pause and adds nothing - though `last_event_unix` still moves forward to
/// `now_unix` regardless, so a LATER short gap is never measured against a
/// stale instant from before the pause (the defect a naive "only update on
/// a write" version would have - see `record_hook_event`'s own doc comment
/// on the write throttle for why that one is safe and this one would not
/// be). A negative gap (clock skew) is clamped to zero, never subtracted.
///
/// `event` (sixth rewrite, 2026-09-17 - see this section's own "the repeat's
/// own risk" doc comment) folds onto the two risk counters the identical way
/// a reset already folds onto `accrued_secs`: a reset discards them outright,
/// then this same event's own kind seeds the fresh period (an `UntestedEdit`
/// that itself triggered a reset still counts as the period's first edit;
/// nothing else does). Otherwise: `UntestedEdit` adds one to `edits_since_
/// test`, `TestRun` drops it straight back to zero, `CompactStart` sets
/// `compacted_since_anchor` (sticky - never cleared by anything but a reset),
/// and `Other` touches neither.
pub fn accrue_session_work(session: &SessionWorkState, now_unix: i64, last_evaluation_seen: Option<i64>, event: HookEventKind) -> SessionWorkState {
    if session_work_reset_needed(session, now_unix, last_evaluation_seen) {
        return SessionWorkState {
            anchor_unix: Some(now_unix),
            accrued_secs: 0,
            last_event_unix: Some(now_unix),
            edits_since_test: u32::from(event == HookEventKind::UntestedEdit),
            compacted_since_anchor: event == HookEventKind::CompactStart,
        };
    }
    let gap = now_unix - session.last_event_unix.unwrap_or(now_unix);
    let add = if (0..EVAL_PAUSE_MINUTES * 60).contains(&gap) { gap } else { 0 };
    let edits_since_test = match event {
        HookEventKind::UntestedEdit => session.edits_since_test + 1,
        HookEventKind::TestRun => 0,
        HookEventKind::CompactStart | HookEventKind::Other => session.edits_since_test,
    };
    SessionWorkState {
        anchor_unix: session.anchor_unix,
        accrued_secs: session.accrued_secs + add,
        last_event_unix: Some(now_unix),
        edits_since_test,
        compacted_since_anchor: session.compacted_since_anchor || event == HookEventKind::CompactStart,
    }
}

/// This project's own two facts for the evaluation debt's trigger (see this
/// section's own doc comment for why a per-project sidecar replaced a single
/// global verdict clock). `#[serde(default)]` on every field: a sidecar
/// written by an earlier partial run, or hand-written for a test, must still
/// parse, with "nothing known yet" rather than a parse failure - the same
/// fail-open stance `capture::Marker`'s own fields already take.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectEvalState {
    /// The first time a main-session Stop was ever seen in this project -
    /// `None` before that has ever happened. Set once
    /// (`update_eval_debt_state`'s `get_or_insert`) and never cleared again:
    /// unlike the ceiling-crossing clock this replaced (`over_ceiling_since`,
    /// still the name a sidecar written before 2026-09-16 carries - see the
    /// `serde(alias)` below for why that value carries over unchanged), a
    /// quiet day, or the judgement debt dropping to zero, no longer resets
    /// anything: the question this field answers is no longer "how long has
    /// a backlog sat over some line" but simply "how long has THOR been
    /// tracking this project at all".
    #[serde(alias = "over_ceiling_since", default)]
    pub tracking_since: Option<i64>,
    /// Every id this project has ever had for a live Report carrying the
    /// `evaluation-report` tag, as of the most recent Stop that looked - the
    /// membership `last_evaluation_seen` below is measured against. Grows
    /// only: an id is never removed just because the report was later
    /// retracted, since a `Report`, once filed, has already done the one
    /// thing this sidecar cares about - existed, and was seen.
    #[serde(default)]
    pub known_report_ids: std::collections::BTreeSet<String>,
    /// The time a Stop first saw an id that was not yet in
    /// `known_report_ids` - `None` when no evaluation report has ever been
    /// seen for this project. Deliberately NOT the report's own filing time:
    /// the event log records no creation time for a live item, only when
    /// each revision was appended, and a Report can be revised - "first seen
    /// by a Stop" is the only time source this sidecar has, and it is
    /// exactly the instant that matters here, since it is what a LATER Stop
    /// compares "now" against to decide whether the silence it bought has
    /// worn off.
    #[serde(default)]
    pub last_evaluation_seen: Option<i64>,
    /// The id that set `last_evaluation_seen` - named so a reader (`doctor`,
    /// the Stop hook's own message) can point at which report actually
    /// silenced this, not only when. When more than one new id appears in
    /// the same Stop (rare: reports are filed one at a time in practice),
    /// the lexicographically greatest wins - a deterministic, testable
    /// tie-break, and `eval-<project>-<date>`'s own shape means that is
    /// usually also the newest by date.
    #[serde(default)]
    pub last_evaluation_report_id: Option<String>,
    /// How many times the Stop hook has blocked a turn for this project's
    /// evaluation debt since the last report was seen - added 2026-09-16
    /// (fourth rewrite, this section's own doc comment) to replace the old
    /// once-per-SESSION sidecar (`bin/serve.rs`'s retired `eval-debt-
    /// asked.json`) now that the debt blocks every turn instead of at most
    /// one per session, so a reader needs to know how many times THIS
    /// PROJECT has been asked, not merely whether one particular session
    /// already heard it once. Reset to 0 the moment a new report is seen
    /// (`update_eval_debt_state` below, the same branch that stamps `last_
    /// evaluation_seen`) - counts asks SINCE THE LAST REPORT, never a
    /// lifetime total.
    #[serde(default)]
    pub asked_count: u32,
    /// The instant the FIRST of those asks happened - `None` when nothing
    /// is currently unanswered (no report is owed right now, or the debt
    /// has never fired since the last one was seen). Set once
    /// (`record_eval_debt_asked`'s own `get_or_insert`) and reset to `None`
    /// alongside `asked_count` the moment a new report is seen.
    #[serde(default)]
    pub first_asked_since_report: Option<i64>,
    /// Every session's own accrued-work account inside this project, keyed
    /// by session id - added 2026-09-17 (fifth rewrite, this section's own
    /// doc comment) alongside `accrue_session_work`/`record_hook_event`.
    /// `#[serde(default)]` so a sidecar written before this field existed
    /// parses with an empty map, exactly the "no work accrued yet" state a
    /// session this old sidecar never heard of would read as anyway.
    #[serde(default)]
    pub sessions: std::collections::BTreeMap<String, SessionWorkState>,
}

/// The whole sidecar: one [`ProjectEvalState`] per project key (see
/// [`project_key`]).
pub type EvalDebtState = std::collections::BTreeMap<String, ProjectEvalState>;

/// Where the evaluation debt's trigger state lives, next to the store - the
/// same directory and the same naming convention as every other sidecar on
/// this boundary (`eval-debt-asked.json`, `teeth-asked.json`, `session-
/// watermark.json`, all in `bin/serve.rs`).
pub fn eval_debt_state_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("eval-debt-state.json")
}

/// Read the whole sidecar. FAIL-OPEN, the same stance every sidecar on this
/// boundary already takes (`capture::read_markers`, `bin/serve.rs`'s own
/// session-watermark reader): a missing file, an unreadable one, or one that
/// fails to parse all come back as an empty map - "nothing known yet", never
/// an error, since this backs a Stop-time read/write path that must never
/// block and a `doctor` read path that must never speak on failure.
pub fn read_eval_debt_state(db: &Path) -> EvalDebtState {
    std::fs::read_to_string(eval_debt_state_path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// This project's own two facts, or the all-default "nothing known yet"
/// state when the sidecar - or just this project's own entry in it - does
/// not exist. READ-ONLY: `doctor` (`ops::health::judgement_debt_line`) calls
/// this, and only this, never [`update_eval_debt_state`] below - a read-only
/// diagnostic must never write, or the two facts it reports would depend on
/// whichever tool happened to run last.
pub fn project_eval_state(db: &Path, project: Option<&str>) -> ProjectEvalState {
    read_eval_debt_state(db).remove(project_key(project)).unwrap_or_default()
}

/// Every id of a live Report, scoped to EXACTLY `project` (not
/// `project::applies_to`'s "or global" rule: a report filed for one project
/// must never silence a different project's own debt, and - moot in
/// practice, since `model::gate`'s ground 21 refuses a scopeless Report
/// outright except for one exempt id - a global report would otherwise only
/// ever have silenced a checkout that itself resolves to no project),
/// carrying the `evaluation-report` tag. The set [`update_eval_debt_state`]
/// below compares each Stop's own live set against.
fn evaluation_report_ids(store: &EventStore, project: Option<&str>) -> Vec<String> {
    crate::live::live_items(store)
        .into_iter()
        .filter(|li| li.item.kind == model::item::Kind::Report)
        .filter(|li| li.item.project.as_deref() == project)
        .filter(|li| li.item.tags.iter().any(|t| t == "evaluation-report"))
        .map(|li| li.id)
        .collect()
}

/// The newest live evaluation-report Report for `project`, paired with its
/// own first event's seq - "newest" meaning highest creation seq, since a
/// live item's own JSON body carries no creation timestamp anywhere in this
/// system. `None` when the store holds none for this project.
///
/// WHY `doctor` (`ops::health::judgement_debt_line`) NEEDS THIS INSTEAD OF
/// JUST READING THE SIDECAR'S OWN `last_evaluation_report_id`. The sidecar
/// only learns about a report the next time a Stop runs in this project
/// after it was filed - `update_eval_debt_state` above stamps it, but
/// nothing runs that function outside a real Stop. A report filed and never
/// yet followed by a Stop is real, live, and sitting in the store right
/// now, but invisible to `project_eval_state`. `doctor` runs cold, with no
/// Stop of its own, so reading only the sidecar would let it miss - and
/// never even mention - a report that exists. This reads the store
/// directly instead, the same store `judgement_debt_counts` already opens
/// for the rest of this same doctor line.
///
/// One `get_events_by_entity` call per candidate id - cheap in practice,
/// since a project rarely accumulates more than a handful of these over its
/// life (one evaluation report a day, at most, is the entire point of the
/// debt this backs).
pub fn newest_evaluation_report(store: &EventStore, project: Option<&str>) -> Option<(String, i64)> {
    evaluation_report_ids(store, project)
        .into_iter()
        .filter_map(|id| {
            let seq = store.get_events_by_entity(&id).ok()?.first()?.seq;
            Some((id, seq))
        })
        .max_by_key(|(_, seq)| *seq)
}

/// Refresh this project's own facts from `store` as it stands right now,
/// and write the sidecar back if anything actually changed. Called ONCE PER
/// MAIN-SESSION STOP in a project (`bin/serve.rs`'s `hook_once`,
/// unconditionally within its own `!is_subagent && stop_project.is_some()`
/// guard) - deliberately unconditioned by the enough-time-worked-here gate
/// that decides whether the obligation is actually SHOWN (the minutes-
/// worked half of `eval_debt_owed`): that is about whether to SPEAK, this
/// is about whether the RECORD stays true, and a session that is never
/// asked must still leave the tracking clock and the report sighting
/// exactly as accurate as one that was. NEVER called at all for a checkout
/// with no project (`stop_project.is_some()`, added 2026-09-16) - see this
/// file's own "evaluation debt" section doc comment, third rewrite, for
/// why: no project means no Report can ever be filed to silence this, so
/// there is nothing honest for a `""`-keyed entry to track.
///
/// NEVER CALLED BY `doctor` (`ops::health::judgement_debt_line`), which
/// reads this same state through [`project_eval_state`] but must stay
/// read-only - see that function's own doc comment.
pub fn update_eval_debt_state(store: &EventStore, db: &Path, project: Option<&str>, now_unix: i64) -> ProjectEvalState {
    let mut all = read_eval_debt_state(db);
    let key = project_key(project).to_string();
    let before = all.get(&key).cloned().unwrap_or_default();
    let mut entry = before.clone();

    // `tracking_since`: set on the FIRST main-session Stop this project was
    // ever seen at (`get_or_insert` never overwrites a `Some`), and left
    // untouched on every Stop after that, forever. No longer read by
    // anything that decides whether the debt fires (`eval_debt_owed`, since
    // 2026-09-16's fourth rewrite - see this section's own doc comment) but
    // still written, unchanged: a project's own "how long has THOR known
    // about it" is still honest, still cheap to keep true, and an old
    // sidecar already carries it under either name (see the `serde(alias)`
    // above).
    entry.tracking_since.get_or_insert(now_unix);

    // `last_evaluation_seen`/`last_evaluation_report_id`: any id live right
    // now that this sidecar has not seen before is "new" - stamp the moment
    // and grow the known set, so the SAME report never re-triggers this on a
    // later Stop just for still existing. THE SAME MOMENT ALSO SILENCES THE
    // ASK COUNTER (2026-09-16, fourth rewrite): a new report is exactly the
    // event that answers every unanswered ask since the last one, so
    // `asked_count`/`first_asked_since_report` reset together with it -
    // never on a quiet day, never on the calendar rolling over, only ever
    // on a report actually being seen (see `record_eval_debt_asked` below
    // for the other half, incrementing these on an ask).
    let new_ids: Vec<String> =
        evaluation_report_ids(store, project).into_iter().filter(|id| !entry.known_report_ids.contains(id)).collect();
    if let Some(newest) = new_ids.iter().max().cloned() {
        entry.last_evaluation_seen = Some(now_unix);
        entry.last_evaluation_report_id = Some(newest);
        entry.asked_count = 0;
        entry.first_asked_since_report = None;
    }
    entry.known_report_ids.extend(new_ids);

    if entry == before {
        return entry;
    }
    all.insert(key, entry.clone());
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = std::fs::write(eval_debt_state_path(db), text);
    }
    entry
}

/// Record one more Stop-hook ask for this project's evaluation debt -
/// called only when the debt actually fires and the turn is about to be
/// blocked for it (`bin/serve.rs`'s `hook_once`, right where the old
/// session-keyed `record_eval_debt_asked` used to write `eval-debt-
/// asked.json`, retired 2026-09-16 alongside the once-per-session gate - see
/// this section's own doc comment, fourth rewrite). Increments this
/// project's own `asked_count` and, if nothing is currently unanswered,
/// stamps `first_asked_since_report` with `now_unix` - `get_or_insert`,
/// exactly like `tracking_since` above, so a repeat ask never moves the
/// clock a later reader measures "how long has this gone unanswered"
/// against. Both are reset together the moment `update_eval_debt_state`
/// above next sees a new report.
///
/// UNCONDITIONAL WRITE, unlike `update_eval_debt_state` above: every call
/// here is a real state change (the count always goes up by at least one),
/// so there is no "nothing changed" case to short-circuit.
///
/// Best effort, like every sidecar here: a write that fails leaves the next
/// ask to try again, never breaks a turn.
pub fn record_eval_debt_asked(db: &Path, project: Option<&str>, now_unix: i64) {
    let mut all = read_eval_debt_state(db);
    let key = project_key(project).to_string();
    let mut entry = all.get(&key).cloned().unwrap_or_default();
    entry.asked_count += 1;
    entry.first_asked_since_report.get_or_insert(now_unix);
    all.insert(key, entry);
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = std::fs::write(eval_debt_state_path(db), text);
    }
}

/// The impure half of `accrue_session_work`: read this (project, session)
/// pair's own accrual out of the sidecar, fold in one more hook event at
/// `now_unix`, and write the result back - UNLESS the change is small enough
/// to skip, in which case the caller still gets the correct computed value,
/// only not yet persisted. `None` for `project: None`, touching neither the
/// sidecar nor the filesystem at all - the identical stance every other
/// evaluation-debt write already takes (`update_eval_debt_state`'s own call
/// site in `bin/serve.rs`'s `hook_once`): a checkout with no project can
/// never file the Report that silences this debt, so there is nothing
/// honest for a `""`-keyed session entry to track.
///
/// CALLED ON EVERY HOOK EVENT of a session inside a project - `SessionStart`,
/// `UserPromptSubmit`, `PreToolUse` and `Stop` alike, a subagent's own
/// events included (`bin/serve.rs`'s `hook_once`, unconditioned by
/// `is_subagent`: a subagent's own work still counts as work of ITS session,
/// even though its `Stop` can never be the one that blocks - that gate is
/// applied where `evaluation_debt` decides whether to speak, never here).
/// Deliberately store-free, unlike `update_eval_debt_state`: this never
/// opens the `EventStore` at all, only the small JSON sidecar already beside
/// it, which is what keeps it cheap enough to call from `PreToolUse` on
/// every single tool call without turning that path into a store scan.
///
/// THE WRITE THROTTLE. Persists the whole sidecar back to disk when `is_stop`
/// is true (a `Stop` is rare enough, and important enough as the one event
/// that can actually block a turn, that it is never worth deferring), OR
/// when a reset just happened (`after.anchor_unix != before.anchor_unix` -
/// worth making durable immediately, though even an unwritten reset
/// self-heals on the very next event, since `session_work_reset_needed`
/// re-derives it fresh from the still-stale on-disk anchor rather than from
/// anything this call would have cached), OR when the WALL-CLOCK gap since
/// the on-disk `last_event_unix` has already reached `EVAL_WORK_WRITE_
/// THROTTLE_SECS` (or there is no on-disk value yet at all - the very first
/// event this pair has ever seen, which must always be durable or nothing
/// could ever accrue a second time). That last condition is deliberately the
/// RAW gap, not the accrued delta: a gap classified as a PAUSE adds nothing
/// to `accrued_secs`, so an accrued-delta throttle would never force a write
/// for one, leaving `last_event_unix` on disk stuck before the pause - and
/// the very next short, genuinely-worked gap would then be measured against
/// that stale pre-pause instant instead of the real previous event, merging
/// two gaps that should have been judged separately (see `accrue_session_
/// work`'s own doc comment on why `last_event_unix` always moves forward
/// even when a pause adds no work). Measuring the raw gap instead closes
/// that: any gap at least as long as the throttle - work or pause alike -
/// is always written before it can go stale enough to matter, and only a
/// run of gaps each smaller than the throttle itself is ever left to
/// telescope into the next write, which is exact by construction (the sum
/// of gaps each under a bound, none of which individually reached `EVAL_
/// PAUSE_MINUTES`, equals one merged gap that has not reached it either).
pub fn record_hook_event(
    db: &Path,
    project: Option<&str>,
    session_id: &str,
    is_stop: bool,
    now_unix: i64,
    event: HookEventKind,
) -> Option<SessionWorkState> {
    project?;
    let mut all = read_eval_debt_state(db);
    let key = project_key(project).to_string();
    let mut entry = all.get(&key).cloned().unwrap_or_default();
    let before = entry.sessions.get(session_id).cloned().unwrap_or_default();
    let after = accrue_session_work(&before, now_unix, entry.last_evaluation_seen, event);

    let reset_happened = after.anchor_unix != before.anchor_unix;
    let write_gap_elapsed = !before.last_event_unix.is_some_and(|t| now_unix - t < EVAL_WORK_WRITE_THROTTLE_SECS);
    // A counter change (either risk field, not merely the gap arithmetic) is
    // written immediately too, alongside `is_stop`/`reset_happened`/`write_
    // gap_elapsed` above - see this section's own "the repeat's own risk"
    // doc comment. Edits and test/build commands are far rarer than every
    // other tool call a session makes, so this never turns every `PreToolUse`
    // into a write the way dropping the time throttle entirely would; it
    // only means the ONE class of event this whole rewrite is about is never
    // the one left sitting unwritten until some later, unrelated event
    // happens to cross the plain time throttle. Losing an increment here
    // would silently understate the very risk the repeat is gated on.
    let counter_changed = after.edits_since_test != before.edits_since_test || after.compacted_since_anchor != before.compacted_since_anchor;
    if !is_stop && !reset_happened && !write_gap_elapsed && !counter_changed {
        return Some(after);
    }
    entry.sessions.insert(session_id.to_string(), after.clone());
    all.insert(key, entry);
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = std::fs::write(eval_debt_state_path(db), text);
    }
    Some(after)
}

/// The (session id, its own accrual) this project's sidecar last saw a hook
/// event from, by `last_event_unix` - `doctor`'s own cold-read stand-in for
/// "the current session", since it has no session of its own to ask (see
/// `ops::health::judgement_debt_line`'s own doc comment). `None` when the
/// sidecar holds no session at all for this project yet. READ-ONLY, like
/// every other reader of this sidecar `doctor` uses: never called from a
/// real hook.
pub fn most_recently_active_session(state: &ProjectEvalState) -> Option<(&str, &SessionWorkState)> {
    state.sessions.iter().map(|(k, v)| (k.as_str(), v)).max_by_key(|(_, v)| v.last_event_unix.unwrap_or(i64::MIN))
}

/// The pure resolution rule behind `default_eval_command_path` below:
/// Claude Code's per-user commands folder is under whichever of these two
/// candidates is set, USERPROFILE tried first - exactly
/// `ops::install::default_eval_command_path`'s own rule (`home_dir`, that
/// crate). DUPLICATED, not shared: `ops` depends on `serve`, never the other
/// way round (`ops/Cargo.toml` names `serve` as a dependency; the reverse
/// would be a cycle Cargo refuses outright), so the Stop hook here cannot
/// call into `ops` to find this file. Split into a pure function taking the
/// two candidates as plain strings, rather than reading the environment
/// itself, for the same reason `reentry.rs`'s own `depth_from_env_value`
/// is split from its I/O wrapper: `std::env::set_var` is process-wide and
/// races across parallel test threads, so the rule a test needs to drive
/// with every combination of "set"/"unset" has to be pure. `ops/tests` calls
/// this function directly, side by side with `ops::install`'s own copy, to
/// prove neither ever silently drifts from the other.
pub fn eval_command_path_from(userprofile: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    userprofile.or(home).map(|h| PathBuf::from(h).join(".claude").join("commands").join("thor-eval.md"))
}

/// Where the owner's end-of-session evaluation routine lives right now, read
/// from this process' own environment - see `eval_command_path_from` for the
/// pure rule and why this crate carries its own copy of it.
pub fn default_eval_command_path() -> Option<PathBuf> {
    eval_command_path_from(std::env::var("USERPROFILE").ok().as_deref(), std::env::var("HOME").ok().as_deref())
}

#[cfg(test)]
mod eval_debt_predicate_tests {
    use super::*;

    // A fixed reference instant, deliberately at NOON UTC (43_200 seconds
    // past midnight) so "still today" and "a full day earlier" are both
    // obviously correct by eye: anything within 12 hours either side of NOW
    // stays on the same UTC calendar day, and anything a full DAY or more
    // away always crosses into a different one, regardless of what time of
    // day NOW itself happens to be.
    const NOW: i64 = 1_800_014_400;
    const HOUR: i64 = 3600;
    const DAY: i64 = 86400;

    // ------------------------------------------------------- eval_done_today

    #[test]
    fn no_report_ever_seen_is_never_done_today() {
        assert!(!eval_done_today(None, NOW));
    }

    #[test]
    fn a_report_seen_earlier_today_is_done_today() {
        assert!(eval_done_today(Some(NOW - HOUR), NOW), "an hour ago, same UTC day");
        assert!(eval_done_today(Some(NOW), NOW), "this very instant");
    }

    /// Case named in the build brief: a report first seen today silences it
    /// for the rest of the day.
    #[test]
    fn a_report_seen_yesterday_is_not_done_today() {
        assert!(!eval_done_today(Some(NOW - DAY), NOW), "exactly one day earlier is always a different UTC day");
    }

    /// NOW is noon; 23 hours earlier is 13:00 the day before - still
    /// "yesterday" even though less than a full 24 hours have passed. This
    /// is exactly the behaviour the retired rolling-24-hour window did NOT
    /// have (see this file's "evaluation debt" section, fourth rewrite).
    #[test]
    fn a_report_seen_23_hours_ago_that_crossed_midnight_is_not_done_today() {
        assert!(!eval_done_today(Some(NOW - 23 * HOUR), NOW));
    }

    #[test]
    fn a_report_seen_at_a_future_instant_on_a_different_day_is_not_done_today() {
        // Clock skew or a hand-edited sidecar: a future timestamp on a
        // DIFFERENT day must not spuriously agree with "today" either.
        assert!(!eval_done_today(Some(NOW + DAY), NOW));
    }

    // --------------------------------------------------------- eval_debt_owed
    //
    // `edits_since_test`/`compacted_since_anchor` are irrelevant to every
    // test in this block: none of them ever reach the REPEAT branch with
    // enough accrued time for a risk to matter (`eval_done_today` is either
    // `None`/yesterday, or the accrued minutes stay under `EVAL_REPEAT_
    // WORK_MINUTES`), so every call below passes `0, false` - see "eval_
    // debt_owed: the repeat risk" further down for the tests that actually
    // exercise the risk gate.

    /// Case named in the build brief: 59 minutes accrued this session ->
    /// silent, 61 -> fires - no report has ever been seen in either case,
    /// proving the first-work floor alone decides the difference.
    #[test]
    fn fifty_nine_minutes_worked_is_silent_sixty_one_fires() {
        assert!(!eval_debt_owed(59, None, NOW, 0, false), "59 minutes must not yet be enough");
        assert!(eval_debt_owed(61, None, NOW, 0, false), "61 minutes must be enough");
    }

    #[test]
    fn exactly_the_minimum_minutes_worked_is_enough() {
        // The gate is "at least" `EVAL_FIRST_WORK_MINUTES`, not strictly more.
        assert!(eval_debt_owed(EVAL_FIRST_WORK_MINUTES, None, NOW, 0, false));
    }

    #[test]
    fn one_minute_under_the_minimum_is_silent_regardless_of_the_report_history() {
        assert!(!eval_debt_owed(EVAL_FIRST_WORK_MINUTES - 1, Some(NOW - DAY), NOW, 0, false));
    }

    /// Case named in the build brief: a report first seen today silences
    /// the FIRST obligation - but not a repeat once enough more work has
    /// gone by since (see the `repeat_` tests below), so this only holds
    /// for accrued minutes still under `EVAL_REPEAT_WORK_MINUTES`.
    #[test]
    fn a_report_first_seen_today_silences_it() {
        assert!(!eval_debt_owed(EVAL_FIRST_WORK_MINUTES, Some(NOW - HOUR), NOW, 0, false));
    }

    /// Case named in the build brief: a report first seen yesterday does
    /// not silence today's obligation.
    #[test]
    fn a_report_first_seen_yesterday_does_not_silence_today() {
        assert!(eval_debt_owed(EVAL_FIRST_WORK_MINUTES, Some(NOW - DAY), NOW, 0, false));
    }

    #[test]
    fn never_evaluated_at_all_fires_once_enough_time_is_worked() {
        assert!(eval_debt_owed(EVAL_FIRST_WORK_MINUTES, None, NOW, 0, false));
    }

    /// Case named in the build brief: the first evaluation of the day still
    /// fires without any risk - the FIRST-of-day branch carries no risk
    /// condition at all, unchanged by the sixth rewrite. Identical to
    /// `never_evaluated_at_all_fires_once_enough_time_is_worked` above,
    /// named explicitly for this build brief's own case.
    #[test]
    fn the_first_evaluation_of_the_day_still_fires_without_any_risk() {
        assert!(eval_debt_owed(EVAL_FIRST_WORK_MINUTES, None, NOW, 0, false));
    }

    // ------------------------------------------- eval_debt_owed: the repeat

    /// Case named in the build brief ("with a report today 179 minutes
    /// silent and 181 fires"): once a report already exists for today, the
    /// FIRST-work threshold no longer applies at all - only the much larger
    /// repeat threshold does, measured from whatever reset the report
    /// itself caused. A qualifying risk is held constant across both calls,
    /// so this stays a pure test of the TIME boundary - see "the repeat
    /// risk" block below for the risk boundary itself.
    #[test]
    fn with_a_report_today_179_minutes_is_silent_181_fires() {
        let seen_today = Some(NOW - HOUR);
        assert!(
            !eval_debt_owed(179, seen_today, NOW, EVAL_REPEAT_MIN_UNTESTED_EDITS, false),
            "179 minutes must not yet be enough for a repeat, even with a risk already present"
        );
        assert!(
            eval_debt_owed(181, seen_today, NOW, EVAL_REPEAT_MIN_UNTESTED_EDITS, false),
            "181 minutes must be enough for a repeat, with a risk present"
        );
    }

    #[test]
    fn with_a_report_today_exactly_the_repeat_minimum_is_enough() {
        assert!(eval_debt_owed(EVAL_REPEAT_WORK_MINUTES, Some(NOW - HOUR), NOW, 0, true));
    }

    #[test]
    fn with_a_report_today_well_past_the_first_threshold_but_under_the_repeat_one_stays_silent() {
        // Case named in the build brief: a report seen today, 90 minutes
        // accrued since - well past `EVAL_FIRST_WORK_MINUTES`, but nowhere
        // near `EVAL_REPEAT_WORK_MINUTES` - must stay silent. The whole
        // point of the fifth rewrite: a report already seen today no longer
        // reads the FIRST threshold at all. A risk is present in the fixture
        // (both forms at once) to prove it is really the TIME gate stopping
        // this, never the risk gate.
        assert!(!eval_debt_owed(90, Some(NOW - HOUR), NOW, 10, true));
    }

    #[test]
    fn with_no_report_today_the_repeat_threshold_never_applies() {
        // Without `eval_done_today`, `EVAL_REPEAT_WORK_MINUTES` (180) worth
        // of accrued work is still just "well past the first threshold",
        // and must fire exactly as any other first-of-day case would - no
        // risk needed, since this never reaches the repeat branch at all.
        assert!(eval_debt_owed(EVAL_REPEAT_WORK_MINUTES, None, NOW, 0, false));
    }

    // ------------------------------------- eval_debt_owed: the repeat's risk
    //
    // THE SIXTH REWRITE ITSELF (2026-09-17, owner's decision - see this
    // file's own "the repeat's own risk" doc comment): accrued time reaching
    // `EVAL_REPEAT_WORK_MINUTES` is no longer enough on its own for a repeat
    // - a real risk has to hold too. Every case here holds `seen_today` and
    // the accrued minutes fixed at exactly the values that would have fired
    // unconditionally before this rewrite, varying only the two risk
    // parameters, so each test isolates the risk gate alone.

    #[test]
    fn case_181_minutes_with_two_untested_edits_and_no_summary_stays_silent() {
        // Case named in the build brief.
        assert!(!eval_debt_owed(181, Some(NOW - HOUR), NOW, 2, false), "two untested edits is under the risk floor");
    }

    #[test]
    fn case_181_minutes_with_three_untested_edits_fires() {
        // Case named in the build brief.
        assert!(eval_debt_owed(181, Some(NOW - HOUR), NOW, 3, false), "three untested edits reaches the risk floor");
    }

    #[test]
    fn case_181_minutes_with_a_summary_and_zero_edits_fires() {
        // Case named in the build brief.
        assert!(eval_debt_owed(181, Some(NOW - HOUR), NOW, 0, true), "a context summary is a risk on its own, with no edits at all");
    }

    #[test]
    fn case_181_minutes_with_neither_edits_nor_a_summary_stays_silent() {
        // THE CENTRAL CASE THIS REWRITE EXISTS FOR: accrued time alone, with
        // no risk of either kind, must never fire a repeat any more.
        assert!(!eval_debt_owed(181, Some(NOW - HOUR), NOW, 0, false), "time alone must never fire a repeat");
    }

    #[test]
    fn exactly_the_minimum_untested_edits_is_enough() {
        // The risk gate is "at least" `EVAL_REPEAT_MIN_UNTESTED_EDITS`, not
        // strictly more.
        assert!(eval_debt_owed(EVAL_REPEAT_WORK_MINUTES, Some(NOW - HOUR), NOW, EVAL_REPEAT_MIN_UNTESTED_EDITS, false));
    }

    #[test]
    fn one_untested_edit_under_the_minimum_with_no_summary_is_silent() {
        assert!(!eval_debt_owed(EVAL_REPEAT_WORK_MINUTES, Some(NOW - HOUR), NOW, EVAL_REPEAT_MIN_UNTESTED_EDITS - 1, false));
    }

    #[test]
    fn both_risks_at_once_still_fires() {
        assert!(eval_debt_owed(EVAL_REPEAT_WORK_MINUTES, Some(NOW - HOUR), NOW, EVAL_REPEAT_MIN_UNTESTED_EDITS, true));
    }

    #[test]
    fn days_ago_floors_and_never_goes_negative() {
        assert_eq!(days_ago(NOW, NOW - 3 * DAY), 3);
        assert_eq!(days_ago(NOW, NOW - 3 * DAY - 1), 3, "not yet a fourth full day");
        assert_eq!(days_ago(NOW, NOW + HOUR), 0, "a future timestamp reads as 0, never negative");
    }

    #[test]
    fn minutes_ago_floors_and_never_goes_negative() {
        assert_eq!(minutes_ago(NOW, NOW - 3 * 60), 3);
        assert_eq!(minutes_ago(NOW, NOW - 3 * 60 - 1), 3, "not yet a fourth full minute");
        assert_eq!(minutes_ago(NOW, NOW + 60), 0, "a future timestamp reads as 0, never negative");
    }
}

/// `accrue_session_work`/`session_work_reset_needed` (the pure gap/pause/
/// reset rule) and `record_hook_event`/`most_recently_active_session` (its
/// I/O wrapper and doctor's own cold read) - see `usefulness`'s own
/// "evaluation debt" section, fifth rewrite, for the whole story.
#[cfg(test)]
mod session_work_tests {
    use super::*;

    // The identical fixed noon-UTC instant `eval_debt_predicate_tests` uses,
    // for the identical reason: anything within 12 hours either side stays
    // on the same UTC calendar day by eye.
    const NOW: i64 = 1_800_014_400;
    const MIN: i64 = 60;

    // --------------------------------------------- accrue_session_work: gaps

    /// Case named in the build brief: gaps under 30 minutes accrue.
    #[test]
    fn a_gap_under_the_pause_threshold_accrues_in_full() {
        let session = SessionWorkState { anchor_unix: Some(NOW - 10 * MIN), accrued_secs: 5 * MIN, last_event_unix: Some(NOW - 10 * MIN), ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::Other);
        assert_eq!(after.accrued_secs, 5 * MIN + 10 * MIN, "the whole 10-minute gap must be added");
        assert_eq!(after.anchor_unix, session.anchor_unix, "no reset: the anchor must not move");
        assert_eq!(after.last_event_unix, Some(NOW));
    }

    /// Case named in the build brief: a 30-minute gap adds nothing.
    #[test]
    fn a_gap_at_the_pause_threshold_adds_nothing() {
        let session = SessionWorkState {
            anchor_unix: Some(NOW - 40 * MIN),
            accrued_secs: 5 * MIN,
            last_event_unix: Some(NOW - EVAL_PAUSE_MINUTES * MIN),
            ..Default::default()
        };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::Other);
        assert_eq!(after.accrued_secs, 5 * MIN, "exactly 30 minutes is already a pause, not work");
        assert_eq!(after.last_event_unix, Some(NOW), "the clock still moves forward, even though nothing accrued");
    }

    #[test]
    fn a_gap_just_under_the_pause_threshold_still_accrues_in_full() {
        let session = SessionWorkState {
            anchor_unix: Some(NOW - 40 * MIN),
            accrued_secs: 0,
            last_event_unix: Some(NOW - (EVAL_PAUSE_MINUTES * 60 - 1)),
            ..Default::default()
        };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::Other);
        assert_eq!(after.accrued_secs, EVAL_PAUSE_MINUTES * 60 - 1, "one second under the threshold must still count in full");
    }

    #[test]
    fn a_negative_gap_from_clock_skew_never_subtracts() {
        let session = SessionWorkState { anchor_unix: Some(NOW), accrued_secs: 100, last_event_unix: Some(NOW + MIN), ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::Other);
        assert_eq!(after.accrued_secs, 100, "a clock that appears to have gone backwards must add zero, never go negative");
    }

    // ---------------------------------------- session_work_reset_needed / resets

    #[test]
    fn the_very_first_event_ever_is_a_reset_to_zero() {
        let after = accrue_session_work(&SessionWorkState::default(), NOW, None, HookEventKind::Other);
        assert_eq!(after, SessionWorkState { anchor_unix: Some(NOW), accrued_secs: 0, last_event_unix: Some(NOW), ..Default::default() });
    }

    /// Case named in the build brief: a new UTC day resets it.
    #[test]
    fn a_new_utc_day_resets_the_anchor_and_the_work() {
        // One second before today's midnight - "yesterday" no matter the
        // wall-clock hour NOW itself happens to represent.
        let yesterday = NOW - NOW.rem_euclid(86400) - 1;
        let session = SessionWorkState { anchor_unix: Some(yesterday), accrued_secs: 9_000, last_event_unix: Some(yesterday), ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::Other);
        assert_eq!(after.accrued_secs, 0, "crossing into a new UTC day must drop whatever was accrued");
        assert_eq!(after.anchor_unix, Some(NOW), "and restart the anchor at the event that crossed the boundary");
    }

    #[test]
    fn staying_within_the_same_utc_day_never_resets_on_its_own() {
        let session = SessionWorkState { anchor_unix: Some(NOW - 5 * MIN), accrued_secs: 100, last_event_unix: Some(NOW - MIN), ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::Other);
        assert_eq!(after.anchor_unix, session.anchor_unix, "the same calendar day must never reset on its own");
    }

    /// Case named in the build brief: a new report resets the anchor and the
    /// work.
    #[test]
    fn a_report_seen_after_the_anchor_resets_the_anchor_and_the_work() {
        let session = SessionWorkState { anchor_unix: Some(NOW - 60 * MIN), accrued_secs: 3_000, last_event_unix: Some(NOW - MIN), ..Default::default() };
        let after = accrue_session_work(&session, NOW, Some(NOW - 30 * MIN), HookEventKind::Other);
        assert_eq!(after.accrued_secs, 0, "a report seen since this accrual period began must drop the total");
        assert_eq!(after.anchor_unix, Some(NOW));
    }

    #[test]
    fn a_report_seen_before_the_anchor_never_resets_it_again() {
        // Already accounted for by an earlier reset - a report seen BEFORE
        // this accrual period began must not keep re-triggering one forever.
        let session = SessionWorkState { anchor_unix: Some(NOW - 60 * MIN), accrued_secs: 100, last_event_unix: Some(NOW - MIN), ..Default::default() };
        let after = accrue_session_work(&session, NOW, Some(NOW - 90 * MIN), HookEventKind::Other);
        assert_eq!(after.anchor_unix, session.anchor_unix, "a report older than the anchor is old news, not a new reset");
        assert_eq!(after.accrued_secs, 100 + MIN, "the ordinary one-minute gap must still accrue normally");
    }

    #[test]
    fn a_report_seen_at_exactly_the_anchor_instant_never_resets_it_again() {
        // Strictly later, per `session_work_reset_needed`'s own doc comment:
        // a report seen at the exact instant the anchor was set is the
        // report that CAUSED this reset, not a new one on top of it.
        let session = SessionWorkState { anchor_unix: Some(NOW - 60 * MIN), accrued_secs: 0, last_event_unix: Some(NOW - 60 * MIN), ..Default::default() };
        let after = accrue_session_work(&session, NOW, Some(NOW - 60 * MIN), HookEventKind::Other);
        assert_eq!(after.anchor_unix, session.anchor_unix);
    }

    // ------------------------------------ accrue_session_work: the two risks
    //
    // `HookEventKind`'s own effect on `edits_since_test`/`compacted_since_
    // anchor` - see this file's own "the repeat's own risk" doc comment.

    #[test]
    fn an_untested_edit_adds_one_to_the_edit_count() {
        let session = SessionWorkState { anchor_unix: Some(NOW - MIN), last_event_unix: Some(NOW - MIN), edits_since_test: 2, ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::UntestedEdit);
        assert_eq!(after.edits_since_test, 3);
    }

    /// Case named in the build brief: a test command resets the edit count.
    #[test]
    fn a_test_run_resets_the_edit_count_to_zero() {
        let session = SessionWorkState { anchor_unix: Some(NOW - MIN), last_event_unix: Some(NOW - MIN), edits_since_test: 5, ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::TestRun);
        assert_eq!(after.edits_since_test, 0);
    }

    /// Case named in the build brief: a .md edit does not count - proven
    /// here at the accrual level too (`classify_hook_event_tests` already
    /// proves the classification itself never produces `UntestedEdit` for
    /// one): an `Other`-classified event must leave the edit count exactly
    /// as it was.
    #[test]
    fn an_other_event_never_moves_the_edit_count() {
        let session = SessionWorkState { anchor_unix: Some(NOW - MIN), last_event_unix: Some(NOW - MIN), edits_since_test: 2, ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::Other);
        assert_eq!(after.edits_since_test, 2);
    }

    #[test]
    fn a_compact_start_sets_the_summary_flag_and_it_stays_sticky() {
        let session = SessionWorkState { anchor_unix: Some(NOW - MIN), last_event_unix: Some(NOW - MIN), ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::CompactStart);
        assert!(after.compacted_since_anchor);
        // Sticky: a LATER ordinary event must not clear it again.
        let later = accrue_session_work(&after, NOW + MIN, None, HookEventKind::Other);
        assert!(later.compacted_since_anchor, "a compaction earlier in the same anchor period is still a real risk");
    }

    /// Case named in the build brief: a new UTC day resets both (the edit
    /// count and the summary flag), the same as it already resets
    /// `accrued_secs`.
    #[test]
    fn a_new_utc_day_resets_the_edit_count_and_the_summary_flag_too() {
        let yesterday = NOW - NOW.rem_euclid(86400) - 1;
        let session = SessionWorkState {
            anchor_unix: Some(yesterday),
            last_event_unix: Some(yesterday),
            edits_since_test: 5,
            compacted_since_anchor: true,
            ..Default::default()
        };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::Other);
        assert_eq!(after.edits_since_test, 0, "a new UTC day must drop the untested-edit count too");
        assert!(!after.compacted_since_anchor, "and the summary flag too");
    }

    /// Case named in the build brief: a new report resets both.
    #[test]
    fn a_report_seen_after_the_anchor_resets_the_edit_count_and_the_summary_flag_too() {
        let session = SessionWorkState {
            anchor_unix: Some(NOW - 60 * MIN),
            last_event_unix: Some(NOW - MIN),
            edits_since_test: 4,
            compacted_since_anchor: true,
            ..Default::default()
        };
        let after = accrue_session_work(&session, NOW, Some(NOW - 30 * MIN), HookEventKind::Other);
        assert_eq!(after.edits_since_test, 0);
        assert!(!after.compacted_since_anchor);
    }

    /// A reset that coincides with the very event that would otherwise have
    /// counted still seeds the fresh period with it - the new period's own
    /// first edit still counts as one, it is only what came BEFORE the reset
    /// that is discarded.
    #[test]
    fn a_reset_still_counts_its_own_triggering_event() {
        let yesterday = NOW - NOW.rem_euclid(86400) - 1;
        let session = SessionWorkState { anchor_unix: Some(yesterday), last_event_unix: Some(yesterday), edits_since_test: 5, ..Default::default() };
        let after = accrue_session_work(&session, NOW, None, HookEventKind::UntestedEdit);
        assert_eq!(after.edits_since_test, 1, "the old count is discarded, but this event's own edit still seeds the new period");
    }

    // ----------------------------------------------------- record_hook_event

    fn read_sidecar_session(db: &Path, project: Option<&str>, session_id: &str) -> SessionWorkState {
        project_eval_state(db, project).sessions.get(session_id).cloned().unwrap_or_default()
    }

    #[test]
    fn a_no_project_checkout_is_never_recorded_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        assert_eq!(record_hook_event(&db, None, "s1", false, NOW, HookEventKind::Other), None);
        assert!(!eval_debt_state_path(&db).exists(), "a no-project event must never even create the sidecar");
    }

    #[test]
    fn the_very_first_event_is_always_written_even_though_it_adds_no_accrued_time() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let result = record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::Other).expect("a real project must record");
        assert_eq!(result, SessionWorkState { anchor_unix: Some(NOW), accrued_secs: 0, last_event_unix: Some(NOW), ..Default::default() });
        assert_eq!(read_sidecar_session(&db, Some("thor"), "s1"), result, "must be durable on disk immediately");
    }

    /// Case named in the build brief: the write throttle holds (no write on
    /// a PreToolUse that adds under 60 seconds).
    #[test]
    fn a_small_gap_on_a_non_stop_event_is_computed_but_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::Other).unwrap();
        let sidecar = eval_debt_state_path(&db);
        let before = std::fs::read_to_string(&sidecar).unwrap();

        let result = record_hook_event(&db, Some("thor"), "s1", false, NOW + 5, HookEventKind::Other).expect("still a real project");
        assert_eq!(result.accrued_secs, 5, "the small gap must still be reflected in the value handed back");

        let after = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(before, after, "a 5-second gap on a non-Stop event must never be written to disk");
    }

    #[test]
    fn a_gap_reaching_the_write_throttle_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::Other).unwrap();
        record_hook_event(&db, Some("thor"), "s1", false, NOW + EVAL_WORK_WRITE_THROTTLE_SECS, HookEventKind::Other).unwrap();
        assert_eq!(read_sidecar_session(&db, Some("thor"), "s1").accrued_secs, EVAL_WORK_WRITE_THROTTLE_SECS);
    }

    #[test]
    fn a_stop_event_always_writes_even_for_a_tiny_gap() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::Other).unwrap();
        let result = record_hook_event(&db, Some("thor"), "s1", true, NOW + 2, HookEventKind::Other).unwrap();
        assert_eq!(read_sidecar_session(&db, Some("thor"), "s1"), result, "a Stop must always be durable immediately");
    }

    /// Proves the correctness argument in `record_hook_event`'s own doc
    /// comment on the write throttle: a pause must be written immediately
    /// even though it adds no accrued work, or the NEXT short gap would be
    /// measured against a stale pre-pause instant and wrongly read as
    /// another pause.
    #[test]
    fn a_pause_boundary_is_always_written_so_the_next_short_gap_is_not_merged_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::Other).unwrap();
        let after_pause = NOW + (EVAL_PAUSE_MINUTES + 5) * 60;
        let result = record_hook_event(&db, Some("thor"), "s1", false, after_pause, HookEventKind::Other).unwrap();
        assert_eq!(result.accrued_secs, 0, "fixture sanity: the pause itself must add nothing");
        assert_eq!(
            read_sidecar_session(&db, Some("thor"), "s1").last_event_unix,
            Some(after_pause),
            "the pause boundary must be written so a later short gap is measured from here, not from before the pause"
        );

        let resumed = after_pause + 30;
        let result = record_hook_event(&db, Some("thor"), "s1", true, resumed, HookEventKind::Other).unwrap();
        assert_eq!(result.accrued_secs, 30, "must be measured from the pause boundary, not telescoped with the pause itself");
    }

    #[test]
    fn two_sessions_in_the_same_project_accrue_independently() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::Other).unwrap();
        record_hook_event(&db, Some("thor"), "s2", false, NOW, HookEventKind::Other).unwrap();
        record_hook_event(&db, Some("thor"), "s1", true, NOW + 10 * MIN, HookEventKind::Other).unwrap();
        assert_eq!(read_sidecar_session(&db, Some("thor"), "s1").accrued_secs, 10 * MIN);
        assert_eq!(
            read_sidecar_session(&db, Some("thor"), "s2").accrued_secs,
            0,
            "a different session in the same project must never see the other one's accrual"
        );
    }

    /// Case named in the build brief: old sidecars parse.
    #[test]
    fn a_sidecar_written_before_sessions_existed_still_parses_and_is_still_usable() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let text = r#"{"thor":{"tracking_since":1234,"known_report_ids":[],"last_evaluation_seen":null,"last_evaluation_report_id":null,"asked_count":0,"first_asked_since_report":null}}"#;
        std::fs::write(eval_debt_state_path(&db), text).unwrap();
        let state = project_eval_state(&db, Some("thor"));
        assert_eq!(state.tracking_since, Some(1234), "fixture sanity: the rest of the entry still reads back");
        assert!(state.sessions.is_empty(), "a sessions field that never existed must default to an empty map");

        assert!(
            record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::Other).is_some(),
            "must still be usable, not choke on the old shape"
        );
    }

    /// Case named in the build brief: old sidecars parse - the narrower
    /// claim, for a sidecar written after `sessions` existed but before the
    /// two risk fields did.
    #[test]
    fn a_sidecar_session_written_before_the_risk_fields_existed_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let text = r#"{"thor":{"tracking_since":1234,"known_report_ids":[],"last_evaluation_seen":null,"last_evaluation_report_id":null,"asked_count":0,"first_asked_since_report":null,"sessions":{"s1":{"anchor_unix":1000,"accrued_secs":60,"last_event_unix":1000}}}}"#;
        std::fs::write(eval_debt_state_path(&db), text).unwrap();
        let session = read_sidecar_session(&db, Some("thor"), "s1");
        assert_eq!(session.accrued_secs, 60, "fixture sanity: the rest of the session entry still reads back");
        assert_eq!(session.edits_since_test, 0, "a session written before this field existed must default to zero");
        assert!(!session.compacted_since_anchor, "and the summary flag must default to false");
    }

    /// Case named in the build brief: a counter change is written
    /// immediately - unlike a plain time-only gap under the write throttle
    /// (`a_small_gap_on_a_non_stop_event_is_computed_but_not_written` above),
    /// an untested edit must never wait for the throttle to catch up.
    #[test]
    fn an_untested_edit_is_written_immediately_even_under_the_write_throttle() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::Other).unwrap();
        let sidecar = eval_debt_state_path(&db);
        let before = std::fs::read_to_string(&sidecar).unwrap();

        record_hook_event(&db, Some("thor"), "s1", false, NOW + 5, HookEventKind::UntestedEdit).unwrap();
        let after = std::fs::read_to_string(&sidecar).unwrap();
        assert_ne!(before, after, "an edit that changes the untested-edit counter must be written immediately, even under the time throttle");
        assert_eq!(read_sidecar_session(&db, Some("thor"), "s1").edits_since_test, 1);
    }

    /// The identical proof for a test/build run resetting the counter back
    /// to zero - also a counter change, also written immediately.
    #[test]
    fn a_test_run_reset_is_written_immediately_even_under_the_write_throttle() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_hook_event(&db, Some("thor"), "s1", false, NOW, HookEventKind::UntestedEdit).unwrap();
        record_hook_event(&db, Some("thor"), "s1", false, NOW + 1, HookEventKind::UntestedEdit).unwrap();
        assert_eq!(read_sidecar_session(&db, Some("thor"), "s1").edits_since_test, 2, "fixture sanity");
        let sidecar = eval_debt_state_path(&db);
        let before = std::fs::read_to_string(&sidecar).unwrap();

        record_hook_event(&db, Some("thor"), "s1", false, NOW + 3, HookEventKind::TestRun).unwrap();
        let after = std::fs::read_to_string(&sidecar).unwrap();
        assert_ne!(before, after, "a test/build run resetting the counter must be written immediately too");
        assert_eq!(read_sidecar_session(&db, Some("thor"), "s1").edits_since_test, 0);
    }

    // ------------------------------------------- most_recently_active_session

    #[test]
    fn most_recently_active_session_is_none_for_an_empty_project() {
        assert_eq!(most_recently_active_session(&ProjectEvalState::default()), None);
    }

    #[test]
    fn most_recently_active_session_picks_the_latest_last_event() {
        let mut state = ProjectEvalState::default();
        state.sessions.insert(
            "older".to_string(),
            SessionWorkState { anchor_unix: Some(NOW), accrued_secs: 500, last_event_unix: Some(NOW), ..Default::default() },
        );
        state.sessions.insert(
            "newer".to_string(),
            SessionWorkState { anchor_unix: Some(NOW), accrued_secs: 10, last_event_unix: Some(NOW + MIN), ..Default::default() },
        );
        let (id, work) = most_recently_active_session(&state).expect("must find one");
        assert_eq!(id, "newer");
        assert_eq!(work.accrued_secs, 10);
    }
}

#[cfg(test)]
mod eval_debt_state_tests {
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

    /// A live evaluation-report Report, correctly tagged and scoped - ground
    /// 21 (`model::gate`) refuses a Report with no project at all except one
    /// exempt id, so every fixture Report here needs a real project.
    fn declare_report(store: &mut EventStore, id: &str, project: &str, tags: &[&str]) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: format!("fixture evaluation report {id}"),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
    }

    fn owe(store: &mut EventStore, n: usize, project: Option<&str>) {
        for i in 0..n {
            let id = format!("owed-{}-{i}", project.unwrap_or("global"));
            declare(store, &id, project);
            serve_n(store, &id, JUDGEMENT_DEBT_AFTER);
        }
    }

    // ----------------------------------------------------------- round trip

    #[test]
    fn a_missing_sidecar_reads_as_the_all_default_state() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        assert_eq!(project_eval_state(&db, Some("thor")), ProjectEvalState::default());
    }

    #[test]
    fn a_corrupt_sidecar_reads_as_the_all_default_state_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        std::fs::write(eval_debt_state_path(&db), "{ this is not json").unwrap();
        assert_eq!(project_eval_state(&db, Some("thor")), ProjectEvalState::default());
    }

    #[test]
    fn a_written_state_reads_back_identical() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();
        let written = update_eval_debt_state(&store, &db, None, 1_800_000_000);
        let read_back = project_eval_state(&db, None);
        assert_eq!(written, read_back);
        assert_eq!(read_back.tracking_since, Some(1_800_000_000));
    }

    /// UNLIKE THE OLD CEILING-GATED WRITE: `tracking_since` is set the very
    /// first time this function ever runs for a project, regardless of how
    /// large - or how small, even zero - its backlog is, so the sidecar is
    /// written on this very first call rather than held back until some
    /// later crossing.
    #[test]
    fn the_first_call_for_a_project_always_writes_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();
        update_eval_debt_state(&store, &db, None, 1_000);
        assert!(eval_debt_state_path(&db).exists(), "tracking_since is set on the very first call, so the sidecar must exist");
    }

    #[test]
    fn a_second_call_with_nothing_new_never_rewrites_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();
        update_eval_debt_state(&store, &db, None, 1_000);
        let path = eval_debt_state_path(&db);
        let written_once = std::fs::read_to_string(&path).unwrap();
        update_eval_debt_state(&store, &db, None, 2_000);
        let written_twice = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written_once, written_twice,
            "nothing changed (tracking_since stays, no new report), so the file must stay byte-identical"
        );
    }

    #[test]
    fn two_projects_keep_separate_entries_in_one_sidecar_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();
        update_eval_debt_state(&store, &db, Some("thor"), 1_000);
        update_eval_debt_state(&store, &db, Some("acme"), 2_000);
        assert_eq!(project_eval_state(&db, Some("thor")).tracking_since, Some(1_000));
        assert_eq!(
            project_eval_state(&db, Some("acme")).tracking_since,
            Some(2_000),
            "each project's own tracking clock starts at its own first Stop, independent of the other"
        );
    }

    // ------------------------------------------------------- tracking_since

    #[test]
    fn tracking_since_is_set_on_the_first_stop_and_never_moves_after() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();
        let first = update_eval_debt_state(&store, &db, None, 1_000);
        assert_eq!(first.tracking_since, Some(1_000));
        let second = update_eval_debt_state(&store, &db, None, 50_000);
        assert_eq!(second.tracking_since, Some(1_000), "the clock must not restart on a later Stop in the same project");
    }

    /// Case named in the build brief: a low debt never clears the clock -
    /// the defect the old ceiling-gated version had by design (falling back
    /// under the ceiling cleared `over_ceiling_since` outright). `owe` below
    /// crosses what used to be the ceiling, and a verdict then drops the
    /// count back to zero; `tracking_since` must not move either time.
    #[test]
    fn a_low_or_falling_debt_never_clears_tracking_since() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let first = update_eval_debt_state(&store, &db, None, 1_000);
        assert_eq!(first.tracking_since, Some(1_000), "fixture sanity: tracking started");
        owe(&mut store, 20, None);
        let with_backlog = update_eval_debt_state(&store, &db, None, 2_000);
        assert_eq!(with_backlog.tracking_since, Some(1_000), "a real backlog appearing must not move the clock either");
        crate::mark::record_useful(&mut store, "s", "s", "t", "2026-09-08T00:00:00Z", "owed-global-0").unwrap();
        let dropped = update_eval_debt_state(&store, &db, None, 3_000);
        assert_eq!(dropped.tracking_since, Some(1_000), "a verdict that drops the backlog back down must not clear the clock");
    }

    /// Case named in the build brief: an old sidecar carries over as the
    /// clock. A sidecar written before 2026-09-16, carrying the field under
    /// its old name, must still supply the same instant as `tracking_since`
    /// - the whole point of the `serde(alias)` on that field: the clock the
    /// owner's machine already started keeps running under the new name
    /// instead of silently resetting to `None` on the first read after the
    /// upgrade.
    ///
    /// ALSO PROVES the same for `asked_count`/`first_asked_since_report`
    /// (added by the fourth rewrite, this file's own "evaluation debt"
    /// section) the OTHER direction: a sidecar entry written before those
    /// two fields existed at all - this exact JSON shape, with neither key
    /// present - must still parse, reading them as their all-default
    /// "nothing asked yet" values rather than a parse failure.
    #[test]
    fn an_old_sidecar_field_name_carries_over_as_tracking_since() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let text = r#"{"thor":{"over_ceiling_since":1234,"known_report_ids":[],"last_evaluation_seen":null,"last_evaluation_report_id":null}}"#;
        std::fs::write(eval_debt_state_path(&db), text).unwrap();
        let state = project_eval_state(&db, Some("thor"));
        assert_eq!(state.tracking_since, Some(1234), "the old field name must still populate tracking_since");
        assert_eq!(state.asked_count, 0, "a sidecar written before asked_count existed must default it to zero");
        assert_eq!(
            state.first_asked_since_report, None,
            "a sidecar written before first_asked_since_report existed must default it to None"
        );
    }

    // --------------------------------------------------- evaluation reports

    #[test]
    fn a_new_live_evaluation_report_stamps_last_evaluation_seen_and_names_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        declare_report(&mut store, "eval-thor-2026-09-01", "thor", &["evaluation-report"]);
        let state = update_eval_debt_state(&store, &db, Some("thor"), 5_000);
        assert_eq!(state.last_evaluation_seen, Some(5_000));
        assert_eq!(state.last_evaluation_report_id.as_deref(), Some("eval-thor-2026-09-01"));
        assert!(state.known_report_ids.contains("eval-thor-2026-09-01"));
    }

    #[test]
    fn the_same_report_seen_again_never_restamps_last_evaluation_seen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        declare_report(&mut store, "eval-thor-2026-09-01", "thor", &["evaluation-report"]);
        update_eval_debt_state(&store, &db, Some("thor"), 5_000);
        let again = update_eval_debt_state(&store, &db, Some("thor"), 9_000);
        assert_eq!(again.last_evaluation_seen, Some(5_000), "the same, already-known report must not restamp the clock");
    }

    #[test]
    fn a_report_missing_the_tag_is_never_counted() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        declare_report(&mut store, "eval-thor-2026-09-01", "thor", &["some-other-tag"]);
        let state = update_eval_debt_state(&store, &db, Some("thor"), 5_000);
        assert_eq!(state.last_evaluation_seen, None);
    }

    #[test]
    fn a_report_filed_for_a_different_project_never_silences_this_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        declare_report(&mut store, "eval-acme-2026-09-01", "acme", &["evaluation-report"]);
        let state = update_eval_debt_state(&store, &db, Some("thor"), 5_000);
        assert_eq!(state.last_evaluation_seen, None, "acme's own report must not silence thor's debt");
    }

    // ------------------------------------------------------- the ask counter
    //
    // `record_eval_debt_asked` (called only when the Stop hook actually
    // fires, never on every Stop the way `update_eval_debt_state` above is)
    // and its own reset, added 2026-09-16 (fourth rewrite, this file's own
    // "evaluation debt" section) to replace the retired once-per-session
    // sidecar. Case named in the build brief: "the ask counter increments
    // per blocked turn and resets when a report is seen".

    #[test]
    fn record_eval_debt_asked_increments_the_count_every_call() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_eval_debt_asked(&db, Some("thor"), 1_000);
        assert_eq!(project_eval_state(&db, Some("thor")).asked_count, 1);
        record_eval_debt_asked(&db, Some("thor"), 2_000);
        assert_eq!(project_eval_state(&db, Some("thor")).asked_count, 2);
        record_eval_debt_asked(&db, Some("thor"), 3_000);
        assert_eq!(project_eval_state(&db, Some("thor")).asked_count, 3, "every call is a real, new ask");
    }

    #[test]
    fn record_eval_debt_asked_stamps_first_asked_since_report_only_on_the_first_call() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_eval_debt_asked(&db, Some("thor"), 1_000);
        assert_eq!(project_eval_state(&db, Some("thor")).first_asked_since_report, Some(1_000));
        record_eval_debt_asked(&db, Some("thor"), 9_000);
        assert_eq!(
            project_eval_state(&db, Some("thor")).first_asked_since_report,
            Some(1_000),
            "a later ask must never move the first-asked clock"
        );
    }

    #[test]
    fn two_projects_keep_separate_ask_counts() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_eval_debt_asked(&db, Some("thor"), 1_000);
        record_eval_debt_asked(&db, Some("thor"), 2_000);
        record_eval_debt_asked(&db, Some("acme"), 5_000);
        assert_eq!(project_eval_state(&db, Some("thor")).asked_count, 2);
        assert_eq!(project_eval_state(&db, Some("acme")).asked_count, 1, "a different project's own count is independent");
    }

    /// THE RESET: a new evaluation report silences not only `last_
    /// evaluation_seen` (already proven above) but also this project's own
    /// ask counter and first-asked clock, in the exact same Stop that first
    /// sees it - the moment a report answers every unanswered ask since the
    /// last one.
    #[test]
    fn a_new_live_evaluation_report_resets_the_ask_counter_and_first_asked_clock() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        record_eval_debt_asked(&db, Some("thor"), 1_000);
        record_eval_debt_asked(&db, Some("thor"), 2_000);
        record_eval_debt_asked(&db, Some("thor"), 3_000);
        let before = project_eval_state(&db, Some("thor"));
        assert_eq!(before.asked_count, 3, "fixture sanity: three unanswered asks so far");
        assert_eq!(before.first_asked_since_report, Some(1_000), "fixture sanity");

        let mut store = EventStore::new(&db).unwrap();
        declare_report(&mut store, "eval-thor-2026-09-16", "thor", &["evaluation-report"]);
        let after = update_eval_debt_state(&store, &db, Some("thor"), 9_000);
        assert_eq!(after.asked_count, 0, "a newly seen report must reset the ask counter to zero");
        assert_eq!(after.first_asked_since_report, None, "and clear the first-asked clock, nothing is unanswered any more");
    }

    /// A second Stop that sees NOTHING new (no new report) must leave an
    /// already-nonzero ask counter exactly as it was - `update_eval_debt_
    /// state` only ever resets it as a SIDE EFFECT of a new report, never on
    /// its own as a general "nothing changed" cleanup.
    #[test]
    fn a_stop_with_no_new_report_never_resets_an_existing_ask_count() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();
        record_eval_debt_asked(&db, Some("thor"), 1_000);
        record_eval_debt_asked(&db, Some("thor"), 2_000);
        update_eval_debt_state(&store, &db, Some("thor"), 3_000);
        let state = project_eval_state(&db, Some("thor"));
        assert_eq!(state.asked_count, 2, "no new report was seen, so the count must stay exactly as it was");
        assert_eq!(state.first_asked_since_report, Some(1_000));
    }
}

#[cfg(test)]
mod eval_command_path_tests {
    use super::*;

    #[test]
    fn userprofile_wins_when_both_are_set() {
        assert_eq!(
            eval_command_path_from(Some("C:\\Users\\fixture"), Some("/home/fixture")),
            Some(PathBuf::from("C:\\Users\\fixture").join(".claude").join("commands").join("thor-eval.md"))
        );
    }

    #[test]
    fn home_is_the_fallback_when_userprofile_is_absent() {
        assert_eq!(
            eval_command_path_from(None, Some("/home/fixture")),
            Some(PathBuf::from("/home/fixture").join(".claude").join("commands").join("thor-eval.md"))
        );
    }

    #[test]
    fn neither_set_resolves_to_nothing() {
        assert_eq!(eval_command_path_from(None, None), None);
    }
}
