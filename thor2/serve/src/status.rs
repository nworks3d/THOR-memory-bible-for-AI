//! The status command's data (CONTRACT gap 3): what the tool knows right
//! now about the fact store, in numbers a person can read without opening
//! source - how many live items exist per kind, how many of the fireable
//! ones (`Kind::can_fire`) were declared but never delivered, and how many
//! have decayed out of the injection surfaces (`crate::decay`). The code
//! index's own commit/drift numbers are a separate concern (see
//! `lookup::code_index_status`, confined to that module) - this one is
//! purely about the fact store `audit`/`live_items` already read.

use crate::audit::audit_rows;
use crate::decay::DecayContext;
use crate::live::live_items;
use model::item::Kind;
use std::fmt;
use thor_core::event_store::EventStore;

/// Every kind, in a fixed reporting order - not a decision about which kinds
/// CAN fire (that stays `model::item::Kind::can_fire`'s alone, see
/// `model/tests/single_can_fire_definition.rs`), just the order this report
/// lists them in.
const ALL_KINDS: [Kind; 5] = [Kind::Rule, Kind::Orientation, Kind::Report, Kind::Lookup, Kind::Chunk];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindCount {
    pub kind: Kind,
    pub count: usize,
}

/// The "served repeatedly, never marked useful" reading (CONTRACT gap 3,
/// same defect class as `crate::decay`'s own doc comment): a plain `count`
/// is not the whole truth, because the AND it comes from has two halves and
/// only one of them is ever actually deciding anything in real use.
///
/// THE DEFECT THIS PREVENTS. `count` is `times_served >= STALE_AFTER_SERVINGS
/// AND NOT ever_marked_useful`. Marking is documented elsewhere in this
/// workspace as effectively dead in real use (`crate::decay`'s own doc
/// comment: "Marks of usefulness recorded in a full day of real work: ZERO.
/// Not few. None."). When that holds, `NOT ever_marked_useful` is true of
/// EVERY fireable item, so it filters nothing - the AND collapses to
/// `times_served >= STALE_AFTER_SERVINGS` alone, while the printed label
/// still reads as if "never marked" were telling a reader something about
/// THESE items specifically. A `count` of 0 in that state looks identical to
/// a `count` of 0 in a store where marking genuinely discriminates (some
/// items marked, these just were not among them) - and only the second case
/// is actually a clean bill of health. This type keeps that difference
/// visible instead of folding it into a bare number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServedNeverMarked {
    /// Exactly what the old bare `usize` counted, unchanged: fireable items
    /// served at least `decay::STALE_AFTER_SERVINGS` times and never marked
    /// useful. See `store_status` for the computation.
    pub count: usize,
    /// True when at least one fireable item ANYWHERE in the store has ever
    /// been marked useful. Deliberately NOT scoped to the items `count`
    /// itself counts - an item `count` counts is by definition never marked
    /// (or the filter above would have excluded it), so only some OTHER
    /// fireable item being marked useful can ever prove marking works at
    /// all here. This is that proof, or the absence of it.
    pub marking_ever_happened: bool,
}

