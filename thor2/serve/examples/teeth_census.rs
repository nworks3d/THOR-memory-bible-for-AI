//! Read-only census: every LIVE Rule/Orientation whose severity is `costly`
//! or `irreversible` - exactly the population `ops::health::teeth_line`
//! calls "heavy" - printed one JSON object per line, with enough on each row
//! for an owner to decide what to do about it. Modelled on `check_census.rs`
//! in this same directory: same store-opening constructor, same walk over
//! `serve::live::live_items`, never a hand-written SQL query against the raw
//! event log.
//!
//! WHY THIS EXISTS. `doctor` already prints a `teeth` line: "N of M rule(s)
//! marked irreversible or costly can actually refuse something." That is a
//! COUNT, and a count alone answers nothing for the one person who actually
//! has to act on it - a new owner staring at "14 of 205" has no way to find
//! out WHICH 205, let alone which of the other 191 to write a check for
//! first. The count was also independently checked by hand: a SQL query
//! written directly against the store found only 59 live heavy items -
//! fewer than a third of `teeth_line`'s 205. A count cannot arbitrate
//! between two counts; only a list, built through the exact same fold
//! `teeth_line` itself runs, can. See this file's own `main` for that
//! fold, and see the tool's own run output for which of 205/59 the live
//! store actually backs - a hand SQL query is exactly the shortcut
//! `check_census.rs`'s own header already warns against taking, for the
//! same reason: it is a second, independent reimplementation of "what is
//! live", and the one place that question is allowed to be answered is
//! `serve::live::live_items`.
//!
//! WHAT "ANSWERED" MEANS. A heavy item is more than a claim nothing can
//! catch a violation of in exactly two ways: it carries a machine-runnable
//! `check` (`model::item::Check`, run by `model::check::run`), or it
//! carries the store's own existing exemption tag for a rule the owner has
//! already decided cannot be checked. That tag is read by
//! `model::store::teeth_answer`, the SAME function `model::gate` calls when
//! a NEW or REVISED `Rule` answers this question at write time - reused
//! here rather than re-parsed by hand so this census can never quietly
//! disagree with what the gate would accept today. A BARE `no-literal` (no
//! `:<reason>`) does not count: the gate itself only still honours a bare
//! tag on an item that already carried it before a reason was required
//! (`TeethAnswer::Bare`, grandfathered) and never accepts one as a new
//! answer, so this census holds every live item to that same current bar
//! rather than grandfathering an old bare tag in silently. A REASONED tag,
//! `no-literal:<why>`, counts only once its reason reaches
//! `model::store::NO_LITERAL_REASON_MIN` (20) characters - short of that the
//! gate's own `no_literal_reason_problem` would refuse it as "'no' with
//! extra steps", so a shorter one already in the store gets no more credit
//! here than it would get today. An item that is neither is UNANSWERED:
//! heavy, live, and nothing about it has ever been asked or answered.
//!
//! SCOPE - only a live item whose kind can fire (`Kind::can_fire`: Rule or
//! Orientation) is ever counted, the same restriction `teeth_line`,
//! `check_census` and `stale_anchors` all place on which items matter here.
//! No project filter of any kind: `serve::live::live_items` never applies
//! one, and neither does `teeth_line` - a global item is exactly as heavy as
//! a project-scoped one, and this tool must never disagree with `doctor`
//! about which items are in scope.
//!
//! Read-only. TEETH_CENSUS_DB names the store to read, with no default - a
//! missing variable exits before anything is opened, the same refusal shape
//! `check_census.rs` uses for its own variables. Opens through
//! `EventStore::open_existing` (never `new`, see `open_with_retry` below):
//! no create, no schema work, no FTS heal - the exact constructor
//! `teeth_line` itself calls, and every other inspection command (doctor,
//! fsck, status) uses directly against a live store, never a copy, for the
//! same reason this may too. A CENSUS MUST NOT REPAIR THE STORE IT IS
//! COUNTING: `new` runs schema work and an FTS heal on open, which is
//! exactly the kind of write a tool whose whole point is an honest count of
//! what is already there must never risk triggering as a side effect of
//! merely looking. `open_existing` already carries its own 5-second
//! `busy_timeout` for ordinary contention during one connection's lifetime
//! (see that constructor's own doc comment in `core/src/event_store.rs`);
//! `open_with_retry` adds a second, coarser layer on top for contention AT
//! OPEN TIME - another process (a backup, a bulk repair) holding the store
//! exclusively for longer than one busy_timeout window. This never writes,
//! creates, migrates or repairs anything in the store it opens, and unlike
//! `check_census.rs` it never touches the filesystem at all - no checkout
//! root, nothing to resolve a check against - so pointing it at the live
//! store carries no risk beyond what `doctor` itself already carries every
//! time it runs.
//!
//! Run (from the `thor2` workspace root):
//!   TEETH_CENSUS_DB=<path to a store, live or a copy> \
//!   cargo run -p serve --example teeth_census
//!
//! Add `--unanswered` to print only the unanswered rows (the worklist), not
//! every heavy row.

