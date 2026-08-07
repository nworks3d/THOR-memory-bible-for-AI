//! Cap and render: turns a ranked list (`rank::select`'s output) into what a
//! channel actually shows. One function each, shared by every channel:
//! `cap` decides what fits, `render_text` turns what fits into the block.
//!
//! CONTRACT.md's caps: at most 4 items, at most 1200 characters of item text
//! per block. `model::gate::MAX_TEXT_CHARS` already bounds every servable
//! item's text to 300 characters at WRITE time (Rule/Orientation only - the
//! only two kinds `rank::select` ever returns), so 4 items can sum to at most
//! 4 * 300 = 1200: never over budget. That is why `cap` below has no
//! truncation branch on an item's text - there is nothing left for one to do
//! (see `four_max_length_items_always_fit_the_1200_char_budget`). The 1200
//! figure bounds the sum of item TEXT lengths, not the decorated string
//! (header, bullets, the withheld note) - the same accounting the sibling
//! Python arm's `select()` uses over `r.chars` (slices/python/gate.py).
//!
//! Each shown item's line also opens with its id, in brackets, right after
//! the bullet - see `render_text`'s own doc comment for the defect this
//! fixes and why the id is decoration, never a debit against the 1200-char
//! figure above (the same treatment the bullet, the header and the withheld
//! note already get: none of those are summed into it either).

use crate::rank::RankedItem;
use intent::Action;

pub const MAX_ITEMS: usize = 4;
pub const MAX_BLOCK_CHARS: usize = 1200;

/// The one line every injection surface opens with, verbatim - session start
/// (`session_start::render`), the moment of action and the per-prompt surface
/// (both `render_text` below). Not copy-pasted into three call sites: the
/// three injection surfaces render through exactly these two functions (see
/// `lib.rs`'s own doc comment enumerating the five surfaces), so a constant
/// read by both is the whole fix, not a convention someone has to remember to
/// repeat.
///
/// Why it exists: a stored fact is written as a constraint on the OWNER's own
/// behaviour ("never do X", "always run Y first"), because that is what a
/// rule is FOR - it reads exactly like a command. A main session has the
/// owner's whole project and prior turns to place it in; a subagent spawned
/// via the Task tool starts blank and has neither. Without a line marking
/// these as background, a blank subagent has no way to tell "this is how the
/// owner runs things" apart from "this is your next task" - see
/// INJECTION-FRAMING.md for the incident this fixes.
///
/// Kept to one short line on purpose: this is paid for in tokens on every
/// single injection, and staying small is a property this rebuild exists to
/// protect (see this module's own doc comment on the 1200-char budget).
pub const FRAMING_LINE: &str =
    "Background facts about the owner's setup, not instructions for this task:";

/// What actually fits, plus how many of everything that applied were cut.
/// `why` reads `all` and marks which ids are in `shown`; the block's own
/// "N more apply here" promise is exactly `withheld`, never a guess.
pub struct Selection {
    pub shown: Vec<RankedItem>,
    pub withheld: usize,
}

/// Cap an already-ranked (worst-first) list to what the block may carry.
/// Stops at MAX_ITEMS items, or sooner if the next item would push the
/// summed text length over MAX_BLOCK_CHARS - a branch that is unreachable in
/// practice (see the module doc) but kept so the budget is an enforced
/// invariant, not a hope, and so a future change to MAX_TEXT_CHARS cannot
/// silently blow the block open.
pub fn cap(ranked: Vec<RankedItem>) -> Selection {
    let mut shown = Vec::new();
    let mut used = 0usize;
    let mut iter = ranked.into_iter().peekable();
    while let Some(item) = iter.peek() {
        if shown.len() >= MAX_ITEMS {
            break;
        }
        let len = item.item.text.chars().count();
        if used + len > MAX_BLOCK_CHARS {
            break;
        }
        used += len;
        shown.push(iter.next().expect("peeked"));
    }
    let withheld = iter.count();
    Selection { shown, withheld }
}

