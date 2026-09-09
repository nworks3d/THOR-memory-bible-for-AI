//! The Response Guard must never fire inside a Task-tool subagent for a rule
//! that is marked `owner_reading_only: true` in the rulebook - a rule about
//! the SHAPE of a reply (length, a summary first, evidence for a claim) for
//! the OWNER's own reading, as opposed to whether the agent told the truth.
//! Decided by the owner 2026-09-09.
//!
//! Two of the rulebook's own rules are not about how a reply reads to him but
//! about whether the agent told the truth - a claim that something was
//! checked or verified with no evidence, a claim that something could not be
//! reached without having tried - and those keep catching a subagent exactly
//! as they catch his own main session; a subagent that lies about either is
//! the same lazy-agent behaviour this whole project exists to catch,
//! regardless of who reads the lie. So the exemption lives on
//! `owner_reading_only`, one optional field per rule (see
//! `serve/src/respond.rs`'s own "reader scope" doc comment, above
//! `evaluate_opt_in`, for the field and its default): a rule marked `true` is
//! skipped for a subagent payload; a rule that declares nothing - every rule
//! shipped before this field existed - keeps applying to everyone, subagent
//! included, on the theory that a gate going quiet is the worst failure
//! class this whole project exists to catch. The tests below that use a
//! rulebook rule marked `owner_reading_only`, proven here against the real
//! compiled `serve hook` binary, prove that exemption is real and scoped
//! (the owner's own main session still works exactly as before, so this
//! narrows one thing and is not a kill switch for the whole guard); the ones
//! further down prove an unmarked rule and a malformed `owner_reading_only`
//! value both still catch a subagent. Mirrors
//! `capture_guard_subagent_gate.rs`'s own shape for the same reason.
//!
//! Also proves the other half of the same 2026-09-09 decision: agents working
//! among themselves are not held by any Stop-time memory-upkeep debt, only
//! what is addressed to the owner is. Paying the crowding debt, the
//! judgement debt or the false-proof (stale-rule) debt is itself a
//! `mark`/`revise`/`retract` through the tool server, which stamps every
//! write it makes with a session identity a subagent shares with its own
//! main session, so a subagent settling one writes a record that later reads
//! as though the owner dealt with it himself, though he never read the item
//! at all - a false record, worse than the gap silence leaves; and holding a
//! subagent's own turn for this memory's maintenance spends a whole agent run
//! on upkeep he will never see asked or answered. So all five (Lane C, the
//! fourth debt (`setup_debt_stop_hook.rs`), and the three below) share the
//! same exemption, each deciding it independently at its own call site in
//! `hook_once`'s `Stop` arm, rather than behind one shared early return - see
//! `serve/src/bin/serve.rs`'s own `is_subagent` doc comment for why: a shared
//! early return across several gates is the exact defect this project has
//! already measured twice under a different name, the loop guard that
//! blinded a whole hook. Each test below proves its own gate independently,
//! the same way the code decides it independently.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BLOCK_TIER_RULEBOOK: &str = r#"[
  {"id":"hard-block","tier":"block","owner_reading_only":true,
   "any_of":["dat nooit meer doen"],"none_of":["any_of"],
   "reminder":"this is a hard rule, not a suggestion"}
]"#;

const BLOCK_MESSAGE: &str = "dat nooit meer doen, begrepen?";

/// The same rule as `BLOCK_TIER_RULEBOOK`, minus `owner_reading_only` -
/// declares nothing about reader scope at all, the shape every rule shipped
/// before this field existed, and the shape most of the owner's own live
/// rulebook still has (honesty rules included).
const UNMARKED_BLOCK_TIER_RULEBOOK: &str = r#"[
  {"id":"unmarked-hard-block","tier":"block",
   "any_of":["dat nooit meer doen"],"none_of":["any_of"],
   "reminder":"this rule declares no owner_reading_only key at all"}
]"#;

/// Same trigger phrase again, this time with a MALFORMED `owner_reading_only`
/// (a JSON string, not a bool) - must not crash the guard and must be read
/// as not-marked, the same as `UNMARKED_BLOCK_TIER_RULEBOOK` above.
const MALFORMED_OWNER_READING_ONLY_RULEBOOK: &str = r#"[
  {"id":"malformed-owner-reading","tier":"block","owner_reading_only":"yes please",
   "any_of":["dat nooit meer doen"],"none_of":["any_of"],
   "reminder":"owner_reading_only is a string here, not a bool"}
]"#;

