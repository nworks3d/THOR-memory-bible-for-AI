//! The global tier is `None`. Never a project named "global".
//!
//! THE DEFECT THIS PREVENTS, caught on 2026-08-03 one step before it shipped.
//! Target identity was normalised from the first commit; project identity was
//! not, so whatever a source wrote became the value. 1.0 spells the global
//! tier as the literal word `global` in its footer, and the migration carried
//! that word through as if it named a project: 74 of the owner's global rules
//! came out owned by a project called "global", two more by "null".
//!
//! That was invisible for as long as nothing scoped by project - those rules
//! matched everywhere, which was accidentally correct. Adding the (correct)
//! project filter to the moment surface would have made them match NOWHERE.
//! They are the cross-project rules: the ones that belong to no project by
//! definition, and the ones that matter most. A correct filter over an
//! unnormalised value silently deletes the entire global tier.
//!
//! Which is why this is pinned at the WRITE path and not at a read: a read
//! that tolerates "global" is a compensating mechanism (CONTRACT R9), and it
//! would have to be repeated in every surface that ever scopes.

use model::item::{Binding, Item, Kind};
use model::normalize::normalize_project;
use model::store;
use thor_core::event_store::EventStore;

fn rule(id: &str, project: Option<&str>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: "never force-push to main".to_string(),
        bindings: vec![Binding::Moment(intent::Action::Push)],
        severity: None,
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a force-push to main lands with no incident".to_string()),
        check: None,
    }
}

#[test]
fn every_spelling_of_the_global_tier_normalises_to_none() {
    for spelling in ["global", "GLOBAL", "Global", " global ", "none", "null", "-", "", "   "] {
        assert_eq!(
            normalize_project(Some(spelling)),
            None,
            "{spelling:?} names the absence of a project, not a project"
        );
    }
    assert_eq!(normalize_project(None), None);
}

#[test]
fn a_real_project_name_survives_untouched() {
    assert_eq!(normalize_project(Some("acme-shop")), Some("acme-shop".to_string()));
    assert_eq!(normalize_project(Some(" The-AI-memory-bible ")), Some("The-AI-memory-bible".to_string()));
    // A name that merely CONTAINS the word is a real project, not the tier.
    assert_eq!(normalize_project(Some("global-config")), Some("global-config".to_string()));
}

#[test]
fn declaring_with_project_global_stores_it_as_the_global_tier() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule("g1", Some("global"))).unwrap();
    let stored = store::show(&db, "g1").unwrap();
    assert_eq!(stored.project, None, "the stored item must belong to the global tier");
}

/// The repair path itself: an item already sitting in the store as project
/// "global" must be revisable to `None` without that reading as a dropped
/// field. Without this the store could never be fixed - the gate's own
/// no-silent-drops rule would refuse the correction.
#[test]
fn repairing_an_already_stored_global_is_not_a_dropped_field() {
    let mut db = EventStore::in_memory().unwrap();
    // Simulate the migrated state by declaring it the way the migration did.
    store::declare(&mut db, "s", "l", "a", &rule("g2", Some("global"))).unwrap();
    let existing = store::show(&db, "g2").unwrap();

    let repaired = rule("g2", None);
    store::revise(&mut db, "s", "l", "a", &existing, &repaired)
        .expect("rewriting a 'global' project as None says the same thing, so it is not a drop");
    assert_eq!(store::show(&db, "g2").unwrap().project, None);
}

/// The check that must still bite: moving an item OUT of a real project into
/// the global tier is a genuine change of meaning, and dropping it silently
/// is the defect the gate exists for.
#[test]
fn dropping_a_real_project_is_still_refused() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule("p1", Some("acme-shop"))).unwrap();
    let existing = store::show(&db, "p1").unwrap();
    assert!(
        store::revise(&mut db, "s", "l", "a", &existing, &rule("p1", None)).is_err(),
        "silently dropping a real project must still be refused"
    );
}
