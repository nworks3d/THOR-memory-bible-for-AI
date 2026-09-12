//! The query side of "was this item ever marked useful" - a plain fold over
//! `ItemMarkedUseful` events in the log, the same shape `serving::serving_stats`
//! already uses for `ItemServed`. Existence only: decay
//! (`crate::decay::DecayContext`) only ever asks "ever, yes or no", never "how
//! many times" - a mark is a permanent, one-way cancellation of decay for that
//! item, not a score.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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

    /// The evaluation debt's own owed count is `judgement_debt_counts`'s
    /// `in_project` half (see `eval_debt_owed`'s own doc comment) - an
    /// all-pinned backlog large enough to have crossed `EVAL_DEBT_CEILING`,
    /// were pinned items still counted, must instead read as zero.
    #[test]
    fn the_eval_debt_owed_count_ignores_always_bound_items() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..EVAL_DEBT_CEILING {
            let id = format!("pinned-eval-{i}");
            declare_pinned(&mut store, &id, None);
            serve_n(&mut store, &id, JUDGEMENT_DEBT_AFTER);
        }
        let owed_in_project = judgement_debt_counts(&store, None).1;
        assert_eq!(owed_in_project, 0, "an all-pinned backlog must never feed the evaluation debt's own ceiling");
        assert!(!eval_debt_owed(owed_in_project, None, 0), "zero owed can never meet the ceiling");
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
// The evaluation debt (2026-09-12): `bin/serve.rs`'s Stop hook, once per
// session, asks the owner to run the WHOLE end-of-session evaluation
// (`ops::install::seed_eval_command`'s routine) rather than judging one more
// item, once this checkout's own judgement-debt backlog is both large and
// stale. See `bin/serve.rs`'s own `evaluation_debt` for the Stop-hook wiring;
// everything below is the pure/reusable half, kept here for the same reason
// `JUDGEMENT_DEBT_AFTER` and `judgement_debt_counts` already are: `doctor`
// (`ops::health::judgement_debt_line`) needs the exact same numbers, never a
// second copy that can silently drift from what the hook actually acts on.

/// How many items this checkout's own judgement debt must hold before the
/// evaluation debt can even be considered - `judgement_debt_counts`'s own
/// `in_project` half, the identical count `doctor` already names under
/// "judgement debt" for this checkout. Ten is a backlog, not a rounding
/// error: below it, walking the items one `mark` at a time (the per-item
/// judgement debt above, or the owner simply noticing) is still the cheaper
/// path, and asking for the whole routine over a handful of items would be
/// the nag this ceiling exists to prevent.
pub const EVAL_DEBT_CEILING: usize = 10;

/// How many hours may pass since this checkout's own newest verdict before
/// the evaluation debt is willing to speak at all - the grace period that
/// keeps it from asking again minutes after an evaluation actually happened.
/// A day, not an hour: an evaluation is deliberate, occasional work, and a
/// backlog that crossed the ceiling five minutes ago is not yet a pattern.
pub const EVAL_DEBT_STALE_HOURS: i64 = 24;

/// THE PURE PREDICATE. Both inputs are already resolved elsewhere
/// (`owed_in_project` from `judgement_debt_counts`, `newest_verdict_unix`
/// from the fold of the same name below, `now_unix` from `crate::time::
/// now_unix`) so this is nothing but the two conditions themselves,
/// unit-testable with plain integers and no store, no clock, no filesystem.
///
/// `newest_verdict_unix` is `None` for "never" (this checkout has not
/// recorded a single verdict on anything that applies to it) and `Some(t)`
/// for the instant of the newest one - see that function's own doc comment
/// for exactly which verdicts count. `now_unix.saturating_sub(t)` rather than
/// plain subtraction: `t` is read from a stored, human-editable timestamp,
/// and a clock skew that put it in the future must read as "not yet stale"
/// rather than underflow.
pub fn eval_debt_owed(owed_in_project: usize, newest_verdict_unix: Option<i64>, now_unix: i64) -> bool {
    if owed_in_project < EVAL_DEBT_CEILING {
        return false;
    }
    match newest_verdict_unix {
        None => true,
        Some(t) => now_unix.saturating_sub(t) > EVAL_DEBT_STALE_HOURS * 3600,
    }
}

