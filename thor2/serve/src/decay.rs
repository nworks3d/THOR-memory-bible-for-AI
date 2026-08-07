//! Serving history, and the hypothesis it falsified.
//!
//! THE HYPOTHESIS - "decay by silence". An item served repeatedly and never
//! once marked useful (`crate::mark::record_useful`) stops reaching an
//! injection surface. Never deleted, never hidden from `lookup::search`; only
//! the three injection paths stop showing it. A single mark cancels it
//! permanently. The reasoning was that an item nobody ever engages with
//! should not linger, and the threshold was documented from the start as NOT
//! MEASURED, with "revisit once real serving numbers exist".
//!
//! THE MEASUREMENT, on the first real day of use (2026-08-03). Real numbers
//! now exist, and they falsify it outright:
//!
//! - Marks of usefulness recorded in a full day of real work: ZERO. Not few.
//!   None. Nothing in any surface asks for one, and an agent with work to do
//!   does not stop to press a button. So "never marked useful" is not a
//!   signal about the item - it is a constant.
//! - With that constant, the rule degenerates into an unconditional timer:
//!   every item that fires often enough disappears, whatever it says.
//! - It therefore retires the MOST relevant items first, because firing often
//!   is what relevance looks like. Backwards by construction.
//! - Observed after ONE day: four rules had already gone silent. All four
//!   bound to the `prod_data` moment, three of severity `costly`, one
//!   `irreversible`. Rules about not testing a write path against the real
//!   store, and about not running a reseed inside the live container. The
//!   mechanism's first act in production was to remove the owner's data-loss
//!   protection, silently, on the day he started using it.
//!
//! WHAT CHANGED. Silence no longer removes anything from an injection
//! surface. The counters stay, and `status`/`doctor` report "served N times,
//! never marked" as something a person can look at - observability without a
//! silent delete (CONTRACT R3 asks for delivery to be observable; it never
//! asked for unobserved usefulness to mean removal).
//!
//! WHY NOT JUST RAISE THE THRESHOLD, OR ADD A NUDGE. Both are compensating
//! mechanisms for a modelling error, which CONTRACT R9 says must be reported
//! rather than quietly added. A higher threshold delays the same silent
//! deletion; a nudge asking agents to mark things is exactly the kind of
//! behaviour-dependent convention R7 forbids relying on, and the one
//! comparable measurement anyone has (0 of 20 model compliance on a similar
//! ask) says it would not work either.
//!
//! WHAT REPLACED IT, the same day. An explicit NEGATIVE signal - the reader
//! saying "this did not belong here" - is a real observation, where the
//! absence of a positive one is not. And the owner had already produced 182
//! of them: 1.0 kept a `noise` ledger, which the migration left behind, and
//! 455 fireable 2.0 items descend from a fact he had explicitly called noise,
//! one of them nine times. So decay now reads THAT, and only that.
//!
//! WHAT DECAY IS NOW. An item called noise at least `NOISE_MARKS_BEFORE_STALE`
//! times, and never marked useful, stops reaching an injection surface. Never
//! deleted, never hidden from `lookup::search`, and a single mark of
//! usefulness overrides it permanently. A serving count decides nothing.
//! `status`/`doctor` still report "served N times, never marked" as something
//! to look at, never as a filter.

use crate::rank::RankedItem;
use model::item::{Binding, Item};
use std::collections::{HashMap, HashSet};
use thor_core::event_store::EventStore;

/// Kept for `status`/`doctor`'s "served this many times and never marked
/// useful" line, which is a number to LOOK AT. It decides nothing: see this
/// module's own doc comment for the day that rule was falsified in
/// production.
pub const STALE_AFTER_SERVINGS: usize = 5;

/// Called noise this many times, and never marked useful, is where an item
/// stops reaching an injection surface.
///
/// TWO, and not a number picked here: it is what 1.0 already did. Its own
/// rule was that a second noise judgement is where a fact starts costing a
/// slot, and that rule ran for months over the same memory and the same
/// reader. One stray judgement means "not here, this time" - a rule can fire
/// once in the wrong place and still be right. A second means the reader has
/// seen it fire wrongly twice, which is a pattern.
///
/// The owner's own ledger backs the shape: of his 182 noise judgements 135
/// are a single mark and 47 are two or more. A threshold of one would retire
/// nearly four times as much on a signal that has not repeated itself.
pub const NOISE_MARKS_BEFORE_STALE: usize = 2;

