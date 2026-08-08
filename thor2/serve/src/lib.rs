//! The five surfaces (CONTRACT.md), never sharing a candidate pool:
//!
//! 1. `session_start` - every `Always` item, global plus the current
//!    project, full and never capped. `rank::select` never matches `Always`
//!    (see `rank::binding_matches`), so this is the ONLY door it fires
//!    through. "The current project" is derived from a filesystem location
//!    by `project::resolve_project` - the one place in the workspace that
//!    does this; a caller may still name a project explicitly instead.
//! 2. MOMENT OF ACTION - `serve()` below: `live::live_items` + `rank::select`
//!    + `render::cap`/`render::render_text`, keyed to a real command/file
//!    about to happen.
//! 3. `prompt` - PER PROMPT: `prompt::resolve` turns a raw prompt into a
//!    `ServeInput`, then the exact same select/cap/render pipeline as
//!    surface 2. A prompt that resolves to nothing selects nothing; there is
//!    no ranked fallback.
//! 4. `lookup` - OPZOEKEN: an explicit search/key request, over every
//!    project, with archive kinds (`Report`/`Chunk`) fully participating, plus
//!    a code search (`lookup::search_code`) with the commit and any
//!    working-copy drift carried on every hit. Never called on its own;
//!    never an injection surface - `lookup` is also the ONLY file in this
//!    crate allowed to reach into the code index (see this crate's own
//!    "code index reachable only via lookup" guard test under `serve/tests`).
//!    Behind the optional `semantic` feature, `lookup::search_best_effort`
//!    also ranks by MEANING (a local ONNX embedding model plus a
//!    precomputed vector sidecar, `embed`/`vectors`) - still exclusively on
//!    this one surface: `serve/tests/semantic_is_lookup_only.rs` greps this
//!    crate's own source and fails if a similarity score's machinery ever
//!    turns up outside `lookup.rs`/`embed.rs`/`vectors.rs`. A build without
//!    the feature, or one where the model files are simply absent, degrades
//!    silently to the same plain text match `search` always did - `status`/
//!    `doctor` are the ones that say which mode is active, never `lookup`
//!    itself (see `semantic_paths::mode_line`).
//! 5. `model::gate` - SCHRIJVEN: unchanged by this crate.
//!
//! `audit` is the odd one out by design: it is a query over declared items
//! and their `ItemServed` history (`audit::audit_rows`), not a function of
//! any particular moment, prompt or target, so it does not go through
//! `rank::select` either. `status` folds the same reads into the numbers a
//! person asks for first ("how many of what, how much never fired").
//!
//! Two failure policies apply at the channel boundary, never inside these
//! functions (R5): `hook` (see `bin/serve.rs`) fails open - silent, exit 0,
//! no matter what goes wrong. `check`/`why`/`audit`/`status` are human-facing
//! tools and are allowed to report an error plainly.
//!
//! `migrate` is not a sixth surface: it is a batch caller of the one write
//! door (`model::store::declare`), for turning an existing export into a
//! filled store. It lives here (not in `model`) so it can read the final
//! result back through `live::live_items`, the same reader every other
//! surface uses.
//!
//! `decay` sits AFTER `rank::select`/`session_start::select` on every one of
//! the three injection surfaces (1, 2, 3), never as a fourth kind of
//! candidate pool: an item called noise twice since anyone last called it
//! useful (`mark::record_useful`) stops reaching them, while `lookup` (surface 4)
//! never applies it at all, so nothing decay excludes ever stops being
//! findable - see `decay`'s own doc comment for the full doctrine.
//!
//! `absent_guard` is a second lane on surface 2 alone (PreToolUse), parallel
//! to Lane C's capture guard rather than part of the select/rank/render
//! pipeline: for a `Write`/`Edit` whose target file carries a live item with
//! a still-current `Check::Absent`, it turns that fact from rendered context
//! into an actual block. The SAME lane also carries an independent LOCATION
//! prohibition (an `Irreversible` item whose own `Check::PathExists` proves a
//! path or directory it anchors is still there): a write or edit whose target
//! sits at or under that path is blocked outright, content aside. A THIRD
//! arm covers a Bash-style command instead of a file: a live Rule/
//! Orientation bound to a `Command` target, carrying the same kind of
//! still-current `Check::Absent`/`Check::AbsentAll`, blocks the tool call
//! outright when the command about to run carries a forbidden literal - see
//! `absent_guard`'s own doc comment ("THE COMMAND guard") and `bin/serve.rs`'s
//! `command_guard_block`. See `absent_guard`'s own doc comment for the full
//! doctrine on all three, and `bin/serve.rs`'s `absent_guard_block` for how
//! the first two are ordered.
//!
//! `stale_guard` reads what `absent_guard` writes but never touches surface
//! 2 itself: it is a THIRD Stop-time check (after the Response Guard and
//! Lane C), blocking turn end at most once per session when a rule
//! `absent_guard` recorded as unable to prove its own currency is still
//! outstanding. See `stale_guard`'s own doc comment, and `bin/serve.rs`'s
//! `stale_guard_stop_check` for where it sits in the Stop arm.