use model::item::{Check, Item, Severity};
use model::store::{teeth_answer, TeethAnswer, NO_LITERAL_REASON_MIN};
use serve::live::live_items;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// Whether `item` answers the teeth question, in the sense above: a
/// machine-runnable check, or the store's own reasoned exemption tag at or
/// above the write gate's own length bar. The one definition this file
/// uses, so the printed `"answered"` field and the summary counts on stderr
/// can never disagree about which items count.
fn is_answered(item: &Item) -> bool {
    if item.check.is_some() {
        return true;
    }
    item.tags.iter().any(|tag| {
        matches!(
            teeth_answer(tag),
            Some(TeethAnswer::Reasoned(reason)) if reason.chars().count() >= NO_LITERAL_REASON_MIN
        )
    })
}

/// The wire name of a `Check` variant - the single top-level key its default
/// externally-tagged JSON representation carries (`Check` has no
/// `#[serde(tag = ...)]` of its own; see that type's doc comment). Read back
/// through `serde_json` rather than re-typed by hand, the same technique
/// `serve::live::target_kind_wire` uses for `TargetKind`, so this can never
/// drift from whatever `Check` actually serialises as.
fn check_kind(check: &Check) -> String {
    match serde_json::to_value(check) {
        Ok(serde_json::Value::Object(map)) => map.into_iter().next().map(|(k, _)| k).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Whether `err`, or anything in its cause chain, is SQLite's own "database
/// is locked" message - the wording `rusqlite` surfaces for `SQLITE_BUSY`/
/// `SQLITE_LOCKED` once `open_existing`'s own 5-second `busy_timeout` has
/// already been waited out once for this connection. String matching rather
/// than downcasting to `rusqlite::Error` on purpose: `rusqlite` is an
/// optional dependency of this crate (behind the `semantic` feature, see
/// `serve/Cargo.toml`), so this example must not require it just to build.
fn is_database_locked(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| cause.to_string().contains("database is locked"))
}

/// Open `db` the same way `main` always has - through `EventStore::
/// open_existing`, never `new` (a census must not repair the store it is
/// counting: `new` runs schema work and an FTS heal on open, see that
/// constructor's own doc comment in `core/src/event_store.rs`) - but retry a
/// "database is locked" failure up to 10 times, 3 seconds apart, before
/// giving up. `open_existing`'s own `busy_timeout` already covers ordinary
/// contention DURING one connection's lifetime; this loop is for contention
/// AT OPEN TIME itself, from another process holding the store exclusively
/// for longer than one busy_timeout window - a backup, a bulk repair, a long
/// migration. Any other failure (the path does not exist, a genuinely
/// corrupt store) is returned immediately, unretried.
fn open_with_retry(db: &std::path::Path) -> anyhow::Result<EventStore> {
    const MAX_ATTEMPTS: u32 = 10;
    const PAUSE: std::time::Duration = std::time::Duration::from_secs(3);
    let mut attempt = 1;
    loop {
        match EventStore::open_existing(db) {
            Ok(store) => return Ok(store),
            Err(e) if attempt < MAX_ATTEMPTS && is_database_locked(&e) => {
                eprintln!("teeth_census: database is locked, retrying (attempt {attempt} of {MAX_ATTEMPTS})...");
                std::thread::sleep(PAUSE);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let unanswered_only = std::env::args().any(|a| a == "--unanswered");

    let Some(db) = std::env::var("TEETH_CENSUS_DB").ok() else {
        eprintln!(
            "teeth_census requires an environment variable, with no default:\n\
             TEETH_CENSUS_DB - path to a store (live or a copy)\n\
             Not set, so this exits now: no store has been opened, nothing has been read."
        );
        std::process::exit(1);
    };
    let db = PathBuf::from(db);

    let store = open_with_retry(&db)?;

    let mut heavy = 0usize;
    let mut answered = 0usize;
    let mut rows: Vec<serde_json::Value> = Vec::new();

    for live in live_items(&store) {
        if !live.item.kind.can_fire() {
            continue; // only Rule/Orientation can ever be heavy in the sense teeth_line means
        }
        if !matches!(live.item.severity, Some(Severity::Irreversible) | Some(Severity::Costly)) {
            continue;
        }
        heavy += 1;
        let item_answered = is_answered(&live.item);
        if item_answered {
            answered += 1;
        }
        if unanswered_only && item_answered {
            continue;
        }
        rows.push(serde_json::json!({
            "id": live.id,
            "kind": live.item.kind,
            "severity": live.item.severity,
            "text": live.item.text,
            "bindings": live.item.bindings,
            "check": live.item.check.as_ref().map(check_kind),
            "tags": live.item.tags,
            "project": live.item.project,
            "answered": item_answered,
        }));
    }

    // Summary to stderr, data to stdout: stdout stays pure one-JSON-object-
    // per-line output, safe to pipe into `jq` or redirect straight to a
    // file, exactly the ndjson contract this tool's own doc comment above
    // promises - mixing a prose summary into that stream would break it for
    // every consumer that is not a human reading a terminal.
    eprintln!("heavy (live, fireable, severity irreversible or costly): {heavy}");
    eprintln!(
        "answered (a check, or a no-literal:<reason> tag of at least {NO_LITERAL_REASON_MIN} characters): {answered}"
    );
    eprintln!("unanswered: {}", heavy - answered);
    if unanswered_only {
        eprintln!("printing only the {} unanswered row(s)", rows.len());
    } else {
        eprintln!("printing all {} heavy row(s)", rows.len());
    }

    for row in &rows {
        println!("{}", serde_json::to_string(row)?);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Kind};

    /// A minimal heavy (irreversible, fireable) Rule, no check and no tags -
    /// the unanswered baseline every test below starts from and then
    /// changes exactly one thing about.
    fn heavy_item(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "never force-push to main".to_string(),
            bindings: vec![Binding::Always],
            severity: Some(Severity::Irreversible),
            project: Some("thor2".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("a force-push to main lands with no incident and no revert needed".to_string()),
            check: None,
        }
    }

    #[test]
    fn a_heavy_item_with_a_check_is_answered_even_with_no_tags() {
        let mut item = heavy_item("t1");
        item.check = Some(Check::PathExists { path: "README.md".to_string() });
        assert!(is_answered(&item));
    }

    #[test]
    fn a_heavy_item_with_neither_a_check_nor_a_tag_is_unanswered() {
        let item = heavy_item("t2");
        assert!(!is_answered(&item));
    }

    #[test]
    fn a_bare_no_literal_tag_with_no_reason_is_unanswered() {
        // The write gate itself only still honours a bare `no-literal` on an
        // item that already carried it before a reason was required
        // (`model::store::TeethAnswer::Bare`) - it never accepts one as a
        // NEW answer. This census holds every already-live item to that
        // same current bar rather than grandfathering an old bare tag in
        // silently, so a bare tag must read as unanswered here too.
        let mut item = heavy_item("t3");
        item.tags = vec!["no-literal".to_string()];
        assert!(!is_answered(&item));
    }

    #[test]
    fn a_reasoned_no_literal_tag_under_twenty_characters_is_unanswered() {
        let mut item = heavy_item("t4");
        item.tags = vec!["no-literal:too short".to_string()]; // "too short" is 9 characters
        assert!(!is_answered(&item));
    }

    #[test]
    fn a_reasoned_no_literal_tag_at_exactly_twenty_characters_is_answered() {
        // Sanity on the fixture itself first, so this test fails loudly - on
        // the fixture, not on `is_answered` - if the literal below ever
        // drifts off the exact boundary it means to sit on.
        assert_eq!("exactly twenty chars".chars().count(), 20);
        let mut item = heavy_item("t5");
        item.tags = vec!["no-literal:exactly twenty chars".to_string()];
        assert!(is_answered(&item));
    }

    #[test]
    fn an_unrelated_tag_does_not_answer_it() {
        let mut item = heavy_item("t6");
        item.tags = vec!["source:some-id".to_string()];
        assert!(!is_answered(&item));
    }

    #[test]
    fn a_reasoned_tag_among_other_unrelated_tags_still_answers_it() {
        let mut item = heavy_item("t7");
        item.tags = vec!["source:some-id".to_string(), "no-literal:an authorised push looks identical".to_string()];
        assert!(is_answered(&item));
    }

    #[test]
    fn check_kind_names_the_variant_and_nothing_else() {
        assert_eq!(check_kind(&Check::PathExists { path: "README.md".to_string() }), "path_exists");
        assert_eq!(check_kind(&Check::Forbidden { literals: vec!["TODO".to_string()] }), "forbidden");
        assert_eq!(
            check_kind(&Check::AbsentAll { path: "a.md".to_string(), literals: vec!["x".to_string()] }),
            "absent_all"
        );
    }
}