fn run_hook(db: &Path, payload: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

fn session_start_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
    })
    .to_string()
}

fn subagent_stop_payload(session_id: &str, last_assistant_message: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": last_assistant_message,
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
    })
    .to_string()
}

fn main_session_stop_payload(session_id: &str, last_assistant_message: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": last_assistant_message,
    })
    .to_string()
}

// -------------------------------------------------------- the fix itself

/// THE DEFECT THIS PREVENTS: a subagent's own reply breaks a rule marked
/// `owner_reading_only` and gets refused turn end over it - a style rule
/// written about the owner's own reading experience, misapplied to an agent
/// whose report is read (and judged) by the main session instead.
#[test]
fn a_subagent_reply_that_breaks_a_block_tier_rule_is_never_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), BLOCK_TIER_RULEBOOK).unwrap();

    let out = run_hook(&db, &subagent_stop_payload("s1", BLOCK_MESSAGE));
    assert!(out.trim().is_empty(), "a subagent's Stop must never be blocked by an owner_reading_only rule: {out}");
}

/// Scoped, not a kill switch: the SAME rulebook and the SAME message, minus
/// `agent_id`, still blocks the owner's own main session in the SAME store -
/// `owner_reading_only` never narrows what his own main session sees.
#[test]
fn the_owners_own_main_session_is_still_blocked_by_the_same_rule() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), BLOCK_TIER_RULEBOOK).unwrap();

    let out = run_hook(&db, &main_session_stop_payload("s1", BLOCK_MESSAGE));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "the owner's own main session must still be blocked normally: {out}");
    assert!(v["reason"].as_str().unwrap().contains("hard rule"), "{out}");
}

/// THE DEFECT THIS PREVENTS, from the SECOND pass of the same day's fix (see
/// this file's own module doc comment): a rule that declares NO
/// `owner_reading_only` key at all must still catch a subagent - the
/// opposite of the first pass's blanket skip, and the whole reason the fix
/// was narrowed to a per-rule field instead of staying a per-payload one.
/// This is the shape the owner's own four honesty rules
/// (`claim-no-access-without-checking`, `checked-claim-needs-evidence`, and
/// their two siblings) are in today: nothing marks them, so nothing exempts
/// them.
#[test]
fn a_subagent_reply_that_breaks_an_unmarked_rule_is_still_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), UNMARKED_BLOCK_TIER_RULEBOOK).unwrap();

    let out = run_hook(&db, &subagent_stop_payload("s1", BLOCK_MESSAGE));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "an unmarked rule must still block a subagent: {out}");
    assert!(v["reason"].as_str().unwrap().contains("no owner_reading_only key"), "{out}");
}

/// The other half of the same proof: the IDENTICAL unmarked rulebook and
/// message, run through the owner's own main session, block exactly as any
/// rulebook rule already did before `owner_reading_only` existed - proving
/// this rulebook shape (declares nothing) behaves exactly as it always has
/// for the main session, and now ALSO reaches a subagent, rather than the
/// field having changed the main-session behaviour by accident.
#[test]
fn the_same_unmarked_rulebook_still_blocks_the_main_session_exactly_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), UNMARKED_BLOCK_TIER_RULEBOOK).unwrap();

    let out = run_hook(&db, &main_session_stop_payload("s1", BLOCK_MESSAGE));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("no owner_reading_only key"), "{out}");
}

/// THE DEFECT THIS PREVENTS: a malformed `owner_reading_only` value (here, a
/// JSON string instead of a bool) must neither crash the guard nor be read
/// as `true` by accident - it must be treated exactly like the key being
/// absent, so it still catches a subagent. Proven end to end here, against
/// the real compiled binary (`respond::parse_opt_in_rules`'s own unit test,
/// `a_malformed_owner_reading_only_value_is_read_as_not_marked`, proves the
/// same claim at the pure-matcher level); the guard's existing fail-open
/// stance on an unreadable RULEBOOK FILE is a different case (zero rules,
/// nothing fires at all) from a malformed VALUE inside one rule that DOES
/// otherwise parse, which is what this proves.
#[test]
fn a_malformed_owner_reading_only_value_still_blocks_a_subagent() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.path().join("guard-response-rulebook.json"), MALFORMED_OWNER_READING_ONLY_RULEBOOK).unwrap();

    let out = run_hook(&db, &subagent_stop_payload("s1", BLOCK_MESSAGE));
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("a malformed owner_reading_only must not crash the hook, and must still block: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
}

// ------------------------------------------------- the other debts, silenced too

