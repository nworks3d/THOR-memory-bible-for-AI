//! Surface 4 has two doors: the `lookup` MCP tool (what an agent uses) and
//! the `serve search` CLI (what a person uses at a terminal). They must
//! answer the same question the same way.
//!
//! THE DEFECT THIS PREVENTS, found on the owner's first real day with 2.0
//! (2026-08-03). His own benchmark for the whole tool is "ask me about the
//! BBQ recipes in my memory, in the middle of a coding session". Asked
//! through the agent for "bbq recept" it answered NOTHING. The identical
//! question at the terminal answered fine.
//!
//! Neither door was broken on its own. They had simply drifted: the CLI
//! called `lookup::search_best_effort` (letter AND meaning), the MCP tool
//! called `lookup::search` (letter only, a substring match on the WHOLE
//! query string). No stored item contains the literal text "bbq recept", so
//! the two-word question found nothing - while every single-word question
//! kept working, which is exactly what makes this kind of drift survive.
//! The agent is the door he actually uses.
//!
//! This is the R6 family: one behaviour, two implementations, free to
//! disagree. A grep is the same instrument this workspace already uses for
//! `Kind::can_fire` and for the normalisation rule, and for the same reason -
//! the property is "there is no second way of doing this", which no
//! ordinary unit test can observe.

use std::path::Path;

fn read(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Both doors must reach the meaning-aware entry point. A door that calls
/// only `lookup::search` answers a natural question with silence.
#[test]
fn the_agent_door_reaches_the_same_search_as_the_terminal_door() {
    let mcp = read("src/lib.rs");
    let cli = read("../serve/src/bin/serve.rs");

    assert!(
        cli.contains("lookup::search_best_effort"),
        "the CLI door must search on meaning as well as the letter"
    );
    assert!(
        mcp.contains("lookup::search_best_effort"),
        "the MCP `lookup` tool must search on meaning as well as the letter - \
         if this fails, an agent asking a two-word question gets silence while \
         the terminal answers fine, which is how this defect hid in the first place"
    );
}

/// The literal-only call may still exist - it is the honest fallback when no
/// sidecar or model is present - but never as the ONLY thing the tool can do.
/// This pins the fallback as a fallback.
///
/// `best` counts both the plain and the cached entry point
/// (`search_best_effort`/`search_best_effort_cached`): the MCP server calls
/// the cached one so a long-lived process reuses an already-loaded embedder
/// instead of reloading it on every `lookup` (GAP 1) - still the same
/// meaning-aware call this test is pinning, just through the entry point
/// that lets a caller keep the load cached across calls.
#[test]
fn the_literal_search_survives_only_as_the_no_model_fallback() {
    let mcp = read("src/lib.rs");
    let plain = mcp.matches("serve::lookup::search(").count();
    let best = mcp.matches("serve::lookup::search_best_effort(").count()
        + mcp.matches("serve::lookup::search_best_effort_cached(").count();
    assert!(
        best >= 1,
        "the meaning-aware call must be present at least once (found {best})"
    );
    assert!(
        plain <= 2,
        "the literal search should appear only as the no-vectors/no-feature \
         fallback beside it, found {plain} call(s) - a third one is most \
         likely a new path that forgot about meaning"
    );
}
