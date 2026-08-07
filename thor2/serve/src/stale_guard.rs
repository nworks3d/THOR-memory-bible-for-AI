//! The stale-rule Stop guard: the THIRD Stop-time check (after the Response
//! Guard and Lane C's capture guard - see `serve/src/bin/serve.rs`'s Stop
//! arm), and the first thing that ever READS the staleness sidecar
//! `serve::absent_guard` already writes (`absent_guard::default_stale_path`,
//! `absent_guard::StaleRecord` - see that module's own "staleness sidecar"
//! section). Until this module existed, that sidecar was a one-way ledger:
//! every rule that matched a file being written but could not prove its own
//! currency right now was recorded and then never looked at again. This
//! closes that loop, at the one moment a person is guaranteed to still be
//! looking: the end of a turn.
//!
//! WHAT COUNTS AS OUTSTANDING. Every entry in the sidecar, EXCEPT one whose
//! item has been revised or retracted SINCE it was last recorded there
//! (`StaleRecord::seq_at_record` - the store's own seq at record time, the
//! same computation `capture::Marker::seq_at_flag` already uses for an
//! unrelated debt). `item_settled` below decides this exactly the way
//! `capture::debt_paid` decides whether ITS OWN debt is paid: given the
//! event kinds that happened after the recorded point, in kind membership
//! alone, with no notion of whether the action actually fixed the specific
//! problem recorded - the owner already acted on this item, and that is
//! enough, exactly the stance `capture::debt_paid` already takes for
//! `fact_created`/`fact_revised`. The two kinds differ (`FactRevised`/
//! `FactRetracted` here, `FactCreated`/`FactRevised` there) because the two
//! guards ask different questions: capture asks "was ANY fact recorded
//! since this prompt", stale asks "was THIS ALREADY-EXISTING item touched
//! since it went stale" - an item that is stale by definition already
//! exists, so a fresh `FactCreated` for its own id can never happen again
//! and has no place in this list.
//!
//! WHY "SINCE IT WAS RECORDED" NEEDS A SEQ, NOT JUST "EVER": an item that
//! was revised once, years ago, for a completely unrelated reason, must not
//! silently clear a staleness that was only observed much later - that
//! would make nearly every real item "settled" by definition, defeating the
//! guard entirely. Anchoring "since" to the record's OWN seq (stamped by
//! `absent_guard::record_stale_text`'s `now_seq` parameter every time it
//! folds a fresh occurrence in) is what keeps this precise.
//!
//! AT MOST ONCE PER SESSION, using the SAME KIND of marker-file mechanism
//! every once-per-session guard in this crate already uses (a small JSON
//! sidecar beside the store, session-keyed, read-modify-write, fail-open -
//! see `absent_guard::default_marker_path`/`capture::default_marker_path`),
//! but its OWN sidecar file (`default_marker_path` below,
//! `stale-guard-blocked.json`), never `absent_guard`'s: that one keys on
//! (session, file) for a content/location PROHIBITION, an entirely
//! different question from this guard's own (session) -> "already told"
//! bit. If the block is ignored, the marker still stands for the rest of
//! THIS session (never fires again here), but the sidecar entry itself is
//! untouched - the next SESSION reads it fresh and blocks again. That
//! repetition, across sessions, is the intended pressure; never within one.
//!
//! THE OFF SWITCH, in exactly the style of `capture::default_mode_config_path`/
//! `capture::parse_mode_config`: a small JSON file beside the store,
//! `stale-guard-mode.json`, `{"mode": "off"}` (or `"on"`, accepted but never
//! required - `resolve_mode` already defaults to `On`). This guard is ON
//! the moment the sidecar carries anything outstanding and the file is
//! simply ABSENT - deliberate, so a fresh deployment is protected without
//! anyone having to opt in - but the file is the documented way to turn it
//! off entirely, per-deployment, without deleting or editing anything else.
//!
//! FAIL-OPEN, no exception: an unreadable or malformed stale sidecar, an
//! unreadable or malformed block marker, a store that will not open, a
//! per-item event lookup that fails - every one of these lets the turn end
//! in silence, never a block. An entry whose settlement cannot be proven
//! either way is never the reason work cannot finish; see
//! `serve/src/bin/serve.rs`'s `stale_guard_stop_check` for where every one
//! of these is actually read (this module itself touches no filesystem and
//! opens no store - every function below is a pure decision over data its
//! caller already fetched, the same separation `capture`'s own pure
//! functions keep from `serve/src/bin/serve.rs`'s I/O).

