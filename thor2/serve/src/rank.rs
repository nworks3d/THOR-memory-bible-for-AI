//! Selection and ranking: everything a `Rule`/`Orientation` binding matches,
//! ordered worst-first. Used by every channel (hook/check/why) through
//! `select` - the one selection function the workspace design note requires.
//!
//! Ranking order (CONTRACT.md, this crate's own brief):
//! 1. severity: irreversible > costly > house_style > no severity at all.
//! 2. a derived overlap key: how many distinctive words of the ACTUAL input
//!    (never the item's own declared tags) the item's text also contains.
//! 3. never sentence length - see `ranking_never_falls_back_to_sentence_length`
//!    below for the defect this refuses to reintroduce. The final tiebreaker
//!    is the item's own id, which is arbitrary but never a length-shaped bias.

use crate::input::ServeInput;
use crate::live::LiveItem;
use model::item::{Binding, Item, Kind, Severity, TargetKind};
use regex::Regex;
use std::sync::OnceLock;

/// One item plus everything the render step needs to know about why it is here.
#[derive(Debug, Clone)]
pub struct RankedItem {
    pub id: String,
    pub item: Item,
}

/// Moved to `model::item` so the write gate can rank a candidate against its
/// rivals without `model` depending on `serve`. Re-exported here because the
/// ranking is still this module's business and every doc comment names it so.
pub use model::item::severity_rank;

/// A distinctive term: letters/digits plus the punctuation an identifier or a
/// path realistically carries, at least 4 characters. Same shape as the
/// sibling Python arm's `_TERM` (slices/python/gate.py) - the idea that a rule
/// naming what is actually happening beats a generic one is not new here,
/// only ported into the item model.
fn term_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-z0-9][a-z0-9._/-]{3,}").unwrap())
}

fn terms(text: &str) -> std::collections::HashSet<String> {
    term_regex().find_iter(&text.to_lowercase()).map(|m| m.as_str().to_string()).collect()
}

/// How many distinctive terms of the INPUT's own context (the real command,
/// path or draft text - never a field the item declared about itself) also
/// appear in the item's text. Derived fresh every call, so an item cannot
/// game its own rank by declaring keywords; only what is actually happening
/// counts.
pub fn closeness(item_text: &str, context: &str) -> usize {
    let ctx_terms = terms(context);
    if ctx_terms.is_empty() {
        return 0;
    }
    let item_terms = terms(item_text);
    ctx_terms.intersection(&item_terms).count()
}

/// Same target: moved to `model::normalize` so the write gate can ask the
/// same question without `model` having to depend on `serve`. Re-exported
/// here under its original name, because every caller and half the doc
/// comments in this workspace name it `rank::target_matches`.
pub use model::normalize::target_matches;

/// `Always` never matches here - on purpose. The five injection surfaces
/// (CONTRACT.md's "vijf oppervlakken die elkaars plaatsen nooit afnemen")
/// never share a candidate pool: `Always` is exactly and only surface 1's
/// (session start, see `crate::session_start`), so it is refused here even
/// though the type still allows an item to declare it - the same
/// belt-and-braces the `Kind::can_fire` gate already applies to `Report`/
/// `Chunk`/`Lookup`. An item bound to BOTH `Always` and a real `Moment`/
/// `Target` still reaches the moment/prompt surfaces through that other
/// binding; only the `Always` binding itself is inert here.
fn binding_matches(binding: &Binding, input: &ServeInput) -> bool {
    match binding {
        Binding::Always => false,
        Binding::Moment(action) => input.moments.contains(action),
        // A Command binding is the one kind whose two sides are not the same
        // sort of string: the rule declares a command ("gh repo edit"), the
        // input carries a whole invocation ("gh repo edit x --visibility
        // public"). `target_matches` compares those as paths - equality or a
        // segment suffix - so a real, argument-bearing command essentially
        // never matched, and every fact anchored to a command was silent at
        // the exact moment it was written for. Reproduced against the real
        // store on 2026-08-14 on the rule guarding repo visibility.
        //
        // The guard arm had already solved this and said so in its own doc
        // comment; the serving path simply never used it. This is that one
        // definition, not a second one - see
        // `absent_guard::command_anchor_names`.
        Binding::Target { kind: TargetKind::Command, value } => input
            .targets
            .iter()
            .any(|(k, v)| *k == TargetKind::Command && crate::absent_guard::command_anchor_names(value, v)),
        Binding::Target { kind, value } => input
            .targets
            .iter()
            .any(|(k, v)| target_matches(*kind, value, *k, v)),
    }
}

