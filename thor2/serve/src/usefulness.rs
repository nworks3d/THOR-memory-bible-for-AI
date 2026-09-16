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
        assert!(!eval_debt_owed(owed_in_project, None, None, 0), "zero owed can never meet the ceiling");
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
// The evaluation debt (2026-09-12, trigger rewritten 2026-09-16): `bin/
// serve.rs`'s Stop hook, once per session, asks the owner to run the WHOLE
// end-of-session evaluation (`ops::install::seed_eval_command`'s routine)
// rather than judging one more item, once this checkout's own judgement-debt
// backlog is both large and stale. See `bin/serve.rs`'s own `evaluation_debt`
// for the Stop-hook wiring; everything below is the pure/reusable half, kept
// here for the same reason `JUDGEMENT_DEBT_AFTER` and `judgement_debt_counts`
// already are: `doctor` (`ops::health::judgement_debt_line`) needs the exact
// same numbers and the exact same sidecar, never a second copy that can
// silently drift from what the hook actually acts on.
//
// THE CLOCK THIS REPLACES. Until 2026-09-16, "stale" meant "this checkout's
// own newest verdict (`newest_verdict_unix`, removed) is more than a day
// old" - but a verdict on a GLOBAL item resets that one clock for every
// project at once, whether or not the project actually over the ceiling saw
// a verdict of its own. Measured on a copy of the owner's own store,
// 2026-09-12 to 2026-09-16: the longest true gap between verdicts anywhere
// reached only 23.35 to 23.90 hours, so "nothing judged for 24 hours" was
// never once true in four days, while the backlog sat at or over the
// ceiling 56% to 100% of the measured hours across all nine projects. The
// debt never fired once. Two facts kept per project in a sidecar
// (`ProjectEvalState` below) replace it: how long the backlog has
// CONTINUOUSLY sat at or over the ceiling, and how long since an evaluation
// report was last (first) seen for that project - neither one moves just
// because some unrelated global rule happened to get judged.

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