use crate::absent_guard::{StaleOutcome, StaleRecord};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thor_core::event_store::EventKind;

// -------------------------------------------------------------- off switch

/// Which of the two states this guard is in - see this module's own doc
/// comment for why `On` (not `Off`) is what an ABSENT config file means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    On,
    Off,
}

/// The mode config sits beside the store, same stance as every other
/// per-deployment config file in this crate (`capture::default_mode_config_path`,
/// `absent_guard::default_marker_path`): a project directory could otherwise
/// plant one and silently flip this guard off for every session in it.
pub fn default_mode_config_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("stale-guard-mode.json")
}

/// Parse `stale-guard-mode.json`'s one key: `{"mode": "off"|"on"}`. Mirrors
/// `capture::parse_mode_config`'s own shape exactly (same one-key JSON
/// shape, same fail-open stance): missing file, malformed JSON, a missing
/// `mode` key, or any value other than the two recognized ones all yield
/// `None` - "no explicit choice was made here" - and the caller
/// (`resolve_mode`) falls through to its own default.
pub fn parse_mode_config(text: &str) -> Option<Mode> {
    let v: Value = serde_json::from_str(text).ok()?;
    let mode = v.get("mode").and_then(|m| m.as_str())?.trim().to_ascii_lowercase();
    match mode.as_str() {
        "off" => Some(Mode::Off),
        "on" => Some(Mode::On),
        _ => None,
    }
}

/// The pure mode-selection rule (I/O lives in `serve/src/bin/serve.rs`, so
/// this decision is unit-tested without touching a filesystem at all, the
/// same split `capture::resolve_capture_mode` already keeps). An explicit,
/// parseable config always wins; anything else - including no file at all -
/// defaults to `On`, which is what makes this guard protective the moment
/// it is deployed, with nobody having to opt in.
pub fn resolve_mode(mode_config_text: Option<&str>) -> Mode {
    mode_config_text.and_then(parse_mode_config).unwrap_or(Mode::On)
}

// ------------------------------------------------------ once-per-session

/// Where the once-per-session block marker lives - the same convention
/// every sidecar file in this crate follows: beside the store, never inside
/// a project directory a session could plant one in. Its OWN file, never
/// `absent_guard::default_marker_path` - see this module's own doc comment
/// for why the two must never share a key space.
pub fn default_marker_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("stale-guard-blocked.json")
}

/// Parse the marker file: the set of session ids already blocked once by
/// this guard. Malformed JSON (or a file that never existed) yields an
/// empty set - fail-open, the same stance every marker file in this crate
/// takes.
pub fn read_blocked(text: &str) -> HashSet<String> {
    serde_json::from_str(text).unwrap_or_default()
}

/// Has this session already been blocked once by this guard?
pub fn already_blocked(blocked: &HashSet<String>, session_id: &str) -> bool {
    blocked.contains(session_id)
}

/// Fold a new blocked session into whatever the marker file already held -
/// pure, mirrors `absent_guard::mark_blocked_text`'s own shape: the caller
/// does the actual read/write, this only computes the new contents from the
/// old ones. `None` only when the result cannot be serialised at all, which
/// never happens for this shape in practice.
pub fn mark_blocked_text(existing_text: Option<&str>, session_id: &str) -> Option<String> {
    let mut blocked = existing_text.map(read_blocked).unwrap_or_default();
    blocked.insert(session_id.to_string());
    serde_json::to_string(&blocked).ok()
}

// ------------------------------------------------------------- settlement

