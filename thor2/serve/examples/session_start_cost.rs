//! Read-only cost measurement for surface 1 (SESSION START). Nobody has
//! measured what `serve::session_start`'s block currently costs: its own doc
//! comment says it is "full, never capped, never ranked" - the sibling
//! moment/prompt surfaces are capped at `render::MAX_ITEMS` (4) and
//! `render::MAX_BLOCK_CHARS` (1200), a budget `render.rs`'s own module doc
//! comment argues for in detail, and session start never got that same
//! discipline even though it is the one surface that fires at every single
//! session start. This tool measures the block; it does not decide anything
//! about it - no recommendation, no verdict, just the numbers.
//!
//! RUNS THE REAL PIPELINE, not an approximation of it. In order:
//!   1. `serve::live::always_candidates` - the candidate pool.
//!   2. `serve::session_start::select` - kind/binding/project selection.
//!   3. `serve::decay::DecayContext::load` + `serve::decay::retain_live` -
//!      drop anything called noise twice and never marked useful.
//!   4. `serve::session_start::render` - the block text itself.
//! This is exactly the sequence `bin/serve.rs`'s own `SessionStart` hook
//! branch runs, and exactly what its `cmd_session_start` preview command
//! (`serve session-start`) runs too - not a reimplementation of either.
//! `session_start::select` never sorts (see its own doc comment: "a pure
//! filter... no further sort is applied"), and `retain_live` is
//! order-preserving, so the order this tool measures over is the same order
//! a real session would actually be shown.
//!
//! READ-ONLY. SSC_DB must point at the path of a COPY of a store, never the
//! live one - required, with no default fallback; unset prints why and exits
//! before opening anything. Opens through `EventStore::open_existing`, the
//! same constructor `stale_anchors.rs` uses and documents as the closest
//! thing this crate exposes to a read-only open (no schema migration, no FTS
//! heal on open, unlike `EventStore::new`). Nothing in this file ever calls a
//! write function (`model::store::declare`/`revise`, `serve::mark`,
//! `serve::deliver::record_delivery`, ...) - only the four reads above.
//!
//! SSC_PROJECT is optional and supplies the project key directly, the same
//! role a resolved "cwd" plays for a real session (see
//! `serve::project::resolve_project` and `project::applies_to`). Unset means
//! no current project - global-only, exactly like a session whose project
//! could not be resolved. The project actually used is always printed first.
//!
//! WHAT "median" AND "largest" MEAN. Per-item size is each item's own TEXT
//! length in characters - the same accounting `render.rs`'s own 1200-char
//! budget argument uses (the sum of item TEXT lengths, never the decorated
//! "- [id] text" line: see that module's doc comment). The rendered block's
//! own total character count, printed separately above those two numbers, IS
//! the decorated string, straight from the real `render` call - the two
//! numbers measure different things on purpose, and neither is derived from
//! the other.
//!
//! THE TOKEN ESTIMATE IS NOT MEASURED. No tokenizer runs anywhere in this
//! tool. It divides the real rendered block's character count by
//! `CHARS_PER_TOKEN_ESTIMATE`, a fixed, named assumption (4 characters per
//! token, the commonly cited rough ratio for English prose) - printed
//! alongside the number every time, never silently.
//!
//! THE WHAT-IF TABLE IS NOT A PROPOSAL. It answers one question only: over
//! the exact order `select`/`retain_live` already produced, what would the
//! first N items alone render to, for N in 5, 10, 15, 20 - the real
//! `session_start::render` called on a truncated slice, not a reimplemented
//! renderer and not the moment surface's own char-budget cap algorithm
//! (`render::cap`, which this surface never runs through at all). No item is
//! ever reordered to build this table.
//!
//! Run (from the `thor2` workspace root):
//!   SSC_DB=<path to a COPY of a store> [SSC_PROJECT=<project key>] \
//!   cargo run -p serve --example session_start_cost

use serve::decay::{self, DecayContext};
use serve::live;
use serve::rank::RankedItem;
use serve::session_start;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// The one assumption every token number in this tool rests on, named so it
/// is never a silent guess. Not a measured value: no tokenizer runs here.
const CHARS_PER_TOKEN_ESTIMATE: f64 = 4.0;

/// The item-count caps the what-if table reports on, in the order requested.
const CAPS: [usize; 4] = [5, 10, 15, 20];