/// Whole days between `then_unix` and `now_unix`, floored, never negative -
/// the one "N day(s) ago" rule both `doctor`'s judgement-debt line and the
/// Stop hook's own evaluation-debt message use for the same instant, so
/// neither ever rounds it differently from the other.
pub fn days_ago(now_unix: i64, then_unix: i64) -> i64 {
    (now_unix - then_unix).max(0) / 86400
}

/// The body shape `ItemMarkedUseful` and `ItemMarkedNoise` both carry
/// (`model::marked`) - only the one field this fold needs, read generically
/// so one parse serves both event kinds.
#[derive(serde::Deserialize)]
struct MarkedAt {
    marked_at: String,
}

/// The most recent verdict - a mark of usefulness OR of noise, either one
/// settles a review the same way `judged_since` (`bin/serve.rs`) already
/// treats them - among items that apply to `checkout_project` (project-scoped
/// to it, or global: `crate::project::applies_to`, the exact filter
/// `judgement_debt_counts`/`judgement_debt_named` already use for their own
/// `in_project` half), as Unix seconds. `None` when no such item has ever
/// been marked at all.
///
/// DELIBERATELY NOT SCOPED TO "CURRENTLY OWED" ITEMS ONLY, even though the
/// evaluation debt's own ceiling condition (`eval_debt_owed`'s
/// `owed_in_project`) is. A mark of usefulness or noise resets ITS OWN
/// item's served-since-verdict count to zero (`served_since_last_verdict`),
/// which drops that item OUT of the owed set the moment it is judged - so a
/// verdict scoped to "still owed right now" could never see the very
/// judgement that just happened, and a session that had just finished a real
/// evaluation pass would be told nothing here was ever judged. What this
/// asks instead is the plain question the debt's own message makes to the
/// owner - "has anything in this project's scope been judged lately" - which
/// is answered by ANY verdict on ANY item this checkout would ever be asked
/// about, owed or not.
///
/// LIVE ITEMS ONLY, same reasoning `judgement_debt` (`bin/serve.rs`) already
/// gives for its own "AND ONLY WHAT IS STILL LIVE" filter: a verdict's own
/// event never says what project it was scoped to at the time, only
/// `entity_id` - so this reads that back from the CURRENT live item, and an
/// id no longer live (retracted since) has no current scope to check at all,
/// so it is excluded rather than guessed at.
///
/// NOT PINNED-EXCLUDED, unlike `owed_items` above (and so unlike
/// `judgement_debt_counts`/`judgement_debt_named`, which both read that
/// exclusion from it): a verdict is a verdict regardless of what the item is
/// bound to today, and a mark given while it was trigger-bound, or given
/// after it is later unpinned, is exactly the honest evidence the evaluation
/// debt asks this function for. The waste this whole change removes is
/// asking for a NEW verdict nobody needs, never discounting a real one
/// already on record.
pub fn newest_verdict_unix(store: &EventStore, checkout_project: Option<&str>) -> Option<i64> {
    let live = crate::live::live_items(store);
    let project_of: HashMap<&str, Option<&str>> =
        live.iter().map(|li| (li.id.as_str(), li.item.project.as_deref())).collect();
    let events = store.get_all_events().ok()?;
    events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise))
        .filter(|e| {
            project_of.get(e.entity_id.as_str()).is_some_and(|p| crate::project::applies_to(*p, checkout_project))
        })
        .filter_map(|e| serde_json::from_str::<MarkedAt>(&e.body).ok())
        .filter_map(|m| crate::time::unix_from_iso8601(&m.marked_at))
        .max()
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

    // Fixed reference instant - any value works, since the predicate only
    // ever looks at the DIFFERENCE between it and a verdict's own timestamp.
    const NOW: i64 = 1_800_000_000;

    #[test]
    fn fires_at_exactly_the_ceiling_with_a_verdict_25_hours_old() {
        let verdict = NOW - 25 * 3600;
        assert!(eval_debt_owed(EVAL_DEBT_CEILING, Some(verdict), NOW));
    }

    #[test]
    fn silent_one_below_the_ceiling() {
        // A verdict that is "never" (None) would otherwise satisfy the
        // staleness half outright - proving the ceiling alone still holds
        // the line even against the most permissive possible staleness input.
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING - 1, None, NOW));
    }

    #[test]
    fn silent_with_a_verdict_23_hours_old() {
        let verdict = NOW - 23 * 3600;
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING, Some(verdict), NOW));
    }

    #[test]
    fn fires_with_no_verdict_ever() {
        assert!(eval_debt_owed(EVAL_DEBT_CEILING + 5, None, NOW));
    }

    #[test]
    fn a_verdict_from_the_future_is_not_yet_stale() {
        // Clock skew, or a hand-edited timestamp: must not underflow into a
        // huge apparent age via `saturating_sub` reading the wrong direction.
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING, Some(NOW + 3600), NOW));
    }

    #[test]
    fn days_ago_floors_and_never_goes_negative() {
        assert_eq!(days_ago(NOW, NOW - 3 * 86400), 3);
        assert_eq!(days_ago(NOW, NOW - 3 * 86400 - 1), 3, "not yet a fourth full day");
        assert_eq!(days_ago(NOW, NOW + 3600), 0, "a future timestamp reads as 0, never negative");
    }
}