/// A crowded pool to write into: `MAX_ITEMS` items of equal weight already
/// claiming one moment, so the next arrival gets the crowding note rather
/// than an outright refusal - the same fixture shape
/// `serve/src/bin/serve.rs`'s own `judgement_debt_tests::crowd_a_moment`
/// uses, copied here because that module is private to the binary and this
/// file can only reach the `model`/`serve` LIBRARY crates.
fn crowd_a_moment(store: &mut thor_core::event_store::EventStore) {
    const DISTINCT: [&str; 5] = [
        "a webhook retry backs off before it gives up entirely",
        "the estimator rounds a quote up to whole cents",
        "a spool label carries the batch it came from",
        "the scheduler skips a printer that is on hold",
        "an invoice number never restarts inside a year",
    ];
    for i in 0..model::item::MAX_ITEMS {
        let item = model::item::Item {
            id: format!("holder-{i}"),
            kind: model::item::Kind::Rule,
            text: DISTINCT[i % DISTINCT.len()].to_string(),
            bindings: vec![model::item::Binding::Moment(intent::Action::Deploy)],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("holder {i} turns out not to matter")),
            check: None,
        };
        model::store::declare(store, "earlier", "earlier", "t", &item).expect("fixture must store");
    }
}

fn crowded_newcomer(id: &str) -> model::item::Item {
    model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: "a shipment label is printed once and never reprinted silently".to_string(),
        bindings: vec![model::item::Binding::Moment(intent::Action::Deploy)],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a label is reprinted without anyone noticing".to_string()),
        check: None,
    }
}

/// THE DEFECT THIS PREVENTS, reversing this test's own former assertion:
/// decided by the owner 2026-09-09, later the same day the per-gate split
/// above first shipped. Settling this debt (fold, re-anchor, or the
/// `crowded-on-purpose` tag) is a `revise`/`retract` through the tool
/// server, which stamps it with a session identity a subagent shares with
/// its own main session - so a subagent paying it would write a record that
/// later reads as though the owner dealt with it himself, and holding its
/// turn for memory upkeep he will never see costs a whole agent run for
/// nothing. Agents working among themselves are not held by this debt.
#[test]
fn a_subagents_stop_is_silent_for_the_crowding_debt() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store);
    }
    // What SessionStart does: mark where the log stood when this session
    // began - crowding_debt refuses to speak at all without this watermark.
    run_hook(&db, &session_start_payload("s1"));
    {
        let mut store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("mine")).unwrap();
    }

    let out = run_hook(&db, &subagent_stop_payload("s1", ""));
    assert!(out.trim().is_empty(), "a subagent's Stop must never be held by the crowding debt: {out}");
}

/// Scoped, not a kill switch: the IDENTICAL store and payload, minus
/// `agent_id`, still blocks the owner's own main session on the crowding
/// debt exactly as it did before this task (see `crowding_debt` itself,
/// unchanged).
#[test]
fn the_owners_own_main_session_still_gets_the_crowding_debt() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store);
    }
    run_hook(&db, &session_start_payload("s1"));
    {
        let mut store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("mine")).unwrap();
    }

    let out = run_hook(&db, &main_session_stop_payload("s1", ""));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("mine"), "must name the crowded item: {reason}");
    assert!(reason.contains("FOLD"), "must be the crowding debt's own message: {reason}");
}

fn declare_watched_rule(store: &mut thor_core::event_store::EventStore, id: &str) {
    let item = model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: "a rule this fixture serves over and over".to_string(),
        bindings: vec![model::item::Binding::Always],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("this rule turns out not to matter".to_string()),
        check: None,
    };
    model::store::declare(store, "s", "l", "a", &item).unwrap();
}

/// THE DEFECT THIS PREVENTS, reversing this test's own former assertion:
/// same decision as the crowding debt above, and for a sharper reason -
/// paying this one calls `mark`, whose verdict is stamped with a session
/// identity a subagent shares with its own main session, so settling it
/// from inside a subagent would write a record that later reads as though
/// the owner judged the item himself, though he never read it - a false
/// record worse than the gap silence leaves. `serve::deliver::
/// record_delivery` (the same call `hook_once` itself makes on every real
/// delivery) is called directly, stamped with this test's own session id, so
/// both `served_since_last_verdict` (any session) and `served_ids_in_session`
/// (THIS session specifically) see it without needing 40 real subprocess
/// round trips.
#[test]
fn a_subagents_stop_is_silent_for_the_judgement_debt() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
        declare_watched_rule(&mut store, "watched-rule");
        for _ in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
            serve::deliver::record_delivery(&mut store, "s1", "s1", "hook", "2026-09-09T00:00:00Z", &["watched-rule".to_string()]);
        }
    }

    let out = run_hook(&db, &subagent_stop_payload("s1", ""));
    assert!(out.trim().is_empty(), "a subagent's Stop must never be held by the judgement debt: {out}");
}

