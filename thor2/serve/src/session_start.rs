//! Surface 1: SESSION START. Serves exactly the items bound `Always`, scoped
//! to global (no project) plus the current project - full, never capped,
//! never ranked: CONTRACT.md's promise here is "the standing rules, complete,
//! or nothing", not "the four most relevant ones".
//!
//! `Always` reaches an injection surface through exactly this door.
//! `rank::binding_matches` refuses `Always` outright (see its own doc
//! comment) precisely so the moment/prompt surfaces can never also serve it -
//! the five surfaces never take each other's places (CONTRACT.md).

use crate::live::LiveItem;
use crate::rank::RankedItem;
use model::item::Binding;

/// Every live item whose kind can fire, is bound `Always`, and whose own
/// project is either unset (global) or exactly `project` - in the same
/// deterministic id order `live::live_items` already provides (this is a
/// pure filter over that order, so no further sort is applied: "no ranking,
/// no threshold" is not just a policy here, it is the absence of a sort
/// call). A different project's `Always` item is excluded, never shown and
/// never counted.
pub fn select(candidates: &[LiveItem], project: Option<&str>) -> Vec<RankedItem> {
    candidates
        .iter()
        .filter(|c| c.item.kind.can_fire())
        .filter(|c| c.item.bindings.iter().any(|b| matches!(b, Binding::Always)))
        .filter(|c| crate::project::applies_to(c.item.project.as_deref(), project))
        .map(|c| RankedItem { id: c.id.clone(), item: c.item.clone() })
        .collect()
}