#[cfg(test)]
mod newest_verdict_tests {
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

    #[test]
    fn an_empty_store_has_no_newest_verdict() {
        let store = EventStore::in_memory().unwrap();
        assert_eq!(newest_verdict_unix(&store, None), None);
    }

    #[test]
    fn a_single_mark_is_the_newest_verdict() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "a", None);
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "a").unwrap();
        assert_eq!(
            newest_verdict_unix(&store, None),
            crate::time::unix_from_iso8601("2026-08-02T00:00:00Z")
        );
    }

    #[test]
    fn the_later_of_two_verdicts_wins_regardless_of_kind() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "a", None);
        declare(&mut store, "b", None);
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "a").unwrap();
        crate::mark::record_noise(&mut store, "s", "l", "a", "2026-08-05T00:00:00Z", "b").unwrap();
        assert_eq!(
            newest_verdict_unix(&store, None),
            crate::time::unix_from_iso8601("2026-08-05T00:00:00Z"),
            "a later NOISE mark still counts as the newest verdict"
        );
    }

    #[test]
    fn a_verdict_owed_only_to_another_project_is_excluded() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "acme-item", Some("acme"));
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "acme-item").unwrap();
        assert_eq!(newest_verdict_unix(&store, Some("thor")), None, "a different checkout must not see it");
        assert!(newest_verdict_unix(&store, Some("acme")).is_some(), "fixture sanity: acme's own checkout does");
    }

    #[test]
    fn a_global_verdict_counts_toward_every_project() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "global-item", None);
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "global-item").unwrap();
        assert!(newest_verdict_unix(&store, Some("thor")).is_some());
        assert!(newest_verdict_unix(&store, Some("acme")).is_some());
    }

    #[test]
    fn a_verdict_on_a_since_retracted_item_is_excluded() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "gone", None);
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-08-02T00:00:00Z", "gone").unwrap();
        assert!(newest_verdict_unix(&store, None).is_some(), "fixture sanity: live and judged");
        model::store::retract(&mut store, "t", "t", "t", "gone", "no longer needed").unwrap();
        assert_eq!(newest_verdict_unix(&store, None), None, "a verdict on a dead item proves nothing current");
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