impl fmt::Display for ServedNeverMarked {
    /// Plain digits when marking has discriminated at least once among the
    /// fireable population - the sentence in `serve/src/bin/serve.rs` that
    /// embeds this value reads exactly as it did before this type existed.
    /// Otherwise the count is followed by the reason a reader must not take
    /// it as reassurance: see this type's own doc comment.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.count)?;
        if !self.marking_ever_happened {
            write!(
                f,
                " (marking has never happened anywhere in this store, so 'never marked' is true of every fireable item by default - not a signal about these specifically)"
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStatus {
    /// One row per kind, `ALL_KINDS` order, zero included so a kind with
    /// nothing declared still gets a visible "0" rather than vanishing.
    pub counts: Vec<KindCount>,
    /// How many live, fireable (`Rule`/`Orientation`) items exist at all -
    /// the denominator for `declared_never_fired`.
    pub fireable_total: usize,
    /// Of the fireable items, how many have never once been delivered - the
    /// same count `serve audit`'s own "declared, never delivered" line
    /// reports, exposed here as a number rather than a table.
    pub declared_never_fired: usize,
    /// Of the fireable items, how many are currently decayed (called noise
    /// at least `decay::NOISE_MARKS_BEFORE_STALE` times since the last mark
    /// of usefulness) -
    /// excluded from every injection surface right now, but still fully
    /// live and still fully findable via `lookup::search`.
    pub decayed: usize,
    /// Served at least `decay::STALE_AFTER_SERVINGS` times and never marked
    /// useful. A number to LOOK AT, never a filter - see `crate::decay` for
    /// the day that rule was falsified in production. Carries more than a
    /// bare count - see `ServedNeverMarked`'s own doc comment for why a 0
    /// here must never be printed without saying whether marking has ever
    /// happened at all in this store.
    ///
    /// KNOWN CONSUMER BUG, NOT FIXABLE FROM HERE: `mcp/src/lib.rs`'s
    /// `status` tool (its `format!` call building the summary string, at
    /// the time of writing around line 734) never reads this field at all -
    /// it prints `st.decayed` a second time under THIS field's label,
    /// left behind by commit 38c53cb ("status stops calling it decayed,
    /// because nothing decays any more"): that commit relabelled the text
    /// in both `serve/src/bin/serve.rs` and `mcp/src/lib.rs` from "decayed
    /// (served, never marked useful)" to "served repeatedly, never marked
    /// useful", while both still read `st.decayed` for the value (correct
    /// AT THAT COMMIT, since `decayed` was still servings-based then). The
    /// very next commit, 8f3656d, rewrote `decayed` to mean something else
    /// entirely (called noise `NOISE_MARKS_BEFORE_STALE`+ times, never
    /// marked useful - see `crate::decay`) and introduced THIS field to
    /// carry the old servings-based meaning; it fixed
    /// `serve/src/bin/serve.rs` to read the new field and print `decayed`
    /// under its own new "retired as noise" label, but never touched
    /// `mcp/src/lib.rs`'s `status` tool at all. So today the MCP surface
    /// prints `decayed` (a noise-judgement count) under the label that
    /// belongs to `served_never_marked` (a serving-count), and never prints
    /// `served_never_marked` under any label at all. A store with heavy
    /// real serving traffic and no noise judgements yet reads a perfectly
    /// credible-looking "0" there for entirely the wrong reason. Fixing
    /// this needs a change in `mcp/src/lib.rs`, outside this file's
    /// boundary - see the `serve/src/bin/serve.rs` lines right after the
    /// commit above (the two separate, correctly-wired `println!` calls)
    /// for the shape the MCP tool's `format!` call would need to match.
    pub served_never_marked: ServedNeverMarked,
    /// Of the fireable items, how many carry no `falsifier` at all - a `Rule`
    /// or `Orientation` written before the field existed (every item this
    /// workspace's own migration carried over from 1.0), since `gate::declare`
    /// refuses a NEW write missing one but an old item is never rewritten just
    /// to gain it (see `model::item::Item::falsifier`'s own doc comment). This
    /// is the worklist number: it is never invented per item, only counted.
    pub missing_falsifier: usize,
    /// Which mode `lookup::search_best_effort` (surface 4, feature
    /// `semantic`) runs in right now - one plain-language line, so this
    /// status report and `ops::health`'s own line can never disagree about
    /// what is active (see `semantic_paths::mode_line`).
    pub semantic_search: String,
}

/// Fold `live_items` into per-kind counts, plus the audit crate's own
/// never-fired count, the decay count, and how many fireable items still
/// have no falsifier. Fails open like every other reader on this boundary: a
/// broken log yields all-zero counts rather than an error (`live_items`,
/// `audit_rows` and `DecayContext::load` already degrade this way; this just
/// folds their output).
pub fn store_status(store: &EventStore) -> StoreStatus {
    let items = live_items(store);
    let mut counts: Vec<KindCount> = ALL_KINDS.iter().map(|&kind| KindCount { kind, count: 0 }).collect();
    for live in &items {
        if let Some(row) = counts.iter_mut().find(|row| row.kind == live.item.kind) {
            row.count += 1;
        }
    }
    let rows = audit_rows(store);
    let declared_never_fired = rows.iter().filter(|r| r.stats.times_served == 0).count();
    let decay = DecayContext::load(store);
    let decayed = rows.iter().filter(|r| decay.is_stale(&r.item)).count();
    let served_never_marked_count = rows
        .iter()
        .filter(|r| r.stats.times_served >= crate::decay::STALE_AFTER_SERVINGS)
        .filter(|r| !decay.was_ever_marked_useful(&r.id))
        .count();
    // Scoped to the WHOLE fireable population, not just the rows counted
    // above: an item counted above is by definition never marked (or the
    // filter would have excluded it), so only some OTHER fireable item
    // being marked useful can ever prove the marking signal is alive at
    // all - see `ServedNeverMarked`'s own doc comment.
    let marking_ever_happened = rows.iter().any(|r| decay.was_ever_marked_useful(&r.id));
    let served_never_marked =
        ServedNeverMarked { count: served_never_marked_count, marking_ever_happened };
    let missing_falsifier = rows.iter().filter(|r| r.item.falsifier.is_none()).count();
    let semantic_search = crate::semantic_paths::mode_line(None);
    StoreStatus {
        counts,
        fireable_total: rows.len(),
        declared_never_fired,
        decayed,
        served_never_marked,
        missing_falsifier,
        semantic_search,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item};
    use model::store;

    fn item(id: &str, kind: Kind) -> Item {
        Item {
            id: id.to_string(),
            kind,
            text: "do the thing".to_string(),
            bindings: if kind == Kind::Rule || kind == Kind::Orientation {
                vec![Binding::Always]
            } else if kind == Kind::Lookup {
                vec![]
            } else {
                vec![]
            },
            severity: None,
            // Archive material needs a scope (gate ground 21); a fireable item
            // is allowed to be global and stays that way here.
            project: (!kind.can_fire()).then(|| "test-project".to_string()),
            tags: vec![],
            expires: if kind == Kind::Report { Some("2027-01-01".to_string()) } else { None },
            key: if kind == Kind::Lookup { Some(format!("{id}-key")) } else { None },
            falsifier: if kind.can_fire() {
                Some("this synthetic fixture item turns out to be wrong".to_string())
            } else {
                None
            },
            check: None,
        }
    }

    /// The fixture above pins everything to `Always`, and an `Always` item is
    /// exempt from decay by design (see `decay::DecayContext::is_stale`). Any
    /// test ABOUT decay therefore needs an ordinarily bound rule, or it would
    /// be asserting against the exemption rather than against the threshold.
    fn bound_rule(id: &str) -> Item {
        let mut it = item(id, Kind::Rule);
        // A project, because ground 19 refuses a GLOBAL item anchored at a
        // source file: it would fire in every repository that happens to have
        // one by that name. These fixtures are about serving counts, not about
        // that ground, so they name a project the way a real caller does.
        it.project = Some("some-project".to_string());
        it.bindings = vec![Binding::Target {
            kind: model::item::TargetKind::Path,
            value: format!("src/{id}.rs"),
        }];
        it
    }

    #[test]
    fn an_empty_store_reports_zero_for_every_kind() {
        let db = EventStore::in_memory().unwrap();
        let status = store_status(&db);
        assert_eq!(status.counts.len(), 5, "every kind gets a row, even at zero");
        assert!(status.counts.iter().all(|row| row.count == 0));
        assert_eq!(status.fireable_total, 0);
        assert_eq!(status.declared_never_fired, 0);
    }

    #[test]
    fn each_declared_item_is_counted_under_its_own_kind_only() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("r1", Kind::Rule)).unwrap();
        store::declare(&mut db, "s", "l", "a", &item("o1", Kind::Orientation)).unwrap();
        store::declare(&mut db, "s", "l", "a", &item("rep1", Kind::Report)).unwrap();
        store::declare(&mut db, "s", "l", "a", &item("look1", Kind::Lookup)).unwrap();
        store::declare(&mut db, "s", "l", "a", &item("chunk1", Kind::Chunk)).unwrap();

        let status = store_status(&db);
        for row in &status.counts {
            assert_eq!(row.count, 1, "expected exactly one {:?}, got {}: {:?}", row.kind, row.count, status.counts);
        }
    }

    #[test]
    fn declared_never_fired_matches_audits_own_count() {
        let mut db = EventStore::in_memory().unwrap();
        // Two DIFFERENT items (the near-duplicate gate refuses a second
        // live item of the same kind with the same or near-same text) -
        // what this test is actually about is the never-fired COUNT, which
        // does not depend on which text either one carries.
        let mut never = item("never", Kind::Rule);
        never.text = "review pull requests within one business day".to_string();
        store::declare(&mut db, "s", "l", "a", &never).unwrap();
        let mut fired = item("fired", Kind::Rule);
        fired.text = "renew the tls certificate before it expires".to_string();
        store::declare(&mut db, "s", "l", "a", &fired).unwrap();
        crate::deliver::record_delivery(&mut db, "s", "l", "serve", "2026-08-02T00:00:00Z", &["fired".to_string()]);

        let status = store_status(&db);
        assert_eq!(status.fireable_total, 2);
        assert_eq!(status.declared_never_fired, 1);
    }

    #[test]
    fn archive_kinds_never_count_toward_fireable_total() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("rep1", Kind::Report)).unwrap();
        store::declare(&mut db, "s", "l", "a", &item("look1", Kind::Lookup)).unwrap();
        store::declare(&mut db, "s", "l", "a", &item("chunk1", Kind::Chunk)).unwrap();
        let status = store_status(&db);
        assert_eq!(status.fireable_total, 0, "Report/Lookup/Chunk are archive, never fireable");
        assert_eq!(status.declared_never_fired, 0);
    }

    /// A serving count is a number to look at, never a retirement. Pins the
    /// split the falsification of 2026-08-03 forced: the two numbers must not
    /// be the same number again (see `crate::decay`).
    #[test]
    fn a_repeatedly_served_item_is_counted_but_never_retired() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &bound_rule("hot-1")).unwrap();
        for i in 0..crate::decay::STALE_AFTER_SERVINGS {
            crate::deliver::record_delivery(
                &mut db, "s", "l", "serve", &format!("2026-08-0{}T00:00:00Z", i + 1), &["hot-1".to_string()],
            );
        }
        let status = store_status(&db);
        assert_eq!(status.served_never_marked.count, 1, "counted, so a person can look at it");
        assert_eq!(status.decayed, 0, "and never retired for it");
    }

    // ------------------------------------------------- served_never_marked
    // honesty: marking is documented (crate::decay's own doc comment) as
    // effectively dead in real use, which makes "never marked useful" true
    // of every fireable item by default rather than a fact about any one of
    // them. These pin that a reading can never be mistaken for the OTHER
    // case (marking genuinely discriminates, and these items just were not
    // marked) - see `ServedNeverMarked`'s own doc comment.

    /// THE DEFECT THIS PREVENTS: a store with heavy real serving traffic and
    /// not one mark of usefulness anywhere used to print a bare "0" (or
    /// whatever `count` happened to be) that reads identically to "nothing
    /// to see here" - indistinguishable from a store where marking actually
    /// works and simply had nothing to flag. Real use is documented
    /// elsewhere in this workspace to look exactly like this fixture: zero
    /// marks in a full day of real work (`crate::decay`'s own doc comment).
    #[test]
    fn served_never_marked_names_the_dead_marking_signal_instead_of_reading_as_all_clear() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &bound_rule("hot-1")).unwrap();
        for i in 0..crate::decay::STALE_AFTER_SERVINGS {
            crate::deliver::record_delivery(
                &mut db, "s", "l", "serve", &format!("2026-08-0{}T00:00:00Z", i + 1), &["hot-1".to_string()],
            );
        }
        let status = store_status(&db);
        assert_eq!(status.served_never_marked.count, 1);
        assert!(
            !status.served_never_marked.marking_ever_happened,
            "nothing in this store was ever marked useful"
        );
        let printed = status.served_never_marked.to_string();
        assert!(
            printed.contains("never happened"),
            "the printed value must say marking is dead, not just show a bare number: {printed}"
        );
    }

    /// The same defect at the exact number from the bug report: a genuine
    /// 0 (nothing yet crosses the threshold) must ALSO carry the caveat when
    /// marking has never happened - a 0 is the single most dangerous value
    /// to get this wrong on, since it is the one that looks healthiest.
    #[test]
    fn a_genuine_zero_is_still_flagged_as_a_dead_signal_when_marking_never_happens() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &bound_rule("warm-1")).unwrap();
        // Served, but nowhere near the threshold - the shape of a young,
        // actively-used store: real traffic, nothing marked, nothing has
        // crossed the bar yet either.
        crate::deliver::record_delivery(&mut db, "s", "l", "serve", "2026-08-03T00:00:00Z", &["warm-1".to_string()]);
        let status = store_status(&db);
        assert_eq!(status.served_never_marked.count, 0);
        assert!(!status.served_never_marked.marking_ever_happened);
        assert!(
            status.served_never_marked.to_string().contains("never happened"),
            "a 0 here must never look identical to a store where marking actually works"
        );
    }

    /// THE OTHER HALF: once marking has happened ANYWHERE in the fireable
    /// population, "never marked" is a real, discriminating fact again, and
    /// the reading must go back to a plain number - the caveat is for a dead
    /// signal, not a permanent disclaimer that would cry wolf forever.
    #[test]
    fn served_never_marked_is_a_plain_number_once_marking_has_happened_anywhere() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &bound_rule("hot-1")).unwrap();
        for i in 0..crate::decay::STALE_AFTER_SERVINGS {
            crate::deliver::record_delivery(
                &mut db, "s", "l", "serve", &format!("2026-08-0{}T00:00:00Z", i + 1), &["hot-1".to_string()],
            );
        }
        // A DIFFERENT item gets marked useful. hot-1 above is still served
        // repeatedly and still never marked, but marking now demonstrably
        // works somewhere in this store. Distinct text (the near-duplicate
        // gate refuses a second live Rule with the same or near-same text
        // as hot-1's default "do the thing").
        let mut elsewhere = bound_rule("marked-elsewhere");
        elsewhere.text = "back up the database nightly".to_string();
        store::declare(&mut db, "s", "l", "a", &elsewhere).unwrap();
        crate::mark::record_useful(&mut db, "s", "l", "a", "2026-08-02T00:00:00Z", "marked-elsewhere").unwrap();

        let status = store_status(&db);
        assert_eq!(status.served_never_marked.count, 1, "hot-1 is still served repeatedly and never marked");
        assert!(
            status.served_never_marked.marking_ever_happened,
            "a mark on a different item still proves the signal is alive"
        );
        assert_eq!(
            status.served_never_marked.to_string(),
            "1",
            "a live marking signal prints as a plain number, no caveat attached"
        );
    }

    /// Two noise judgements DO retire it - that is the signal decay reads now.
    #[test]
    fn an_item_called_noise_twice_is_reported_as_retired() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &bound_rule("noisy-1")).unwrap();
        for i in 0..crate::decay::NOISE_MARKS_BEFORE_STALE {
            crate::mark::record_noise(
                &mut db, "s", "l", "a", &format!("2026-08-0{}T00:00:00Z", i + 1), "noisy-1",
            )
            .unwrap();
        }
        assert_eq!(store_status(&db).decayed, 1);
    }

    /// The defect this guards against: `status` reporting a standing rule as
    /// decayed. It is the same exemption `decay` enforces, checked here
    /// because this is the number the owner actually reads - a count that
    /// says thirteen standing rules retired is exactly the alarm that should
    /// never be able to fire for a pin.
    #[test]
    fn decayed_never_counts_a_pinned_rule_however_many_sessions_started() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("pin-1", Kind::Rule)).unwrap();
        for i in 0..crate::decay::STALE_AFTER_SERVINGS * 4 {
            crate::deliver::record_delivery(
                &mut db, "s", "l", "serve", &format!("2026-08-03T00:00:{:02}Z", i), &["pin-1".to_string()],
            );
        }
        let status = store_status(&db);
        assert_eq!(status.decayed, 0, "an Always-bound rule must never be reported as decayed");
    }

    #[test]
    fn decayed_never_counts_an_item_that_was_marked_useful() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("marked-1", Kind::Rule)).unwrap();
        for i in 0..crate::decay::STALE_AFTER_SERVINGS {
            crate::deliver::record_delivery(
                &mut db, "s", "l", "serve", &format!("2026-08-0{}T00:00:00Z", i + 1), &["marked-1".to_string()],
            );
        }
        crate::mark::record_useful(&mut db, "s", "l", "a", "2026-08-02T00:00:00Z", "marked-1").unwrap();
        let status = store_status(&db);
        assert_eq!(status.decayed, 0, "a marked item must never count as decayed");
    }

    // ------------------------------------------------------- missing_falsifier

    /// A body with no "falsifier" key at all, appended directly - the exact
    /// shape of an item migrated before the field existed (`gate::declare`
    /// is never in this path, matching how a real migration's already-written
    /// bodies were never rewritten to gain the field after the fact).
    fn pre_migration_rule_body(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","kind":"rule","text":"a pre-migration fixture rule","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null}}"#
        )
    }

    #[test]
    fn missing_falsifier_counts_a_fireable_item_with_no_falsifier_at_all() {
        let mut db = EventStore::in_memory().unwrap();
        db.append_event("s", "l", "a", thor_core::event_store::EventKind::FactCreated, "old-1", None, &pre_migration_rule_body("old-1"))
            .unwrap();
        let status = store_status(&db);
        assert_eq!(status.missing_falsifier, 1);
        assert_eq!(status.fireable_total, 1, "a pre-migration item with no falsifier is still live and fireable");
    }

    #[test]
    fn missing_falsifier_is_zero_when_every_fireable_item_has_one() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("has-one-1", Kind::Rule)).unwrap();
        let status = store_status(&db);
        assert_eq!(status.missing_falsifier, 0);
    }

    #[test]
    fn missing_falsifier_never_counts_an_archive_item() {
        // A Report/Lookup/Chunk is never required to carry a falsifier at
        // all (see model::item::Item's own doc comment), and it never counts
        // toward fireable_total either - the worklist is scoped to Rule/
        // Orientation, the two kinds gate::declare actually requires it for.
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("rep1", Kind::Report)).unwrap();
        let status = store_status(&db);
        assert_eq!(status.missing_falsifier, 0);
    }

    // ------------------------------------------------------ semantic_search

    #[test]
    fn semantic_search_line_is_never_blank() {
        // Named after the defect it prevents: whichever mode is active, this
        // status report must always name it - never an empty string that a
        // caller could mistake for "nothing to say".
        let db = EventStore::in_memory().unwrap();
        let status = store_status(&db);
        assert!(status.semantic_search.starts_with("semantic search:"), "{}", status.semantic_search);
    }
}
