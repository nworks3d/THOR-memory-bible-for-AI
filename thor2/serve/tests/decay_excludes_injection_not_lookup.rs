//! End-to-end proof of the decay doctrine (`serve::decay`) through the REAL
//! store and the REAL serving/lookup functions, not just `DecayContext` in
//! isolation:
//!
//! - a repeatedly-served, never-marked item disappears from every injection
//!   surface (moment of action, session start) but stays fully findable via
//!   `lookup::search` - decay is never deletion;
//! - an item marked useful even once never decays, no matter how many times
//!   it is served afterward;
//! - decay is reversible: marking an already-decayed item useful brings it
//!   back on the very next call, with nothing else to "undo".

use intent::Action;
use model::item::{Binding, Item, Kind, Severity};
use model::store;
use serve::input::ServeInput;
use serve::{decay, deliver, live, lookup, mark, session_start};
use thor_core::event_store::EventStore;

fn moment_rule(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("never {id} without checking first"),
        bindings: vec![Binding::Moment(Action::Push)],
        severity: Some(Severity::Irreversible),
        project: None,
        // Gate ground 11: these fixtures test decay, not teeth, and a generic
        // "never <id> without checking first" has no literal to catch.
        tags: vec![model::store::NO_LITERAL_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out to be safe without checking first")),
        check: None,
    }
}

fn always_rule(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("standing rule {id}"),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Irreversible),
        project: None,
        // Same as moment_rule above.
        tags: vec![model::store::NO_LITERAL_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: Some(format!("standing rule {id} turns out to be wrong")),
        check: None,
    }
}

fn serve_it_n_times(db: &mut EventStore, id: &str, n: usize) {
    for i in 0..n {
        deliver::record_delivery(db, "s", "l", "serve", &format!("2026-08-{:02}T00:00:00Z", i + 1), &[id.to_string()]);
    }
}

/// THE DEFECT THIS PREVENTS, measured on the first real day (2026-08-03).
/// Four `prod_data` rules - three `costly`, one `irreversible`, about not
/// testing a write path against the real store and not running a reseed in
/// the live container - stopped reaching the moment surface after ONE day.
/// They stopped because they fired often, which is what relevance looks like,
/// and because zero usefulness marks occur in real use, which makes "never
/// marked useful" a constant rather than a signal about the item.
///
/// A rule that keeps firing must keep firing. Nothing removes it for the
/// absence of a button press. See `serve::decay`'s own module doc for the
/// full argument and for what would justify bringing a real decay back.
#[test]
fn a_rule_that_fires_constantly_keeps_reaching_the_moment_surface() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &moment_rule("hot-1")).unwrap();
    serve_it_n_times(&mut db, "hot-1", decay::STALE_AFTER_SERVINGS * 4);

    let mut input = ServeInput::default();
    input.add_moment(Action::Push);
    let served = serve::serve(&db, &input);
    assert!(
        served.all.iter().any(|r| r.id == "hot-1"),
        "a rule served far past the old threshold, never marked, must still apply"
    );

    let hits = lookup::search(&db, "hot-1");
    assert_eq!(hits.len(), 1, "and it is still fully findable via lookup");
    assert_eq!(hits[0].id, "hot-1");
}

/// The defect this guards against, found on the first real day (2026-08-03):
/// the owner's thirteen standing rules decayed out of session start before he
/// had used the tool once. He keeps several Claude Code sessions open, every
/// one fires SessionStart, and a single restart opened four inside ten
/// seconds - four of five servings spent in a burst that says nothing at all
/// about whether a rule is helping.
///
/// Session start selects `Always` and nothing else (`session_start::select`),
/// and an `Always` item is exempt from decay (`decay::DecayContext::is_stale`),
/// so this surface is now structurally out of decay's reach. That is the
/// point: a standing rule comes down by an unpin, never by a counter running
/// out.
#[test]
fn a_pinned_rule_still_reaches_session_start_after_any_number_of_servings() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &always_rule("pin-2")).unwrap();
    serve_it_n_times(&mut db, "pin-2", decay::STALE_AFTER_SERVINGS * 4);

    let candidates = live::live_items(&db);
    let decay_ctx = decay::DecayContext::load(&db);
    let items = decay::retain_live(session_start::select(&candidates, None), &decay_ctx);
    assert!(
        items.iter().any(|r| r.id == "pin-2"),
        "a standing rule must survive every session start, however many fire"
    );

    let hits = lookup::search(&db, "pin-2");
    assert_eq!(hits.len(), 1, "still findable via lookup");
}

#[test]
fn an_item_marked_useful_even_once_never_decays_end_to_end() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &moment_rule("stays-1")).unwrap();
    serve_it_n_times(&mut db, "stays-1", decay::STALE_AFTER_SERVINGS * 10);
    mark::record_useful(&mut db, "s", "l", "a", "2026-08-02T00:00:00Z", "stays-1").unwrap();

    let mut input = ServeInput::default();
    input.add_moment(Action::Push);
    let served = serve::serve(&db, &input);
    assert!(
        served.all.iter().any(|r| r.id == "stays-1"),
        "an item marked useful must keep firing no matter how many times it was served"
    );
}

/// Marking still works and is still recorded - it just is not what decides
/// whether an item may fire any more. This pins that the write path is intact
/// for the day an explicit signal drives a real decay again, and that marking
/// never REMOVES anything either.
#[test]
fn marking_useful_is_recorded_and_changes_nothing_about_what_applies() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &moment_rule("marked-1")).unwrap();
    serve_it_n_times(&mut db, "marked-1", decay::STALE_AFTER_SERVINGS);

    let mut input = ServeInput::default();
    input.add_moment(Action::Push);
    let before = serve::serve(&db, &input);
    assert!(before.all.iter().any(|r| r.id == "marked-1"), "applies before any mark");

    mark::record_useful(&mut db, "s", "l", "a", "2026-08-02T00:00:00Z", "marked-1").unwrap();
    assert!(
        serve::usefulness::ever_marked_useful(&db).contains("marked-1"),
        "the mark must still be recorded in the log"
    );

    let after = serve::serve(&db, &input);
    assert!(after.all.iter().any(|r| r.id == "marked-1"), "and it still applies afterwards");
}