pub mod absent_guard;
pub mod audit;
pub mod capture;
pub mod decay;
pub mod deliver;
#[cfg(feature = "semantic")]
pub mod embed;
pub mod input;
pub mod judge;
pub mod live;
pub mod lookup;
pub mod mark;
pub mod migrate;
pub mod pins;
pub mod project;
pub mod prompt;
pub mod rank;
pub mod reentry;
pub mod render;
pub mod respond;
pub mod semantic_paths;
pub mod serving;
pub mod session_start;
pub mod stale_guard;
pub mod status;
pub mod time;
pub mod usefulness;
#[cfg(feature = "semantic")]
pub mod vectors;

use thor_core::event_store::EventStore;

/// The whole read side of the one serving path, run once: select what
/// applies, then cap it. Returns the full ranked list too, since `why` needs
/// "everything that applies" and must never re-derive it with a second
/// selection function.
pub struct Served {
    pub all: Vec<rank::RankedItem>,
    pub selection: render::Selection,
}

/// Surface 2 (moment of action): a real command/file about to happen,
/// already turned into a `ServeInput` by the caller. `live::candidates_for`
/// narrows the candidate pool to items whose binding could possibly match
/// `input` BEFORE parsing them, through the `item_binding` projection, when
/// that projection is current - see its own doc comment for why this can
/// never change the answer, only how many items get parsed to reach it; it
/// falls back to every live item otherwise. Decay is applied right after
/// `rank::select`, before `all` is even captured - a decayed item is not
/// merely withheld by the cap, it never applies here at all (see `decay`'s
/// own doc comment).
pub fn serve(store: &EventStore, input: &input::ServeInput) -> Served {
    let candidates = live::candidates_for(store, input);
    let decay = decay::DecayContext::load(store);
    let all = decay::retain_live(rank::select(&candidates, input), &decay);
    let selection = render::cap(all.clone());
    Served { all, selection }
}

/// Surface 3 (per prompt): the raw prompt text is resolved to a `ServeInput`
/// via `prompt::resolve` first - the ONLY difference from `serve()` above.
/// A prompt that resolves to no moment and no declared target produces an
/// empty `ServeInput`, and `rank::select` on an empty input selects nothing:
/// there is no separate "silent" branch to maintain, it falls out of the
/// same select() every other surface uses.
pub fn serve_prompt(store: &EventStore, prompt_text: &str) -> Served {
    let candidates = live::live_items(store);
    let input = prompt::resolve(prompt_text, &candidates);
    let decay = decay::DecayContext::load(store);
    let all = decay::retain_live(rank::select(&candidates, &input), &decay);
    let selection = render::cap(all.clone());
    Served { all, selection }
}

#[cfg(test)]
mod tests {
    use super::*;
    use intent::Action;
    use model::item::{Binding, Item, Kind, Severity};
    use model::store;

    #[test]
    fn why_can_always_make_good_on_what_the_block_promised() {
        // "2 more apply here" must be provable: why's list, minus what was
        // shown, must be exactly `withheld` in count and never fewer.
        //
        // Bound to a real Moment, not Always: `rank::select` (which `serve`
        // calls) never matches an `Always` binding any more - that binding is
        // exclusively surface 1's (session start, see `session_start`), so a
        // fixture proving the moment-surface's cap/withhold bookkeeping must
        // fire through a real Moment/Target match instead.
        let mut db = EventStore::in_memory().unwrap();
        for i in 0..6 {
            let item = Item {
                id: format!("i{i}"),
                kind: Kind::Rule,
                text: format!("rule number {i}"),
                bindings: vec![Binding::Moment(Action::Configure)],
                severity: Some(Severity::Irreversible),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("rule number {i} turns out to be wrong")),
                check: None,
            };
            store::declare(&mut db, "s", "l", "a", &item).unwrap();
        }
        let mut input = input::ServeInput::default();
        input.add_moment(Action::Configure);
        let served = serve(&db, &input);
        assert_eq!(served.all.len(), 6);
        assert_eq!(served.selection.shown.len(), 4);
        assert_eq!(served.selection.withheld, 2);
        assert_eq!(served.all.len() - served.selection.shown.len(), served.selection.withheld);
    }
}