/// Everything decay needs about the store, loaded once per call so a serving
/// path folds the log a single time rather than once per candidate.
pub struct DecayContext {
    ever_marked_useful: HashSet<String>,
    noise: HashMap<String, usize>,
}

impl DecayContext {
    /// Load the two folds a decay decision needs, once, from the real store.
    pub fn load(store: &EventStore) -> Self {
        // Deliberately does NOT fold the serving history any more: a serving
        // count decides nothing here, so loading it on every hook call would
        // be work done to reach a number nobody reads.
        DecayContext {
            ever_marked_useful: crate::usefulness::ever_marked_useful(store),
            noise: crate::usefulness::noise_counts(store),
        }
    }

    /// Was this item ever marked useful? Exposed so `status` can report the
    /// look-at number without folding the log a second time, and without
    /// re-deriving the answer itself.
    pub fn was_ever_marked_useful(&self, id: &str) -> bool {
        self.ever_marked_useful.contains(id)
    }

    /// A context with no serving history at all: nothing is stale. The right
    /// default for any caller (and this module's own tests) that has no
    /// event log to fold at all yet.
    pub fn empty() -> Self {
        DecayContext { ever_marked_useful: HashSet::new(), noise: HashMap::new() }
    }

    /// Whether this item is currently stale: called noise at least
    /// `NOISE_MARKS_BEFORE_STALE` times and never once marked useful. The one
    /// comparison this whole module exists to make - see `serve/tests/
    /// decay_is_decided_in_exactly_one_place.rs`, which fails if this literal
    /// expression is ever duplicated elsewhere in this crate.
    ///
    /// A SERVING COUNT DECIDES NOTHING HERE, and that is the whole lesson of
    /// this module's own doc comment: firing often is what relevance looks
    /// like, so retiring on it is backwards.
    ///
    /// An `Always`-bound item is NEVER stale, and that is not a tuning knob -
    /// it is what `Always` means. Every other binding says "fire when this
    /// file or this action turns up"; `Always` says the owner elevated this
    /// to standing, which is the strongest mark of usefulness there is. A pin
    /// is already a mark, so retiring one for never having been marked
    /// retires it for the wrong reason. Taking a standing rule down is an
    /// unpin, and that is the owner's own act.
    ///
    /// MEASURED, ON THE FIRST REAL DAY (2026-08-03), so this is not a
    /// precaution: the owner runs several Claude Code sessions at once, and
    /// every one of them fires SessionStart. His first restart on 2.0 opened
    /// four sessions inside ten seconds, so all thirteen of his standing
    /// rules spent four of their five allowed servings in one go and decayed
    /// out of the injection surfaces before he had used the tool once.
    /// Raising the threshold would only have moved that failure a restart or
    /// two out (CONTRACT.md R9: a compensating knob is a reported design
    /// failure, not a fix). The modelling error was treating a pin as
    /// something that still has to earn its place.
    pub fn is_stale(&self, item: &Item) -> bool {
        if item.bindings.iter().any(|b| matches!(b, Binding::Always)) {
            return false;
        }
        if self.ever_marked_useful.contains(&item.id) {
            return false;
        }
        self.noise.get(&item.id).copied().unwrap_or(0) >= NOISE_MARKS_BEFORE_STALE
    }
}