/// Mirrors `capture::debt_paid`'s own shape exactly (kind membership over a
/// slice of events, nothing more) for the OTHER pair of kinds that retire an
/// entry here: `kinds_since_recorded` must already be the caller's own
/// events for ONE item's `entity_id`, filtered to `seq` greater than that
/// item's own `StaleRecord::seq_at_record` (see `serve/src/bin/serve.rs`'s
/// `stale_guard_stop_check`, which does that filtering with
/// `EventStore::get_events_by_entity` - the same call `model::store::show`/
/// `history` already use to read one item's own history - so this stays a
/// pure predicate with no store of its own to test against). True when a
/// `FactRevised` or `FactRetracted` is anywhere among them: the owner
/// already acted on this item since the sidecar last recorded it, so it is
/// settled - whether or not that action actually fixed the specific problem
/// recorded, exactly the stance `capture::debt_paid` already takes for its
/// own two kinds.
pub fn item_settled(kinds_since_recorded: &[EventKind]) -> bool {
    kinds_since_recorded.iter().any(|k| matches!(k, EventKind::FactRevised | EventKind::FactRetracted))
}

// ------------------------------------------------------------ outstanding

/// One outstanding entry, ready to render: a stale record that is still
/// unsettled, paired with the item id it names (the sidecar's own map key -
/// `StaleRecord` itself carries no id, see `absent_guard::read_stale`).
#[derive(Debug, Clone, PartialEq)]
pub struct Outstanding {
    pub id: String,
    pub record: StaleRecord,
}

/// Every entry in `stale` whose id is NOT in `settled_ids` - the caller's
/// own per-item `item_settled` lookup, done once per id before this is
/// called (see `stale_guard_stop_check`). Sorted by id, deterministically:
/// the rendered message and every test that reads it must never depend on
/// hash order.
pub fn outstanding(stale: &HashMap<String, StaleRecord>, settled_ids: &HashSet<String>) -> Vec<Outstanding> {
    let mut out: Vec<Outstanding> = stale
        .iter()
        .filter(|(id, _)| !settled_ids.contains(id.as_str()))
        .map(|(id, record)| Outstanding { id: id.clone(), record: record.clone() })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// -------------------------------------------------------------- the block

/// The list in the rendered message never grows past this many rows - see
/// `block_message`, which says how many more there are instead.
const MAX_LISTED: usize = 10;

fn outcome_label(outcome: StaleOutcome) -> &'static str {
    match outcome {
        StaleOutcome::Failed => "FAILED",
        StaleOutcome::CouldNotRun => "COULD NOT RUN",
    }
}