/// Is this item even eligible to reach a gate at all? Delegates to the ONE
/// definition of "which kinds can fire" (`model::item::Kind::can_fire`) -
/// this function must never re-decide that boolean itself (see
/// `model/tests/single_can_fire_definition.rs`, which greps the whole
/// workspace for exactly that regression).
///
/// A `Report` or a `Chunk` may never reach a gate (the write gate already
/// refuses either of them with a binding - this is the SECOND lock on the
/// same door: even one hand-built with a binding, bypassing the gate
/// entirely, must still never be selected here). A `Lookup` is a different
/// door: it answers only an explicit request for its own key, never a
/// moment/target match (see `item::Kind`'s own doc comment), so it is
/// excluded here too, not served by this path at all.
fn eligible(kind: Kind) -> bool {
    kind.can_fire()
}

/// Every live item whose kind may reach a gate and at least one of whose
/// bindings matches the input, ranked worst-first: severity, then closeness
/// to what is actually happening, then id (never length - see the test
/// module). This is `select` in the CONTRACT sense: the ONE function every
/// channel (hook/check/why) calls to decide what applies, before any cap.
pub fn select(candidates: &[LiveItem], input: &ServeInput) -> Vec<RankedItem> {
    let mut hits: Vec<RankedItem> = candidates
        .iter()
        .filter(|c| eligible(c.item.kind))
        // The same scoping session start applies, through the same single
        // place. Without it every project's moment-bound rules competed for
        // this surface's four slots in every project - see
        // `project::applies_to` for the measurement that found it.
        .filter(|c| crate::project::applies_to(c.item.project.as_deref(), input.project.as_deref()))
        .filter(|c| c.item.bindings.iter().any(|b| binding_matches(b, input)))
        .map(|c| RankedItem { id: c.id.clone(), item: c.item.clone() })
        .collect();

    hits.sort_by(|a, b| {
        // 1. IRREVERSIBLE KEEPS THE TOP BAND, always. Nothing lighter may
        //    displace a warning about something that cannot be undone, whatever
        //    else is true of it. This band is what makes the rest of this
        //    ordering safe to change at all.
        irreversible(&a.item)
            .cmp(&irreversible(&b.item))
            // 2. THEN THE ONE ANCHORED AT WHAT YOU ARE TOUCHING. Measured on
            //    the owner's store, 2026-08-09: opening the busiest file in it
            //    showed four general warnings about deploying and none of the
            //    twelve facts about that very file, because severity decided
            //    everything and closeness was never consulted across bands. A
            //    general rule still reaches you at the moment it is about; the
            //    specific one has only this one chance.
            .then_with(|| anchored_at_a_place(&b.item).cmp(&anchored_at_a_place(&a.item)))
            // 2b. AND AMONG PLACES, THE EXACT ONE FIRST. Since a directory
            //     anchor reaches the files inside it (see
            //     `normalize::target_matches`), one rule about a whole tree
            //     could otherwise sit in front of the rule about the very file
            //     being touched. The broad one still reaches you; the precise
            //     one has only this place.
            .then_with(|| at_this_exact_place(&b.item, input).cmp(&at_this_exact_place(&a.item, input)))
            // 3. Then weight, as before, among items equally close.
            .then_with(|| severity_rank(a.item.severity).cmp(&severity_rank(b.item.severity)))
            .then_with(|| {
                let ca = closeness(&a.item.text, &input.context);
                let cb = closeness(&b.item.text, &input.context);
                cb.cmp(&ca) // higher overlap first
            })
            .then_with(|| a.id.cmp(&b.id)) // deterministic, never length
    });
    hits
}

/// A warning about something that cannot be undone. Its own key in the
/// ordering, so nothing lighter can ever take its place - see `select`.
fn irreversible(item: &Item) -> bool {
    item.severity != Some(Severity::Irreversible)
}

