//! A replica may NEVER append to its own log.
//!
//! Its chain has to stay a pure prefix of the authority's; the moment it
//! appends something of its own, the next shipment is refused as a fork and
//! the only way back is rebuilding the replica (see `thor_core::inbox`). The
//! divert exists for exactly that: on a replica every writing tool queues the
//! CALL into the capture inbox and writes nothing locally.
//!
//! THE DEFECT THIS CLOSES, found 2026-08-17 by auditing the connector rather
//! than by anything failing: four of the eight writing tools never diverted at
//! all. `mark`, `pin`, `unpin` and `resolve` wrote straight into the replica's
//! own log. `mark` is the one that mattered most in practice - every session is
//! nudged to judge what it was served, so it is the write an away-from-the-desk
//! agent makes most often, and it was forking the chain every time.
//!
//! Nothing failed loudly when that happened, which is why this is a structural
//! test on the source and not a behavioural one: a new writing tool that
//! forgets to divert must fail HERE, at the moment it is written, rather than
//! on the day a shipment is refused.

use std::path::Path;

/// Every tool that appends to the log or files into the library. A tool that
/// belongs here and does not divert is the bug this test exists to catch.
const WRITING_TOOLS: &[&str] =
    &["remember", "revise", "retract", "pin", "unpin", "resolve", "mark", "shelve"];

/// Every tool that only reads. Queueing one of these would be nonsense: the
/// caller is waiting for the answer now, and a queued read answers nobody.
const READING_TOOLS: &[&str] =
    &["lookup", "get", "history", "status", "search_code", "where_used", "outline", "library"];

/// The body of each `async fn <tool>` in this crate's lib.rs, keyed by name.
fn tool_bodies() -> std::collections::BTreeMap<String, String> {
    let lib_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("lib.rs");
    let text = std::fs::read_to_string(&lib_rs).unwrap();
    let mut bodies = std::collections::BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(at) = line.find("    async fn ") {
            let rest = &line[at + "    async fn ".len()..];
            let name = rest.split(['(', ' ']).next().unwrap_or_default().to_string();
            if let Some((prev, body)) = current.take() {
                bodies.insert(prev, body);
            }
            current = Some((name, String::new()));
            continue;
        }
        if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some((prev, body)) = current {
        bodies.insert(prev, body);
    }
    bodies
}

#[test]
fn every_writing_tool_diverts_to_the_capture_inbox_before_it_writes() {
    let bodies = tool_bodies();
    let mut offenders = Vec::new();
    for tool in WRITING_TOOLS {
        let Some(body) = bodies.get(*tool) else {
            offenders.push(format!("{tool}: no such tool function - rename or remove it from this list"));
            continue;
        };
        let needle = format!("self.capture(\"{tool}\"");
        if !body.contains(&needle) {
            offenders.push(format!("{tool}: writes without diverting - a replica running this forks its own chain"));
        }
    }
    assert!(
        offenders.is_empty(),
        "every writing tool must call self.capture(\"<its own name>\") before it touches anything: {offenders:#?}"
    );
}

/// The divert has to be the FIRST thing the tool does. Capturing after a write
/// would leave the replica having written AND queued - the same fork, plus a
/// duplicate when the queue is drained.
#[test]
fn the_divert_comes_before_any_write_in_every_writing_tool() {
    let bodies = tool_bodies();
    let mut offenders = Vec::new();
    for tool in WRITING_TOOLS {
        let Some(body) = bodies.get(*tool) else { continue };
        let Some(capture_at) = body.find(&format!("self.capture(\"{tool}\"")) else { continue };
        // `blocking` is how every store write is made; `lib.add` is the
        // library's own. Either one before the divert is the bug.
        for write in ["self.blocking(", "lib.add("] {
            if let Some(write_at) = body.find(write) {
                if write_at < capture_at {
                    offenders.push(format!("{tool}: {write} runs before the divert"));
                }
            }
        }
    }
    assert!(offenders.is_empty(), "the divert must come first: {offenders:#?}");
}

/// A queued read would answer nobody, so the divert must stay off the read
/// side. This is the other half of the same guarantee: the list of things that
/// travel through the inbox is exactly the list of things that write.
#[test]
fn no_reading_tool_is_ever_queued() {
    let bodies = tool_bodies();
    let mut offenders = Vec::new();
    for tool in READING_TOOLS {
        let Some(body) = bodies.get(*tool) else {
            offenders.push(format!("{tool}: no such tool function"));
            continue;
        };
        if body.contains("self.capture(") {
            offenders.push(format!("{tool}: a read must never be queued - the caller is waiting for it now"));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

/// The advertised list and the real one must be the same list. If they drift,
/// the drain's version guard either blocks on a tool it could actually replay,
/// or waves through one it cannot - and the second of those loses writes.
#[test]
fn the_replayable_list_matches_what_the_authority_can_actually_replay() {
    let mut declared: Vec<&str> = mcp::REPLAYABLE_TOOLS.to_vec();
    declared.sort_unstable();
    let mut writing: Vec<&str> = WRITING_TOOLS.to_vec();
    writing.sort_unstable();
    assert_eq!(
        declared, writing,
        "REPLAYABLE_TOOLS is what a drain checks a queued batch against, so it must be exactly the \
         set of tools that queue"
    );
}

/// The guard has to see a tool from the future as unreplayable, and everything
/// it does know as fine. This is the check that decides whether a whole batch
/// is held back, so it gets its own test rather than riding along on another.
#[test]
fn a_tool_this_build_does_not_know_is_reported_as_unreplayable() {
    let op = |tool: &str| thor_core::inbox::InboxOp::new(tool, serde_json::json!({}));
    assert!(mcp::unreplayable_tools(&[op("remember"), op("shelve")]).is_empty());
    assert_eq!(
        mcp::unreplayable_tools(&[op("remember"), op("uit-de-toekomst"), op("uit-de-toekomst")]),
        vec!["uit-de-toekomst"],
        "named once, however often it appears"
    );
}

/// Every captured tool must be replayable at the authority. A call that queues
/// but has no arm in `apply_captured` is worse than one that never queued: the
/// agent is told it was saved, and the drain then reports an unknown tool.
#[test]
fn every_captured_tool_can_be_replayed_by_the_authority() {
    let lib_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("lib.rs");
    let text = std::fs::read_to_string(&lib_rs).unwrap();
    let (_, replay) = text.split_once("pub async fn apply_captured").expect("apply_captured must exist");
    let mut missing = Vec::new();
    for tool in WRITING_TOOLS {
        if !replay.contains(&format!("\"{tool}\" =>")) {
            missing.push(*tool);
        }
    }
    assert!(
        missing.is_empty(),
        "these queue on a replica but the authority cannot replay them, so they would be accepted and then lost: {missing:?}"
    );
}