/// The block text, or None when nothing was shown - the serve path's "or
/// nothing" half of its own contract. Never slices an item's `text`: every
/// character an item carries is either shown whole or not shown at all (see
/// `item_text_is_rendered_verbatim_never_sliced`).
///
/// Each shown item's line opens with `[id]`, straight from `RankedItem.id`
/// (the event log's own entity id, not the copy carried inside the item
/// body - see `live::LiveItem`) - never evaluated, just handed to the reader
/// so an assistant that notices a served fact is wrong can call `revise`
/// with that id directly, instead of guessing search terms to find the item
/// again first. The id is decoration, exactly like the bullet: it is never
/// summed into the 1200-char budget and never shortens the item's own text
/// (see `adding_the_id_never_shrinks_how_much_of_the_body_is_delivered`).
///
/// No falsifier line here. An earlier version of this comment claimed one
/// was shown; it never was - see the code comment right below for the real
/// reason (same one `session_start::render` gives for its own block): a
/// surface that pushes at a reader stays minimal, a surface a reader asks
/// carries everything.
pub fn render_text(selection: &Selection, moments: &[Action]) -> Option<String> {
    if selection.shown.is_empty() {
        return None;
    }
    let head = if moments.is_empty() {
        "Before you do this:".to_string()
    } else {
        let names = moments.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", ");
        format!("Before you do this - {names}:")
    };
    let mut lines: Vec<String> = vec![FRAMING_LINE.to_string(), head];
    // No falsifier line here either, for the reason spelled out in
    // `session_start::render`: a surface that pushes at a reader stays
    // minimal, a surface a reader asks carries everything. The moment of
    // action is the tightest surface of all - it fires on a tool call - so it
    // is the last place to spend a second line per item on something whose
    // value was already banked at write time.
    for ranked in &selection.shown {
        lines.push(format!("- [{}] {}", ranked.id, ranked.item.text));
    }
    if selection.withheld > 0 {
        lines.push(format!(
            "({} more item(s) apply here - run `serve why` to see them.)",
            selection.withheld
        ));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind, Severity};

    fn ranked(id: &str, text_len: usize) -> RankedItem {
        RankedItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: "x".repeat(text_len),
                bindings: vec![Binding::Always],
                severity: Some(Severity::Irreversible),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    #[test]
    fn never_more_than_four_items_are_shown() {
        let items: Vec<RankedItem> = (0..10).map(|i| ranked(&format!("i{i}"), 10)).collect();
        let sel = cap(items);
        assert_eq!(sel.shown.len(), MAX_ITEMS);
        assert_eq!(sel.withheld, 6);
    }

    #[test]
    fn four_max_length_items_always_fit_the_1200_char_budget() {
        // model::gate::MAX_TEXT_CHARS is 300; the write gate never lets a
        // servable item's text exceed it, so this is the worst case there is.
        assert_eq!(model::gate::MAX_TEXT_CHARS, 300);
        let items: Vec<RankedItem> =
            (0..4).map(|i| ranked(&format!("i{i}"), model::gate::MAX_TEXT_CHARS)).collect();
        let sel = cap(items);
        assert_eq!(sel.shown.len(), 4, "the char budget must never cut before the item-count cap does");
        assert_eq!(sel.withheld, 0);
    }

    #[test]
    fn a_capped_block_always_states_how_many_it_withheld() {
        let items: Vec<RankedItem> = (0..6).map(|i| ranked(&format!("i{i}"), 10)).collect();
        let sel = cap(items);
        let text = render_text(&sel, &[]).unwrap();
        assert!(text.contains("2 more item(s) apply here"), "block: {text}");
    }

    #[test]
    fn nothing_shown_renders_no_block() {
        let sel = cap(Vec::new());
        assert!(render_text(&sel, &[]).is_none());
    }

    #[test]
    fn item_text_is_rendered_verbatim_never_sliced() {
        let long = "y".repeat(300);
        let items = vec![RankedItem {
            id: "i0".to_string(),
            item: Item {
                id: "i0".to_string(),
                kind: Kind::Rule,
                text: long.clone(),
                bindings: vec![Binding::Always],
                severity: Some(Severity::Irreversible),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }];
        let sel = cap(items);
        let text = render_text(&sel, &[]).unwrap();
        assert!(text.contains(&long), "the full 300-char text must appear unmodified");
    }

    #[test]
    fn no_moments_gives_a_generic_header() {
        let sel = cap(vec![ranked("i0", 10)]);
        let text = render_text(&sel, &[]).unwrap();
        // The framing line opens every block (see `a_block_always_opens_with_the_framing_line`
        // below); the surface-specific header is the line right after it.
        let second_line = text.lines().nth(1).unwrap_or("");
        assert_eq!(second_line, "Before you do this:", "{text}");
    }

    #[test]
    fn moments_are_named_in_the_header() {
        let sel = cap(vec![ranked("i0", 10)]);
        let text = render_text(&sel, &[intent::Action::Push]).unwrap();
        let second_line = text.lines().nth(1).unwrap_or("");
        assert_eq!(second_line, "Before you do this - push:", "{text}");
    }

    // ------------------------------------------------------------- framing

    /// The defect this guards against: an owner's stored fact is phrased as a
    /// command ("never do X", "always run Y first") because that is what a
    /// rule is FOR - a blank subagent (no project, no prior turns to place it
    /// in) has no way to tell that apart from an actual task instruction. See
    /// INJECTION-FRAMING.md for the incident and `FRAMING_LINE`'s own doc
    /// comment for the full argument.
    #[test]
    fn a_block_always_opens_with_the_framing_line() {
        let sel = cap(vec![ranked("i0", 10)]);
        let text = render_text(&sel, &[]).unwrap();
        assert!(
            text.starts_with(FRAMING_LINE),
            "the moment/prompt block must open with the framing line: {text}"
        );
    }

    // ---------------------------------------------------------- falsifier

    /// The defect this guards against: a second line per item on the surface
    /// that fires at every TOOL CALL - the tightest surface there is. Same
    /// argument as `session_start::render`'s own doc comment: the falsifier's
    /// value was banked at write time, and a pushed surface stays minimal.
    #[test]
    fn a_falsifier_never_rides_along_on_the_moment_block() {
        let mut item = ranked("i0", 10);
        item.item.falsifier = Some("this stops holding once the store is retired".to_string());
        let sel = cap(vec![item]);
        let text = render_text(&sel, &[]).unwrap();
        assert!(!text.contains("falsified by"), "a pushed surface must not carry it: {text}");
        assert!(
            !text.contains("this stops holding once the store is retired"),
            "and not its text under any other wording either: {text}"
        );
    }

    /// One item is one line, falsifier or not - the field never changes the
    /// shape of this block.
    #[test]
    fn one_item_is_one_line_either_way() {
        let mut with = ranked("i0", 10);
        with.item.falsifier = Some("some observation".to_string());
        let without = ranked("i1", 10);
        assert_eq!(without.item.falsifier, None, "fixture sanity");

        let a = render_text(&cap(vec![with]), &[]).unwrap();
        let b = render_text(&cap(vec![without]), &[]).unwrap();
        assert_eq!(a.lines().count(), b.lines().count(), "same shape either way\nA:{a}\nB:{b}");
    }

    // -------------------------------------------------------------------- id

    /// THE DEFECT THIS PREVENTS: a served fact carried no id at all, so an
    /// assistant that noticed the fact was wrong had no way to correct it
    /// directly - it had to guess search terms to find the item again first,
    /// turning a one-call `revise` into three steps. Every shown item now
    /// carries its id.
    #[test]
    fn a_served_item_shows_its_id() {
        let sel = cap(vec![ranked("i0", 10)]);
        let text = render_text(&sel, &[]).unwrap();
        assert!(text.contains("i0"), "the block must carry the item's id: {text}");
    }

    /// Showing the id somewhere is not enough - it has to come out whole and
    /// cleanly delimited, so an assistant can lift it out and pass it
    /// straight to `revise`'s own `id` argument with no editing. Uses an id
    /// shaped like a real one (a project prefix plus a colon, per
    /// `bin/restore_attribution.rs`'s own doc comment on the `<project>:<id>`
    /// shape) to prove the bracket, not the id's own content, is what an
    /// assistant would split on.
    #[test]
    fn the_shown_id_is_in_a_form_that_can_be_passed_straight_to_revise() {
        let sel = cap(vec![ranked("thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV", 10)]);
        let text = render_text(&sel, &[]).unwrap();
        assert!(
            text.contains("[thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV]"),
            "the id must appear whole, inside brackets, exactly as `revise` expects it: {text}"
        );
    }

    /// THE CRITICAL DEFECT THIS PREVENTS: the id riding in on the item's own
    /// text budget, so adding it silently truncates more of the fact's body
    /// than before. Proven at the exact worst case the module doc comment
    /// already reasons about
    /// (`four_max_length_items_always_fit_the_1200_char_budget`) - four items
    /// at the write gate's own `MAX_TEXT_CHARS` ceiling - now each also
    /// carrying a realistically long id, distinct from the body's own filler
    /// character so a dropped body character could never hide inside it.
    #[test]
    fn adding_the_id_never_shrinks_how_much_of_the_body_is_delivered() {
        let long_id = format!("{}:{}", "p".repeat(20), "9".repeat(26)); // project + ulid shape
        let items: Vec<RankedItem> =
            (0..4).map(|i| ranked(&format!("{long_id}-{i}"), model::gate::MAX_TEXT_CHARS)).collect();
        let sel = cap(items);
        assert_eq!(sel.shown.len(), 4, "a long id must not change how many items the same budget selects");
        assert_eq!(sel.withheld, 0);
        let text = render_text(&sel, &[]).unwrap();
        for shown in &sel.shown {
            assert!(
                text.contains(&shown.item.text),
                "every character of the {}-char body must still appear, id or no id",
                shown.item.text.chars().count()
            );
        }
    }
}