/// The block message, in the exact required shape: the THOR marker; a plain
/// statement that these rules could not prove themselves while work was
/// going on, so they are either wrong now or their anchor moved; one line
/// per outstanding entry naming its id, whether the check FAILED or COULD
/// NOT RUN, the check itself, and the file that was being written when it
/// fired; capped at `MAX_LISTED` rows with a count of how many more there
/// are; and the three ways out - correct the rule, retract it, or leave it
/// and it comes back next session.
pub fn block_message(entries: &[Outstanding]) -> String {
    let total = entries.len();
    let shown = &entries[..total.min(MAX_LISTED)];
    let mut out = String::from(
        "[THOR] These rules could not prove themselves while work was going on, \
         so they are either wrong now or their anchor moved:\n",
    );
    for entry in shown {
        out.push_str(&format!(
            "- {}: {} - check: {} - file: {}\n",
            entry.id,
            outcome_label(entry.record.outcome),
            entry.record.check,
            entry.record.file,
        ));
    }
    let remaining = total - shown.len();
    if remaining > 0 {
        out.push_str(&format!("...and {remaining} more not shown.\n"));
    }
    out.push_str("Correct the rule, retract it, or leave it as is - it will come back next session.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(outcome: StaleOutcome, seq_at_record: i64) -> StaleRecord {
        StaleRecord {
            outcome,
            check: "Absent { path: \"NOTES.md\", literal: \"forbidden\" }".to_string(),
            file: "NOTES.md".to_string(),
            count: 1,
            seq_at_record,
        }
    }

    // -------------------------------------------------------- off switch

    /// THE DEFECT THIS PREVENTS: with no explicit config at all, this guard
    /// silently does nothing - the whole point is that it protects a fresh
    /// deployment without anyone having to opt in.
    #[test]
    fn no_mode_config_defaults_to_on() {
        assert_eq!(resolve_mode(None), Mode::On);
    }

    /// Fail-open on the config file itself: a malformed or empty mode config
    /// must never disable the guard by accident - only an explicit "off"
    /// may do that.
    #[test]
    fn a_malformed_or_empty_mode_config_defaults_to_on() {
        assert_eq!(resolve_mode(Some("{ not json")), Mode::On);
        assert_eq!(resolve_mode(Some("{}")), Mode::On, "no mode key at all");
        assert_eq!(resolve_mode(Some(r#"{"mode":"maybe"}"#)), Mode::On, "unrecognized value");
    }

    /// THE DEFECT THIS PREVENTS (the off switch itself): with no documented
    /// way to disable this guard, the only recourse for a false positive is
    /// editing or deleting the sidecar - this proves the documented switch
    /// actually works.
    #[test]
    fn an_explicit_off_switch_disables_the_guard() {
        assert_eq!(parse_mode_config(r#"{"mode":"off"}"#), Some(Mode::Off));
        assert_eq!(resolve_mode(Some(r#"{"mode":"off"}"#)), Mode::Off);
    }

    #[test]
    fn an_explicit_on_is_also_recognized_though_never_required() {
        assert_eq!(resolve_mode(Some(r#"{"mode":"on"}"#)), Mode::On);
    }

    // -------------------------------------------------- once-per-session

    /// THE DEFECT THIS PREVENTS: the same session gets told twice - the
    /// "once per session, ever" contract this whole guard exists to keep
    /// (see this module's own doc comment: coming back is reserved for the
    /// NEXT session, never a repeat within this one) - and the converse
    /// defect, a marker so coarse it suppresses an unrelated session too.
    #[test]
    fn a_session_that_already_blocked_is_not_blocked_again() {
        let text = mark_blocked_text(None, "s1").unwrap();
        let blocked = read_blocked(&text);
        assert!(already_blocked(&blocked, "s1"));
        assert!(!already_blocked(&blocked, "s2"), "a different session must not be suppressed");
    }

    /// Fail-open: a marker file that is missing or malformed reads back as
    /// nobody-blocked-yet, never an error.
    #[test]
    fn a_missing_or_malformed_marker_file_reads_as_empty() {
        assert!(read_blocked("").is_empty());
        assert!(read_blocked("{ not json").is_empty());
        assert!(!already_blocked(&read_blocked(""), "s1"));
    }

    // ------------------------------------------------------- settlement

    #[test]
    fn a_revised_item_is_settled() {
        assert!(item_settled(&[EventKind::FactRevised]));
    }

    #[test]
    fn a_retracted_item_is_settled() {
        assert!(item_settled(&[EventKind::FactRetracted]));
    }

    /// THE DEFECT THIS PREVENTS: an unrelated event kind (delivery,
    /// telemetry, or even a fresh `FactCreated` - which, for an id that was
    /// already stale, can never legitimately happen again) is mistaken for
    /// the owner having acted on the item.
    #[test]
    fn an_item_with_no_revise_or_retract_event_since_recorded_is_not_settled() {
        assert!(!item_settled(&[]));
        assert!(!item_settled(&[EventKind::ItemServed, EventKind::ItemMarkedUseful]));
        assert!(!item_settled(&[EventKind::FactCreated]), "a create event alone must never settle a stale entry");
    }

    // ------------------------------------------------------- outstanding

    /// THE DEFECT THIS PREVENTS: an entry whose item was settled still shows
    /// up as outstanding, or the converse - an unsettled entry is dropped.
    #[test]
    fn a_settled_id_is_excluded_and_an_unsettled_one_is_kept() {
        let mut stale = HashMap::new();
        stale.insert("r1".to_string(), sample_record(StaleOutcome::Failed, 3));
        stale.insert("r2".to_string(), sample_record(StaleOutcome::Failed, 3));
        let mut settled = HashSet::new();
        settled.insert("r1".to_string());

        let remaining = outstanding(&stale, &settled);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "r2");
    }

    #[test]
    fn nothing_settled_returns_every_entry_sorted_by_id() {
        let mut stale = HashMap::new();
        stale.insert("z".to_string(), sample_record(StaleOutcome::Failed, 1));
        stale.insert("a".to_string(), sample_record(StaleOutcome::Failed, 1));
        let remaining = outstanding(&stale, &HashSet::new());
        assert_eq!(remaining.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), vec!["a", "z"]);
    }

    #[test]
    fn everything_settled_returns_nothing() {
        let mut stale = HashMap::new();
        stale.insert("r1".to_string(), sample_record(StaleOutcome::Failed, 1));
        let mut settled = HashSet::new();
        settled.insert("r1".to_string());
        assert!(outstanding(&stale, &settled).is_empty());
    }

    // ------------------------------------------------------ block message

    /// THE DEFECT THIS PREVENTS: a block message that cannot be traced back
    /// to which rule fired, what the check said, which file was involved, or
    /// what a reader is supposed to do about it.
    #[test]
    fn the_block_message_names_the_marker_the_id_the_outcome_the_check_and_the_file() {
        let entries = vec![Outstanding { id: "r1".to_string(), record: sample_record(StaleOutcome::Failed, 3) }];
        let msg = block_message(&entries);
        assert!(msg.starts_with("[THOR]"), "{msg}");
        assert!(msg.contains("r1"), "must name the id: {msg}");
        assert!(msg.contains("FAILED"), "must name the outcome: {msg}");
        assert!(msg.contains("Absent"), "must carry the check itself: {msg}");
        assert!(msg.contains("NOTES.md"), "must name the file: {msg}");
        let lower = msg.to_ascii_lowercase();
        assert!(lower.contains("could not prove themselves"), "{msg}");
        assert!(lower.contains("wrong now") && lower.contains("anchor moved"), "{msg}");
        assert!(lower.contains("correct"), "must name the first way out: {msg}");
        assert!(lower.contains("retract"), "must name the second way out: {msg}");
        assert!(lower.contains("next session"), "must name the third way out: {msg}");
    }

    /// The two outcomes must read distinctly - a reader triaging the list
    /// needs to know whether the check ran and failed, or could not even be
    /// attempted.
    #[test]
    fn a_could_not_run_outcome_is_labeled_distinctly_from_failed() {
        let entries = vec![Outstanding { id: "r2".to_string(), record: sample_record(StaleOutcome::CouldNotRun, 0) }];
        let msg = block_message(&entries);
        assert!(msg.contains("COULD NOT RUN"), "{msg}");
        assert!(!msg.contains("r2: FAILED"), "{msg}");
    }

    /// THE DEFECT THIS PREVENTS: an unbounded list makes the block message
    /// itself unreadable - the cap must hold, and the count of what was left
    /// out must still be honest.
    #[test]
    fn the_list_is_capped_at_ten_entries_and_says_how_many_more() {
        let entries: Vec<Outstanding> = (0..13)
            .map(|i| Outstanding { id: format!("r{i:02}"), record: sample_record(StaleOutcome::Failed, 0) })
            .collect();
        let msg = block_message(&entries);
        for i in 0..10 {
            assert!(msg.contains(&format!("r{i:02}")), "entry {i} must be listed: {msg}");
        }
        assert!(!msg.contains("r10:"), "an 11th entry must not be listed: {msg}");
        assert!(msg.contains('3'), "must say how many more remain: {msg}");
    }

    /// The converse of the cap: ten or fewer entries never claims there are
    /// more left.
    #[test]
    fn ten_or_fewer_entries_never_claims_more_are_left() {
        let entries: Vec<Outstanding> = (0..10)
            .map(|i| Outstanding { id: format!("r{i:02}"), record: sample_record(StaleOutcome::Failed, 0) })
            .collect();
        let msg = block_message(&entries);
        assert!(!msg.contains("more not shown"), "{msg}");
    }
}