/// The block text, or `None` when nothing applies - never a truncated list:
/// every item `select` returns is rendered whole, with no `render::cap` in
/// this surface's path at all.
///
/// Each shown item's line opens with `[id]`, straight from `RankedItem.id`
/// (the event log's own entity id, not the copy carried inside the item
/// body - see `live::LiveItem`) - the same bracket form and the same field
/// `render::render_text` uses for the moment/prompt surfaces. See that
/// function's own doc comment for the defect this closes: a served fact
/// with no id forces an assistant that notices it is wrong to guess search
/// terms to find the item again before it can call `revise`. There is no
/// budget to protect the id from here, unlike that surface: this one never
/// truncates anything to begin with (see the paragraph above), so the id is
/// always additive, never a trade against how much of the body is delivered
/// (see `adding_the_id_never_shrinks_how_much_of_the_body_is_delivered`).
///
/// The `falsifier` is deliberately NOT printed here, and this is the one
/// place worth arguing about, so the argument is written down.
///
/// Its value is at WRITE time: it forces the author to name the observation
/// that would make the fact false, and refusing a rule without one is what
/// keeps the store honest. That value is already banked by the time anything
/// reaches this surface. Printing it back on every session start roughly
/// DOUBLED this block on the real store (1900 to 3700 characters) for a
/// benefit nothing has measured - and this whole rebuild exists because the
/// blocks were too noisy, so paying that with no evidence is the exact trade
/// 2.0 is against.
///
/// The line 2.0 draws instead: surfaces that PUSH at a reader stay minimal;
/// surfaces a reader ASKS carry everything. So the falsifier travels on `get`
/// (one item, on request, whole) and is counted in `status`/`doctor`, and
/// never rides along on session start, the moment of action, or the prompt.
pub fn render(items: &[RankedItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut lines: Vec<String> =
        vec![crate::render::FRAMING_LINE.to_string(), "Standing rules for this session:".to_string()];
    for ranked in items {
        lines.push(format!("- [{}] {}", ranked.id, ranked.item.text));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use intent::Action;
    use model::item::{Item, Kind, TargetKind};

    fn item(id: &str, kind: Kind, bindings: Vec<Binding>, project: Option<&str>) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind,
                text: format!("rule {id}"),
                bindings,
                severity: None,
                project: project.map(str::to_string),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    #[test]
    fn a_global_always_item_is_served_regardless_of_the_current_project() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let hits = select(&[c], Some("thor2"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn an_always_item_for_the_current_project_is_served() {
        let c = item("p1", Kind::Rule, vec![Binding::Always], Some("thor2"));
        let hits = select(&[c], Some("thor2"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn an_always_item_scoped_to_a_different_project_is_never_served() {
        // Named after the defect it prevents: session start must never leak
        // another project's standing rules into this one's session.
        let c = item("other1", Kind::Rule, vec![Binding::Always], Some("some-other-project"));
        let hits = select(&[c], Some("thor2"));
        assert!(hits.is_empty(), "a different project's Always item must never appear here");
    }

    #[test]
    fn with_no_current_project_only_global_always_items_are_served() {
        let global = item("g2", Kind::Rule, vec![Binding::Always], None);
        let scoped = item("p2", Kind::Rule, vec![Binding::Always], Some("thor2"));
        let hits = select(&[global, scoped], None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "g2");
    }

    #[test]
    fn an_item_bound_only_to_a_moment_is_never_served_here() {
        // The five surfaces never take each other's places: a Moment/Target
        // binding is surface 2/3's door, never surface 1's.
        let c = item("m1", Kind::Rule, vec![Binding::Moment(Action::Push)], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Moment-only binding must never be served by session start");
    }

    #[test]
    fn an_item_bound_only_to_a_target_is_never_served_here() {
        let c = item(
            "t1",
            Kind::Rule,
            vec![Binding::Target { kind: TargetKind::Path, value: "src/main.rs".to_string() }],
            None,
        );
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Target-only binding must never be served by session start");
    }

    #[test]
    fn a_report_bound_always_never_reaches_session_start_even_bypassing_the_write_gate() {
        // The write gate already refuses a Report with a binding; this
        // constructs one directly (as if that check had never run) to prove
        // this surface is a second, independent lock on the door by KIND -
        // mirroring rank::rs's own `a_report_can_never_reach_a_gate_even_bypassing_the_write_gate`.
        let c = item("r1", Kind::Report, vec![Binding::Always], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Report must never be selected, gate or no gate");
    }

    #[test]
    fn a_chunk_bound_always_never_reaches_session_start_even_bypassing_the_write_gate() {
        let c = item("k1", Kind::Chunk, vec![Binding::Always], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Chunk must never be selected, gate or no gate");
    }

    #[test]
    fn session_start_never_caps_even_past_the_moment_surfaces_four_item_limit() {
        // "volledig, niet afgekapt" (CONTRACT.md): render::MAX_ITEMS (4) must
        // not apply here at all - ten items must all come back.
        let items: Vec<LiveItem> =
            (0..10).map(|i| item(&format!("i{i}"), Kind::Rule, vec![Binding::Always], None)).collect();
        let hits = select(&items, None);
        assert_eq!(hits.len(), 10, "session start must never cap, unlike the moment/prompt surfaces");
        let block = render(&hits).unwrap();
        for i in 0..10 {
            assert!(block.contains(&format!("rule i{i}")), "block must contain item i{i} verbatim: {block}");
        }
    }

    #[test]
    fn no_always_items_renders_no_block() {
        assert!(render(&[]).is_none());
    }

    // ------------------------------------------------------------- framing

    /// The defect this guards against: a standing rule is written as a
    /// constraint on the OWNER's own behaviour ("never do X"), because that
    /// is what a rule is FOR - a blank subagent (no project, no prior turns
    /// to place it in) has no way to tell that apart from an actual task
    /// instruction. Same requirement as `render::render_text`'s own guard
    /// test; this surface must carry it too. See INJECTION-FRAMING.md for the
    /// incident and `render::FRAMING_LINE`'s own doc comment for the argument.
    #[test]
    fn the_pushed_block_always_opens_with_the_framing_line() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(
            block.starts_with(crate::render::FRAMING_LINE),
            "session start must open with the same framing line as every other surface: {block}"
        );
        let second_line = block.lines().nth(1).unwrap_or("");
        assert_eq!(second_line, "Standing rules for this session:", "{block}");
    }

    // ---------------------------------------------------------- falsifier

    /// The defect this guards against: a second line per item on the surface
    /// that fires at EVERY session start. Measured on the real store it took
    /// the standing-rules block from about 1900 to 3700 characters, for a
    /// benefit nothing has measured - in a system rebuilt because its blocks
    /// were too noisy. The falsifier's value is at write time and on `get`;
    /// see `render`'s own doc comment for the whole argument.
    #[test]
    fn a_falsifier_never_rides_along_on_the_pushed_block() {
        let mut c = item("f1", Kind::Rule, vec![Binding::Always], None);
        c.item.falsifier = Some("this stops holding once the store is retired".to_string());
        let hits = select(&[c], None);
        let block = render(&hits).unwrap();
        assert!(
            !block.contains("falsified by"),
            "an injection surface must not carry the falsifier: {block}"
        );
        assert!(
            !block.contains("this stops holding once the store is retired"),
            "and not its text under any other wording either: {block}"
        );
    }

    /// The item itself still renders whole, falsifier or not - the field is
    /// invisible here, never a reason to drop or shorten a rule.
    #[test]
    fn the_rule_itself_is_rendered_whole_either_way() {
        let mut with = item("f1", Kind::Rule, vec![Binding::Always], None);
        with.item.falsifier = Some("some observation".to_string());
        let without = item("f2", Kind::Rule, vec![Binding::Always], None);
        assert_eq!(without.item.falsifier, None, "fixture sanity");

        let with_text = with.item.text.clone();
        let without_text = without.item.text.clone();
        let a = render(&select(&[with], None)).unwrap();
        let b = render(&select(&[without], None)).unwrap();
        assert!(a.contains(&with_text), "the rule text must survive: {a}");
        assert!(b.contains(&without_text), "the rule text must survive: {b}");
        assert_eq!(a.lines().count(), b.lines().count(), "same shape either way");
    }

    // -------------------------------------------------------------------- id

    /// THE DEFECT THIS PREVENTS: a served fact carried no id at all, so an
    /// assistant that noticed the fact was wrong had no way to correct it
    /// directly - it had to guess search terms to find the item again first,
    /// turning a one-call `revise` into three steps. Every shown item now
    /// carries its id. Same defect `render::render_text` already closed for
    /// the moment/prompt surfaces; this closes it here too.
    #[test]
    fn a_session_start_item_shows_its_id() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(block.contains("g1"), "the block must carry the item's id: {block}");
    }

    /// Showing the id somewhere is not enough - it has to come out whole and
    /// cleanly delimited, so an assistant can lift it out and pass it
    /// straight to `revise`'s own `id` argument with no editing. Uses an id
    /// shaped like a real one (a project prefix plus a colon, matching
    /// `render.rs`'s own test of the same name) to prove the bracket, not
    /// the id's own content, is what an assistant would split on.
    #[test]
    fn the_shown_id_is_in_a_form_that_can_be_passed_straight_to_revise() {
        let c = item("thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(
            block.contains("[thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV]"),
            "the id must appear whole, inside brackets, exactly as `revise` expects it: {block}"
        );
    }

    /// THE CRITICAL DEFECT THIS PREVENTS: the id riding in on the item's own
    /// text, so adding it silently shrinks how much of the fact's body is
    /// delivered compared to before. This surface never truncates to begin
    /// with (see `render`'s own doc comment: no `render::cap` in this path
    /// at all, unlike the moment/prompt surfaces), so the id can only ever
    /// be additive here - proven directly with a long body, distinct from
    /// the id's own characters so a dropped body character could never hide
    /// inside it.
    #[test]
    fn adding_the_id_never_shrinks_how_much_of_the_body_is_delivered() {
        let mut c = item("i0", Kind::Rule, vec![Binding::Always], None);
        c.item.text = "y".repeat(300);
        let long_text = c.item.text.clone();
        let block = render(&select(&[c], None)).unwrap();
        assert!(block.contains(&long_text), "the full 300-char body must still appear unmodified: {block}");
        assert!(block.contains("[i0]"), "and the id must appear alongside it, not instead of any of it: {block}");
    }
}