/// Scoped, not a kill switch: the IDENTICAL store and served history, minus
/// `agent_id`, still blocks the owner's own main session on the judgement
/// debt exactly as it did before this task (see `judgement_debt` itself,
/// unchanged).
#[test]
fn the_owners_own_main_session_still_gets_the_judgement_debt() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
        declare_watched_rule(&mut store, "watched-rule");
        for _ in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
            serve::deliver::record_delivery(&mut store, "s1", "s1", "hook", "2026-09-09T00:00:00Z", &["watched-rule".to_string()]);
        }
    }

    let out = run_hook(&db, &main_session_stop_payload("s1", ""));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("watched-rule"), "must name the owed item: {reason}");
    assert!(reason.contains("fired repeatedly"), "must be the judgement debt's own message: {reason}");
}

/// Write `absent-guard-stale.json` directly beside the store, with exactly
/// one outstanding entry - the same fixture shape
/// `stale_guard_stop_hook.rs`'s own `write_stale_sidecar` uses.
fn write_stale_sidecar(dir: &Path, id: &str) {
    let sidecar = serde_json::json!({
        id: {
            "outcome": "failed",
            "check": "Absent { path: \"NOTES.md\", literal: \"forbidden\" }",
            "file": "NOTES.md",
            "count": 1,
            "seq_at_record": 0,
        }
    });
    std::fs::write(dir.join("absent-guard-stale.json"), sidecar.to_string()).unwrap();
}

/// THE DEFECT THIS PREVENTS, reversing this test's own former assertion:
/// same decision as the crowding debt above, and for the identical reason -
/// settling one outstanding entry here (`stale_guard::item_settled`) is
/// itself a `revise` or a `retract`, the same tool-server write the
/// crowding debt's own remediation makes, under a session identity a
/// subagent shares with the owner's own.
#[test]
fn a_subagents_stop_is_silent_for_the_false_proof_debt() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    write_stale_sidecar(dir.path(), "r1");

    let out = run_hook(&db, &subagent_stop_payload("s1", ""));
    assert!(out.trim().is_empty(), "a subagent's Stop must never be held by the false-proof debt: {out}");
}

/// Scoped, not a kill switch: the IDENTICAL sidecar, minus `agent_id`, still
/// blocks the owner's own main session on the false-proof debt exactly as it
/// did before this task (see `stale_guard_stop_check` itself, unchanged).
#[test]
fn the_owners_own_main_session_still_gets_the_false_proof_debt() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    write_stale_sidecar(dir.path(), "r1");

    let out = run_hook(&db, &main_session_stop_payload("s1", ""));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("r1"), "must name the stale item: {out}");
}

// ------------------------------------------------------ the write gate

/// LEFT UNTOUCHED BY THIS TASK, proven rather than merely asserted: the
/// write gate (`model::gate::declare`, called from `model::store::declare`
/// before anything else) takes no notion of "subagent" at all - its only
/// input is the `Item` itself, nothing from a hook payload - so a subagent
/// calling this memory's own `remember`/`revise` tool with a note over
/// `model::gate::MAX_TEXT_CHARS` (300) is refused exactly as the owner's own
/// main session already is (`model::gate`'s own `a_rule_over_300_chars_is_
/// refused`). Called through `model::store::declare`, the same entry point
/// the MCP `remember` tool itself calls, so this proves the real write
/// path, not only the pure validator underneath it - and with `crowded_
/// newcomer`'s own shape otherwise valid (a real binding, a real
/// falsifier), so the length problem is the only one that can fire.
#[test]
fn a_subagents_over_long_note_is_still_refused_by_the_write_gate() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();

    let mut item = crowded_newcomer("agent-written");
    item.text = "x".repeat(model::gate::MAX_TEXT_CHARS + 1);

    let err = model::store::declare(&mut store, "mcp", "mcp", "t", &item)
        .expect_err("an over-300-character note must be refused regardless of who wrote it");
    let message = format!("{err}");
    assert!(message.contains("300"), "{message}");
    assert!(message.contains("character"), "{message}");
}