/// The one place every injection surface calls to drop a stale item AFTER
/// `rank::select`/`session_start::select` has already decided kind/binding
/// eligibility. Order-preserving (a plain filter, never a re-sort): decay
/// never touches ranking, only removes what should not reach a gate at all.
/// Drops what the reader has twice said did not belong, and nothing else.
pub fn retain_live(items: Vec<RankedItem>, decay: &DecayContext) -> Vec<RankedItem> {
    items.into_iter().filter(|r| !decay.is_stale(&r.item)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind};

    fn item_of(id: &str, bindings: Vec<Binding>) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("rule {id}"),
            bindings,
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    fn bound(id: &str) -> Item {
        item_of(
            id,
            vec![Binding::Target { kind: model::item::TargetKind::Path, value: format!("src/{id}.rs") }],
        )
    }

    fn pinned(id: &str) -> Item {
        item_of(id, vec![Binding::Always])
    }

    fn ranked_from(item: Item) -> RankedItem {
        RankedItem { id: item.id.clone(), item }
    }

    /// A context where `id` was called noise `n` times and served a lot.
    fn called_noise(id: &str, n: usize) -> DecayContext {
        let mut noise = HashMap::new();
        noise.insert(id.to_string(), n);
        DecayContext { ever_marked_useful: HashSet::new(), noise }
    }

    #[test]
    fn one_noise_judgement_is_not_enough() {
        assert!(
            !called_noise("i1", NOISE_MARKS_BEFORE_STALE - 1).is_stale(&bound("i1")),
            "a rule can fire once in the wrong place and still be right"
        );
    }

    #[test]
    fn the_second_noise_judgement_retires_it() {
        assert!(called_noise("i1", NOISE_MARKS_BEFORE_STALE).is_stale(&bound("i1")));
    }

    /// THE DEFECT THIS PREVENTS, measured on the first real day (2026-08-03).
    /// Decay used to retire on a SERVING count with no mark of usefulness.
    /// Zero marks occur in real use, so that was a timer, and it retired the
    /// most-used rules first - four `prod_data` rules, one of them
    /// irreversible, were gone after one day. Firing often is what relevance
    /// looks like. A serving count must never retire anything again.
    #[test]
    fn a_serving_count_alone_never_retires_anything() {
        let decay = DecayContext { ever_marked_useful: HashSet::new(), noise: HashMap::new() };
        assert!(
            !decay.is_stale(&bound("hot-1")),
            "served constantly, never marked either way - that says nothing bad about it"
        );
    }

    /// THE OTHER DEFECT, same day: the owner runs several sessions at once,
    /// so one restart spent four of five allowed servings on every standing
    /// rule and all thirteen decayed out before he had used the tool. A pin
    /// is already a mark of usefulness; it comes down by an unpin.
    #[test]
    fn a_pinned_rule_never_decays_however_often_it_is_called_noise() {
        assert!(
            !called_noise("pin-1", NOISE_MARKS_BEFORE_STALE * 50).is_stale(&pinned("pin-1")),
            "an Always-bound rule is the owner's own standing choice"
        );
    }

    #[test]
    fn a_mark_of_usefulness_overrides_any_amount_of_noise() {
        let mut decay = called_noise("i1", NOISE_MARKS_BEFORE_STALE * 20);
        decay.ever_marked_useful.insert("i1".to_string());
        assert!(!decay.is_stale(&bound("i1")), "one mark of usefulness wins outright");
    }

    #[test]
    fn an_item_nobody_judged_is_never_stale() {
        assert!(!DecayContext::empty().is_stale(&bound("never-judged")));
    }

    #[test]
    fn retain_live_drops_only_the_retired_item_and_keeps_order() {
        let mut noise = HashMap::new();
        noise.insert("noisy-1".to_string(), NOISE_MARKS_BEFORE_STALE);
        let decay = DecayContext { ever_marked_useful: HashSet::new(), noise };
        let items = vec![ranked_from(bound("first")), ranked_from(bound("noisy-1")), ranked_from(bound("last"))];
        let ids: Vec<String> = retain_live(items, &decay).iter().map(|r| r.id.clone()).collect();
        assert_eq!(ids, vec!["first".to_string(), "last".to_string()]);
    }

    /// Reversible by design: this is a fold over the log re-evaluated every
    /// call, never a persisted flag, so a later mark of usefulness simply
    /// yields a different answer with nothing to undo.
    #[test]
    fn decay_is_reversible_a_mark_after_the_fact_un_stales_it() {
        let before = called_noise("i1", NOISE_MARKS_BEFORE_STALE);
        assert!(before.is_stale(&bound("i1")), "fixture sanity: retired before any mark");
        let mut after = called_noise("i1", NOISE_MARKS_BEFORE_STALE);
        after.ever_marked_useful.insert("i1".to_string());
        assert!(!after.is_stale(&bound("i1")));
    }
}
