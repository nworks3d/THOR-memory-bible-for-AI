//! No MCP tool may reach an injection surface (CONTRACT.md, this crate's own
//! design note in `src/lib.rs`): session start, the moment of action, and the
//! per-prompt surface are hooks that fire on their own, unprompted - handing
//! an agent a tool that calls them itself would recreate the single ranked
//! pool 2.0 was built to remove.
//!
//! This greps every `.rs` file under `mcp/src` (library and binary alike) for
//! the exact names of those three surfaces' own entry points, mirroring the
//! grep style `serve/tests/codeindex_is_lookup_only.rs` and `model/tests/
//! single_can_fire_definition.rs` already use for this workspace's other
//! "exactly one place" / "never reachable from here" guarantees. A future
//! change that wires an injection surface into an MCP tool would have to
//! write one of these names into this crate's source to do it, so this test
//! is a structural lock on the door, not a hope.

use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every name that would mean this crate reaches an injection surface, were
/// it to appear anywhere in this crate's own source. `serve::serve` and
/// `serve::serve_prompt` are the two library entry points surfaces 2/3 use
/// (see `serve/src/lib.rs`); `session_start`, `rank::select` and
/// `prompt::resolve` are the modules/functions that decide what fires on
/// each of the three surfaces.
const FORBIDDEN_NAMES: &[&str] = &["session_start", "rank::select", "prompt::resolve", "serve::serve(", "serve_prompt"];

#[test]
fn no_tool_names_an_injection_surface_entry_point() {
    let mcp_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&mcp_src, &mut files);
    assert!(!files.is_empty(), "expected to find this crate's own source files under {}", mcp_src.display());

    let mut offenders = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for needle in FORBIDDEN_NAMES {
            if text.contains(needle) {
                offenders.push(format!("{} names '{needle}'", file.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "an MCP tool must never reach an injection surface (session_start / the moment surface / \
         the prompt surface) - found: {offenders:?}"
    );
}

/// The tool surface is a closed list, and this is where it is closed - no tool
/// arrives here by accident, and in particular no tool named after one of the
/// three injection surfaces this crate must never expose.
///
/// It went from six names to twelve on 2026-08-03, deliberately, in two steps,
/// then to fourteen the same day in a third: retract, resolve and history let
/// an agent MAINTAIN the memory instead of only filling it: a wrong fact had
/// no way out, a diverged entity stayed stuck forever, and nobody could see
/// what a fact used to say. search_code, where_used and outline answer the
/// three code questions 1.0 could answer and 2.0 could not, over the index
/// this workspace already builds from git. pin and unpin let an agent make an
/// item a standing rule, or take it down, during a session - 1.0 had both,
/// 2.0 had neither.
///
/// All eight go through `model` or `serve::lookup`, so the write gate still
/// governs everything that lands and nothing here can reach an injection
/// surface - the test above still proves that half independently.
#[test]
fn the_tool_set_is_exactly_the_closed_list() {
    let lib_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("lib.rs");
    let text = std::fs::read_to_string(&lib_rs).unwrap();
    let mut tool_fns: Vec<&str> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("#[tool(description") {
            // The next non-blank line(s) up to the `async fn` carry the
            // function name - scan forward until it is found.
            for probe in lines.by_ref() {
                if let Some(name_start) = probe.find("async fn ") {
                    let rest = &probe[name_start + "async fn ".len()..];
                    let name = rest.split(['(', ' ']).next().unwrap_or_default();
                    tool_fns.push(Box::leak(name.to_string().into_boxed_str()));
                    break;
                }
            }
        }
    }
    tool_fns.sort_unstable();
    assert_eq!(
        tool_fns,
        vec![
            "get",
            "history",
            "lookup",
            "mark",
            "outline",
            "pin",
            "remember",
            "resolve",
            "retract",
            "revise",
            "search_code",
            "status",
            "unpin",
            "where_used",
        ],
        "the tool set is a closed list: adding or removing one is a decision, so it fails here \
         until this list is changed on purpose"
    );
}