/// Is this item anchored at the very thing being touched, rather than at a
/// directory somewhere above it? Both reach you; this decides which one is
/// read first when only a few fit.
fn at_this_exact_place(item: &Item, input: &ServeInput) -> bool {
    item.bindings.iter().any(|b| match b {
        Binding::Target { kind: TargetKind::Path, value } => input.targets.iter().any(|(k, v)| {
            *k == TargetKind::Path && model::normalize::normalize_target(value) == model::normalize::normalize_target(v)
        }),
        Binding::Target { kind: TargetKind::Dir, value } => input.targets.iter().any(|(_, v)| {
            model::normalize::normalize_target(value) == model::normalize::normalize_target(v)
        }),
        _ => false,
    })
}

/// Is this item bound to a PLACE - a file or a directory - rather than
/// reaching the pool through a broad moment? A place-bound item gets one
/// chance, at that place; a moment-bound one reaches you wherever that moment
/// is detected, so it has others.
fn anchored_at_a_place(item: &Item) -> bool {
    item.bindings
        .iter()
        .any(|b| matches!(b, Binding::Target { kind: TargetKind::Path | TargetKind::Dir, .. }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::ServeInput;
    use intent::Action;
    use model::item::{Binding, Item, Kind, Severity, TargetKind};

    fn base(id: &str, kind: Kind) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind,
                text: "do the thing".to_string(),
                bindings: vec![Binding::Moment(Action::Configure)],
                severity: None,
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    /// An input that matches `base`'s own default binding
    /// (`Moment(Configure)`) - the fixture ranking tests below use to get a
    /// guaranteed hit without caring about moment/target mechanics
    /// themselves. Named for what it does, not "always": `Binding::Always`
    /// itself is never matched by `select` (see `binding_matches`'s own doc
    /// comment) and deliberately has no representation here any more.
    fn input_matching_base() -> ServeInput {
        ServeInput { moments: vec![Action::Configure], targets: vec![], context: String::new(), project: None }
    }

    // ------------------------------------------------------ project scope

    fn owned_by(id: &str, project: Option<&str>) -> LiveItem {
        let mut c = base(id, Kind::Rule);
        c.item.project = project.map(str::to_string);
        c
    }

    fn in_project(name: Option<&str>) -> ServeInput {
        ServeInput {
            moments: vec![Action::Configure],
            targets: vec![],
            context: String::new(),
            project: name.map(str::to_string),
        }
    }

    /// THE DEFECT THIS PREVENTS, measured on the live store (2026-08-03).
    /// Session start scoped by project; this surface did not. The store holds
    /// 136 moment-bound rules owned by one business project against 66 for
    /// another and 74 global, so every action in every project drew from the
    /// whole pile and four slots decided the rest. Caught in the act: writing
    /// a Dockerfile for a NAS drew rules about a website's dev directory, an
    /// admin HTTP route and a production database wipe. Each rule correct,
    /// none of them about the work in hand.
    #[test]
    fn another_projects_rule_never_reaches_this_projects_moment() {
        let hits = select(&[owned_by("other", Some("acme-shop"))], &in_project(Some("thor")));
        assert!(hits.is_empty(), "a rule owned by another project must not fire here");
    }

    #[test]
    fn this_projects_own_rule_still_fires() {
        let hits = select(&[owned_by("mine", Some("thor"))], &in_project(Some("thor")));
        assert_eq!(hits.len(), 1, "a rule owned by this project must still fire");
    }

    /// A global rule belongs to every project - that is what "no project of
    /// its own" means, and scoping must never quietly retire the global tier.
    #[test]
    fn a_global_rule_fires_in_every_project_and_with_no_project_at_all() {
        assert_eq!(select(&[owned_by("g", None)], &in_project(Some("thor"))).len(), 1);
        assert_eq!(select(&[owned_by("g", None)], &in_project(Some("anything-else"))).len(), 1);
        assert_eq!(select(&[owned_by("g", None)], &in_project(None)).len(), 1);
    }

    /// When the project cannot be resolved at all, serve the global tier and
    /// nothing else. Under-serving is the safe direction here: a rule that
    /// does not apply teaches the reader to skim past the whole block.
    #[test]
    fn an_unresolvable_project_gets_the_global_tier_only() {
        let candidates = vec![owned_by("g", None), owned_by("owned", Some("acme-shop"))];
        let hits = select(&candidates, &in_project(None));
        let ids: Vec<&str> = hits.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["g"]);
    }

    // --------------------------------------------------------- eligibility

    #[test]
    fn a_report_can_never_reach_a_gate_even_bypassing_the_write_gate() {
        // The write gate already refuses a Report with a binding at declare
        // time; this constructs one directly (as if that check had never
        // run), WITH a binding that genuinely matches the input, to prove
        // SELECT is a second, independent lock on the door by KIND - not
        // merely a side effect of the binding never matching either.
        let mut c = base("r1", Kind::Report);
        c.item.bindings = vec![Binding::Moment(Action::Configure)];
        let hits = select(&[c], &input_matching_base());
        assert!(hits.is_empty(), "a Report must never be selected, gate or no gate");
    }

    #[test]
    fn a_chunk_can_never_reach_a_gate_even_bypassing_the_write_gate() {
        // Same guarantee as `a_report_can_never_reach_a_gate_even_bypassing_the_write_gate`,
        // for the new archive kind: the write gate already refuses a Chunk
        // with a binding, but this constructs one directly (as if that check
        // had never run), with a binding that genuinely matches, to prove
        // SELECT is a second, independent lock on the door by KIND.
        let mut c = base("k1", Kind::Chunk);
        c.item.bindings = vec![Binding::Moment(Action::Configure)];
        let hits = select(&[c], &input_matching_base());
        assert!(hits.is_empty(), "a Chunk must never be selected, gate or no gate");
    }

    #[test]
    fn a_lookup_answers_only_its_own_key_never_a_moment_or_target() {
        let mut c = base("l1", Kind::Lookup);
        c.item.key = Some("k".to_string());
        c.item.bindings = vec![Binding::Moment(Action::Configure)];
        let hits = select(&[c], &input_matching_base());
        assert!(hits.is_empty(), "a Lookup must not be reachable through moment/target selection");
    }

    #[test]
    fn an_item_bound_only_to_always_is_never_selected_by_moment_or_prompt_select() {
        // The defect this test names: `Always` used to fire on ANY input
        // here (the old behaviour `serve::session_start` now replaces),
        // which meant the "moment of action" and "session start" surfaces
        // silently served the same items - exactly the overlap CONTRACT.md's
        // five-surfaces rule forbids. `Always` is exclusively surface 1's now
        // (see `crate::session_start`); this input DOES match the item's
        // kind/eligibility, so if this test ever goes green for the wrong
        // reason, `an_item_bound_to_both_always_and_a_real_moment_still_fires_through_the_moment`
        // right below catches it (proves the same item DOES fire through a
        // real binding).
        let mut c = base("x1", Kind::Rule);
        c.item.bindings = vec![Binding::Always];
        let hits = select(&[c], &input_matching_base());
        assert!(hits.is_empty(), "an Always-only binding must never be selected outside session start");
    }

    #[test]
    fn an_item_bound_to_both_always_and_a_real_moment_still_fires_through_the_moment() {
        // The other half of the guarantee above: excluding `Always` from
        // matching must not poison the REST of an item's bindings - an item
        // bound to both `Always` (for session start) and a real `Moment`
        // (for the moment/prompt surfaces) still resolves through that
        // second binding exactly as if `Always` were not there at all.
        let mut c = base("x1b", Kind::Rule);
        c.item.bindings = vec![Binding::Always, Binding::Moment(Action::Configure)];
        let hits = select(&[c], &input_matching_base());
        assert_eq!(hits.len(), 1, "the Moment binding must still resolve regardless of the Always sibling");
    }

    #[test]
    fn a_moment_binding_only_fires_on_its_own_action() {
        let mut c = base("x2", Kind::Orientation);
        c.item.bindings = vec![Binding::Moment(Action::Push)];
        let miss = ServeInput { moments: vec![Action::Commit], targets: vec![], context: String::new(), project: None };
        assert!(select(&[c.clone_for_test()], &miss).is_empty());
        let hit = ServeInput { moments: vec![Action::Push], targets: vec![], context: String::new(), project: None };
        assert_eq!(select(&[c], &hit).len(), 1);
    }

    impl LiveItem {
        fn clone_for_test(&self) -> LiveItem {
            LiveItem { id: self.id.clone(), item: self.item.clone() }
        }
    }

    #[test]
    fn a_target_binding_matches_by_full_normalized_value() {
        let mut c = base("x3", Kind::Orientation);
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: r"Src\Main.rs".to_string() }];
        let input = ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Path, "src/main.rs".to_string())],
            context: String::new(), project: None,
        };
        assert_eq!(select(&[c], &input).len(), 1);
    }

    #[test]
    fn a_target_bound_by_full_path_still_fires_on_its_bare_name() {
        // The defect this test names: an item declared against its full path
        // (as the migration's real data does) must still fire when the touched
        // target is given only by its last segment, and vice versa - the same
        // "same target" doctrine `normalize::last_segment` documents.
        let mut c = base("x4", Kind::Orientation);
        c.item.bindings =
            vec![Binding::Target { kind: TargetKind::Path, value: r"C:\repo\swap-binary.ps1".to_string() }];
        let input = ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Path, "swap-binary.ps1".to_string())],
            context: String::new(), project: None,
        };
        assert_eq!(select(&[c], &input).len(), 1);
    }

    /// THE DEFECT, reproduced against the real store on 2026-08-14 before it
    /// was fixed here: a Command binding was compared the way a path is, so a
    /// rule anchored to "gh repo edit" applied to a bare `gh repo edit` and
    /// was SILENT for `gh repo edit <repo> --visibility public` - the only
    /// form anyone actually types, and the one the rule exists for. A whole
    /// binding kind that does not fire at its own moment is the worst failure
    /// this system has, because nothing reports it.
    #[test]
    fn a_command_binding_fires_on_the_real_invocation_not_only_the_bare_words() {
        let mut c = base("x4b", Kind::Rule);
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "gh repo edit".to_string() }];
        let with_args = ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Command, "gh repo edit nworks3d/x --visibility public".to_string())],
            context: String::new(),
            project: None,
        };
        assert_eq!(select(&[c.clone_for_test()], &with_args).len(), 1, "the arguments are the normal case");

        // And the narrowness the guard arm already proved: a different
        // subcommand is a different command, and a mention is not a run.
        for miss in ["gh repo view nworks3d/x", "echo remember to gh repo edit later"] {
            let input = ServeInput {
                moments: vec![],
                targets: vec![(TargetKind::Command, miss.to_string())],
                context: String::new(),
                project: None,
            };
            assert!(select(&[c.clone_for_test()], &input).is_empty(), "must not fire on: {miss}");
        }
    }

    #[test]
    fn a_target_binding_never_matches_a_different_kind() {
        let mut c = base("x5", Kind::Orientation);
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "main.rs".to_string() }];
        let input = ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Path, "main.rs".to_string())],
            context: String::new(), project: None,
        };
        assert!(select(&[c], &input).is_empty(), "a Command binding must not answer a Path target");
    }

    // --------------------------------------------------------- ranking

    #[test]
    fn heaviest_severity_ranks_first() {
        let mut irr = base("i", Kind::Rule);
        irr.item.severity = Some(Severity::Irreversible);
        let mut costly = base("c", Kind::Rule);
        costly.item.severity = Some(Severity::Costly);
        let hits = select(&[costly, irr], &input_matching_base());
        assert_eq!(hits[0].id, "i");
        assert_eq!(hits[1].id, "c");
    }

    #[test]
    fn a_missing_severity_never_ranks_as_a_middle_value() {
        // Derived Ord on Option<Severity> would put None FIRST (ahead of
        // Irreversible). This proves a severity-less item sinks BELOW every
        // severity, not above Irreversible and not between Costly and
        // HouseStyle either.
        let mut irr = base("i", Kind::Rule);
        irr.item.severity = Some(Severity::Irreversible);
        let mut house = base("h", Kind::Rule);
        house.item.severity = Some(Severity::HouseStyle);
        let mut none = base("n", Kind::Rule);
        none.item.severity = None;
        let hits = select(&[none, house, irr], &input_matching_base());
        assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec!["i", "h", "n"]);
    }

    #[test]
    fn an_item_naming_what_actually_happened_beats_a_generic_item() {
        let mut generic = base("g", Kind::Rule);
        generic.item.severity = Some(Severity::Costly);
        generic.item.text = "be careful with deployments in general".to_string();
        let mut specific = base("s", Kind::Rule);
        specific.item.severity = Some(Severity::Costly);
        specific.item.text = "docker-compose.yml controls the payment-gateway container".to_string();
        let input = ServeInput {
            // Both items carry base()'s default Moment(Configure) binding, so
            // this must still resolve it to get either item selected at all -
            // only the CLOSENESS tiebreaker below is what this test is
            // actually about.
            moments: vec![Action::Configure],
            targets: vec![],
            context: "docker compose -f docker-compose.yml up payment-gateway".to_string(), project: None,
        };
        let hits = select(&[generic, specific], &input);
        assert_eq!(hits[0].id, "s", "the item naming the real command/path must rank first");
    }

    /// THE INVERSION THIS FIXES, measured on the owner's real store
    /// (2026-08-09): opening the busiest file in it showed four general
    /// warnings about deploying and NONE of the twelve facts about that very
    /// file. Severity decided everything and closeness was never consulted
    /// across bands, so a rule bound to a broad moment always beat one
    /// anchored at the file in your hands. The general rule still reaches you
    /// at the moment it is about; the specific one had only that one chance.
    #[test]
    fn a_fact_anchored_at_this_file_beats_a_general_one_of_the_same_weight() {
        let mut anchored = base("anchored", Kind::Rule);
        anchored.item.severity = Some(Severity::Costly);
        anchored.item.bindings =
            vec![Binding::Target { kind: TargetKind::Path, value: "deploy/compose.yml".to_string() }];

        let mut general = base("general", Kind::Rule);
        general.item.severity = Some(Severity::Costly);
        general.item.bindings = vec![Binding::Moment(Action::Deploy)];

        let mut input = ServeInput::default();
        input.add_file("deploy/compose.yml");
        input.moments.push(Action::Deploy);

        let hits = select(&[general, anchored], &input);
        assert_eq!(hits[0].id, "anchored", "the fact about this very file must come first");
    }

    /// AND THE LINE THAT MAKES THAT SAFE. An irreversible warning keeps the top
    /// band whatever else is true: measured before shipping the rule above,
    /// plain closeness-first would have pushed six irreversible warnings out of
    /// their place on this store. A memory that shows the handy note instead of
    /// the one that stops you wrecking production is worse than one that shows
    /// neither.
    #[test]
    fn an_irreversible_warning_is_never_displaced_by_a_closer_lighter_one() {
        let mut anchored = base("anchored-light", Kind::Rule);
        anchored.item.severity = Some(Severity::HouseStyle);
        anchored.item.bindings =
            vec![Binding::Target { kind: TargetKind::Path, value: "deploy/compose.yml".to_string() }];

        let mut hard = base("hard-general", Kind::Rule);
        hard.item.severity = Some(Severity::Irreversible);
        hard.item.bindings = vec![Binding::Moment(Action::Deploy)];

        let mut input = ServeInput::default();
        input.add_file("deploy/compose.yml");
        input.moments.push(Action::Deploy);

        let hits = select(&[anchored, hard], &input);
        assert_eq!(hits[0].id, "hard-general", "nothing lighter may take an irreversible warning's place");
    }

    #[test]
    fn ranking_never_falls_back_to_sentence_length() {
        // Named after the defect it prevents: an earlier version silently
        // built a length lottery into this exact tiebreaker, and it dropped
        // six of ten irreversible rules by length alone. Ten irreversible
        // items, identical severity, identical (zero) closeness, varying only
        // in text length - the cut must never correlate with length.
        let mut items = Vec::new();
        for i in 0..10 {
            let mut c = base(&format!("item-{i}"), Kind::Rule);
            c.item.severity = Some(Severity::Irreversible);
            // deliberately alternate short/long so a length-based sort would
            // reorder these away from plain id order
            c.item.text = if i % 2 == 0 { "short".repeat(1) } else { "long text ".repeat(20) };
            items.push(c);
        }
        let hits = select(&items, &input_matching_base());
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        let mut expected: Vec<String> = (0..10).map(|i| format!("item-{i}")).collect();
        expected.sort();
        let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
        assert_eq!(ids, expected, "tie order must be id order, never correlated with text length");
    }

    #[test]
    fn target_matches_is_the_only_definition_used_here() {
        // Pins the single-normalization doctrine (R6) locally: target_matches
        // must go through model::normalize, never re-implement comparison.
        assert!(target_matches(TargetKind::Path, r"A\B.rs", TargetKind::Path, "a/b.rs"));
        assert!(!target_matches(TargetKind::Path, "a/b.rs", TargetKind::Symbol, "a/b.rs"));
    }
}