/// THE PURE PREDICATE, rewritten 2026-09-16 (see this section's own doc
/// comment for the defect the old verdict-age version could never close).
/// All three inputs are already resolved elsewhere (`owed_in_project` from
/// `judgement_debt_counts`, `over_ceiling_since`/`last_evaluation_seen` from
/// this project's own `ProjectEvalState` below, `now_unix` from `crate::
/// time::now_unix`), so this stays nothing but the conditions themselves,
/// unit-testable with plain integers and no store, no clock, no filesystem.
///
/// Holds when the backlog is at or over the ceiling, AND it has stayed there
/// for at least `EVAL_DEBT_STALE_HOURS` (`over_ceiling_since` of `None` -
/// the sidecar has not yet recorded a Stop that saw the crossing - reads as
/// "just now", i.e. not yet stale, never as "forever"), AND no evaluation
/// report has silenced it since: either none was ever seen for this
/// project, or the last one seen is itself at least `EVAL_DEBT_STALE_HOURS`
/// old. `now_unix.saturating_sub(..)` throughout rather than plain
/// subtraction: both timestamps are read from a stored, human-editable
/// sidecar, and a clock skew that put either in the future must read as
/// "not yet stale" rather than underflow.
pub fn eval_debt_owed(
    owed_in_project: usize,
    over_ceiling_since: Option<i64>,
    last_evaluation_seen: Option<i64>,
    now_unix: i64,
) -> bool {
    if owed_in_project < EVAL_DEBT_CEILING {
        return false;
    }
    let Some(since) = over_ceiling_since else { return false };
    if now_unix.saturating_sub(since) <= EVAL_DEBT_STALE_HOURS * 3600 {
        return false;
    }
    match last_evaluation_seen {
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

/// The sidecar's own map key for a project - `""` stands for "no project" (a
/// checkout that resolves to no project at all, `None`). Never a real
/// project's own key: `project::resolve_project` never returns `Some("")` -
/// a blank marker line falls through to the git root instead (see that
/// function's own doc comment) - so this can never collide with a real one.
fn project_key(project: Option<&str>) -> &str {
    project.unwrap_or("")
}

/// This project's own two facts for the evaluation debt's trigger (see this
/// section's own doc comment for why a per-project sidecar replaced a single
/// global verdict clock). `#[serde(default)]` on every field: a sidecar
/// written by an earlier partial run, or hand-written for a test, must still
/// parse, with "nothing known yet" rather than a parse failure - the same
/// fail-open stance `capture::Marker`'s own fields already take.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectEvalState {
    /// The first time a main-session Stop in this project saw its own owed
    /// count at or over `EVAL_DEBT_CEILING` - `None` before that has ever
    /// happened, or once a Stop has since seen the count drop back under the
    /// ceiling (the crossing is forgotten outright, not merely paused: the
    /// next crossing starts its own new clock).
    #[serde(default)]
    pub over_ceiling_since: Option<i64>,
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

/// Refresh this project's own two facts from `store` as it stands right
/// now, and write the sidecar back if anything actually changed. Called
/// ONCE PER MAIN-SESSION STOP in a project (`bin/serve.rs`'s `hook_once`,
/// unconditionally within its own `!is_subagent` guard) - deliberately
/// unconditioned by the once-per-session/served-this-session gates that
/// decide whether the obligation is actually SHOWN (`eval_debt_not_yet_
/// asked_this_session`, `session_served_in_project`): those two are about
/// whether to SPEAK, this is about whether the RECORD stays true, and a
/// session that is never asked must still leave the ceiling clock and the
/// report sighting exactly as accurate as one that was.
///
/// NEVER CALLED BY `doctor` (`ops::health::judgement_debt_line`), which
/// reads this same state through [`project_eval_state`] but must stay
/// read-only - see that function's own doc comment.
pub fn update_eval_debt_state(store: &EventStore, db: &Path, project: Option<&str>, now_unix: i64) -> ProjectEvalState {
    let mut all = read_eval_debt_state(db);
    let key = project_key(project).to_string();
    let before = all.get(&key).cloned().unwrap_or_default();
    let mut entry = before.clone();

    // `over_ceiling_since`: set on the first Stop that sees the crossing,
    // left untouched on every Stop after that (`get_or_insert` never
    // overwrites a `Some`) so the clock measures the WHOLE streak, not just
    // the most recent Stop - and cleared outright the first time a Stop sees
    // the count fall back under the ceiling, so the next crossing starts
    // fresh rather than inheriting a streak that already ended.
    let (_, owed_in_project) = judgement_debt_counts(store, project);
    if owed_in_project >= EVAL_DEBT_CEILING {
        entry.over_ceiling_since.get_or_insert(now_unix);
    } else {
        entry.over_ceiling_since = None;
    }

    // `last_evaluation_seen`/`last_evaluation_report_id`: any id live right
    // now that this sidecar has not seen before is "new" - stamp the moment
    // and grow the known set, so the SAME report never re-triggers this on a
    // later Stop just for still existing.
    let new_ids: Vec<String> =
        evaluation_report_ids(store, project).into_iter().filter(|id| !entry.known_report_ids.contains(id)).collect();
    if let Some(newest) = new_ids.iter().max().cloned() {
        entry.last_evaluation_seen = Some(now_unix);
        entry.last_evaluation_report_id = Some(newest);
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
    // ever looks at the DIFFERENCE between it and a stored instant.
    const NOW: i64 = 1_800_000_000;
    const HOUR: i64 = 3600;

    #[test]
    fn silent_below_the_ceiling_regardless_of_how_stale_everything_else_is() {
        // Both other inputs are maximally "fire me" (long over the ceiling,
        // no report ever) - proving the ceiling alone still holds the line.
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING - 1, Some(NOW - 100 * HOUR), None, NOW));
    }

    /// Case named in the build brief: over the ceiling for 23 hours -> silent.
    #[test]
    fn silent_over_the_ceiling_for_only_23_hours() {
        let since = NOW - 23 * HOUR;
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING, Some(since), None, NOW));
    }

    /// Case named in the build brief: 25 hours and no report -> fires.
    #[test]
    fn fires_over_the_ceiling_for_25_hours_with_no_report_ever() {
        let since = NOW - 25 * HOUR;
        assert!(eval_debt_owed(EVAL_DEBT_CEILING, Some(since), None, NOW));
    }

    /// Case named in the build brief: report first seen 10 hours ago -> silent.
    #[test]
    fn silent_when_a_report_was_first_seen_10_hours_ago() {
        let since = NOW - 48 * HOUR;
        let seen = NOW - 10 * HOUR;
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING, Some(since), Some(seen), NOW));
    }

    /// Case named in the build brief: report first seen 30 hours ago and
    /// still over -> fires.
    #[test]
    fn fires_when_the_report_first_seen_30_hours_ago_is_itself_stale() {
        let since = NOW - 48 * HOUR;
        let seen = NOW - 30 * HOUR;
        assert!(eval_debt_owed(EVAL_DEBT_CEILING, Some(since), Some(seen), NOW));
    }

    /// `over_ceiling_since` is `None` on exactly the Stop that first crosses
    /// the ceiling, before `update_eval_debt_state` has written anything for
    /// this project yet - that Stop must never fire on the spot.
    #[test]
    fn a_crossing_the_sidecar_has_not_recorded_yet_is_not_yet_stale() {
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING, None, None, NOW));
    }

    #[test]
    fn a_future_over_ceiling_since_is_not_yet_stale() {
        // Clock skew, or a hand-edited sidecar: must not underflow into a
        // huge apparent age via `saturating_sub` reading the wrong direction.
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING, Some(NOW + HOUR), None, NOW));
    }

    #[test]
    fn a_future_last_evaluation_seen_is_not_yet_stale() {
        let since = NOW - 48 * HOUR;
        assert!(!eval_debt_owed(EVAL_DEBT_CEILING, Some(since), Some(NOW + HOUR), NOW));
    }

    #[test]
    fn days_ago_floors_and_never_goes_negative() {
        assert_eq!(days_ago(NOW, NOW - 3 * 86400), 3);
        assert_eq!(days_ago(NOW, NOW - 3 * 86400 - 1), 3, "not yet a fourth full day");
        assert_eq!(days_ago(NOW, NOW + 3600), 0, "a future timestamp reads as 0, never negative");
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
        let mut store = EventStore::new(&db).unwrap();
        owe(&mut store, EVAL_DEBT_CEILING, None);
        let written = update_eval_debt_state(&store, &db, None, 1_800_000_000);
        let read_back = project_eval_state(&db, None);
        assert_eq!(written, read_back);
        assert_eq!(read_back.over_ceiling_since, Some(1_800_000_000));
    }

    #[test]
    fn writing_only_happens_when_something_actually_changed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();
        update_eval_debt_state(&store, &db, None, 1_000);
        assert!(!eval_debt_state_path(&db).exists(), "an all-default state must never write an empty sidecar");
    }

    #[test]
    fn two_projects_keep_separate_entries_in_one_sidecar_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        owe(&mut store, EVAL_DEBT_CEILING, Some("thor"));
        update_eval_debt_state(&store, &db, Some("thor"), 1_000);
        update_eval_debt_state(&store, &db, Some("acme"), 2_000);
        assert_eq!(project_eval_state(&db, Some("thor")).over_ceiling_since, Some(1_000));
        assert_eq!(
            project_eval_state(&db, Some("acme")).over_ceiling_since,
            None,
            "acme's own backlog never crossed the ceiling"
        );
    }

    // ------------------------------------------------- over_ceiling_since

    #[test]
    fn over_ceiling_since_is_set_on_the_first_crossing_and_never_moves_after() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        owe(&mut store, EVAL_DEBT_CEILING, None);
        let first = update_eval_debt_state(&store, &db, None, 1_000);
        assert_eq!(first.over_ceiling_since, Some(1_000));
        let second = update_eval_debt_state(&store, &db, None, 50_000);
        assert_eq!(
            second.over_ceiling_since,
            Some(1_000),
            "the clock must not restart on a later Stop that is still over the ceiling"
        );
    }

    /// Case named in the build brief: owed below the ceiling resets the clock.
    #[test]
    fn over_ceiling_since_is_cleared_the_first_time_a_stop_sees_the_count_drop_back_under() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        owe(&mut store, EVAL_DEBT_CEILING, None);
        let over = update_eval_debt_state(&store, &db, None, 1_000);
        assert!(over.over_ceiling_since.is_some(), "fixture sanity: over the ceiling");
        crate::mark::record_useful(&mut store, "s", "s", "t", "2026-09-08T00:00:00Z", "owed-global-0").unwrap();
        let dropped = update_eval_debt_state(&store, &db, None, 2_000);
        assert_eq!(dropped.over_ceiling_since, None, "a verdict that drops the count under the ceiling resets the clock");
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