/// Median and largest of each item's own TEXT length (characters, not the
/// decorated rendered line - see this file's own doc comment for why). Also
/// returns the id of whichever item carries the largest text, so the number
/// is traceable back to a real item. `None` when `items` is empty.
fn text_length_stats(items: &[RankedItem]) -> (f64, Option<(usize, String)>) {
    if items.is_empty() {
        return (0.0, None);
    }
    let mut lengths: Vec<usize> = items.iter().map(|r| r.item.text.chars().count()).collect();
    lengths.sort_unstable();
    let mid = lengths.len() / 2;
    let median = if lengths.len() % 2 == 0 {
        (lengths[mid - 1] + lengths[mid]) as f64 / 2.0
    } else {
        lengths[mid] as f64
    };
    let largest_len = *lengths.last().expect("just checked items is non-empty");
    let largest_id = items
        .iter()
        .find(|r| r.item.text.chars().count() == largest_len)
        .map(|r| r.id.clone())
        .expect("some item must carry the maximum length found above");
    (median, Some((largest_len, largest_id)))
}

/// One row of the what-if table: `session_start::render`, called on the
/// first `cap` items of the REAL post-select, post-decay list, in the order
/// that list already has - never re-sorted, never re-ranked. `ids_beyond_cap`
/// is exactly the ids `render` never saw for this row.
struct CapRow {
    shown: usize,
    block_chars: usize,
    ids_beyond_cap: Vec<String>,
}

fn cap_row(items: &[RankedItem], cap: usize) -> CapRow {
    let shown = items.len().min(cap);
    let block_chars = session_start::render(&items[..shown]).map(|b| b.chars().count()).unwrap_or(0);
    let ids_beyond_cap = items[shown..].iter().map(|r| r.id.clone()).collect();
    CapRow { shown, block_chars, ids_beyond_cap }
}

fn main() -> anyhow::Result<()> {
    let Some(db_value) = std::env::var("SSC_DB").ok() else {
        eprintln!(
            "SSC_DB is not set. Set it to the path of a COPY of a store, never the live \
             thor.db. This tool never falls back to a default path, and nothing has been \
             opened or touched."
        );
        std::process::exit(1);
    };
    let db = PathBuf::from(db_value);
    let project = std::env::var("SSC_PROJECT").ok();

    let store = EventStore::open_existing(&db)?;

    let candidates = live::always_candidates(&store);
    let decay = DecayContext::load(&store);
    let items = decay::retain_live(session_start::select(&candidates, project.as_deref()), &decay);

    let block = session_start::render(&items).unwrap_or_default();
    let block_chars = block.chars().count();
    let (median, largest) = text_length_stats(&items);
    let est_tokens = block_chars as f64 / CHARS_PER_TOKEN_ESTIMATE;

    println!("project: {}", project.as_deref().unwrap_or("(none - global only)"));
    println!("items: {}", items.len());
    println!("rendered block characters: {block_chars}");
    match largest {
        Some((len, id)) => {
            println!("median item text length (characters): {median:.1}");
            println!("largest single item: {len} characters (id {id})");
        }
        None => {
            println!("median item text length (characters): n/a, no items");
            println!("largest single item: n/a, no items");
        }
    }
    println!(
        "estimated tokens: {est_tokens:.0} (estimate only, not a real tokenizer count; assumes \
         {CHARS_PER_TOKEN_ESTIMATE:.0} characters per token over the {block_chars}-character \
         rendered block)"
    );

    println!();
    println!(
        "what-if - this surface is never actually capped; the caps below are hypothetical only, \
         applied over the exact order session_start::select and decay::retain_live already \
         produced, not a ranking invented for this table:"
    );
    println!("{:<5} {:<6} {:<12} {:<10} ids beyond the cap", "cap", "shown", "blk_chars", "est_tok");
    for &cap in &CAPS {
        let row = cap_row(&items, cap);
        let est_tok = row.block_chars as f64 / CHARS_PER_TOKEN_ESTIMATE;
        let ids =
            if row.ids_beyond_cap.is_empty() { "(none)".to_string() } else { row.ids_beyond_cap.join(", ") };
        println!("{:<5} {:<6} {:<12} {:<10.0} {}", cap, row.shown, row.block_chars, est_tok, ids);
    }

    Ok(())
}
