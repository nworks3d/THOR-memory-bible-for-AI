//! Maintenance that lives beside the work is a suggestion, and an agent
//! skips suggestions. Measured on this project more than once, and by the
//! assistant that wrote this file, on the same day: it read four misfiring
//! facts, said out loud that a tool existed for exactly that, and marked
//! none of them.
//!
//! So the toll: you may not add a fact to a target that already holds one
//! whose own proof has gone FALSE. It is not a reminder and there is no
//! list to consult. The write simply does not go through, and the refusal
//! names what to settle.
//!
//! What keeps it from becoming a tax people route around is how narrow it
//! is, and each of the three limits below has its own test here:
//!   - only the target you are touching, never a backlog elsewhere
//!   - only `declare`, never `revise`/`retract`, which ARE the maintenance
//!   - only a check that actually came out false, never one that could not
//!     run - "the file is not here" is a different checkout, not a defect,
//!     and the doctrine has always said an unresolvable anchor may not block

use model::item::{Binding, Check, Item, Kind, TargetKind};
use model::store;
use thor_core::event_store::EventStore;

fn anchored(id: &str, target: &str, check: Option<Check>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Orientation,
        // Distinct per id: the near-duplicate refusal is a different gate and
        // must not be what these tests are accidentally measuring.
        text: format!("something worth knowing about {id}"),
        bindings: vec![Binding::Target { kind: TargetKind::Path, value: target.to_string() }],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the file stops being what this says it is".to_string()),
        check,
    }
}

/// A root holding one file with one known line in it, so a `Contains` check
/// can be made to hold or to fail on purpose.
fn root_with(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("noted.md"), content).unwrap();
    let p = dir.path().to_path_buf();
    (dir, p)
}

fn store_with(items: &[Item]) -> EventStore {
    let mut s = EventStore::in_memory().unwrap();
    for item in items {
        store::declare(&mut s, "t", "t", "t", item).expect("fixture must store");
    }
    s
}

/// THE DEFECT THIS PREVENTS: a fact being added right next to one whose
/// proof has already gone false, so the target quietly accumulates rot while
/// every individual write looks fine.
#[test]
fn a_target_holding_a_fact_whose_proof_is_false_refuses_the_next_write() {
    let (_dir, root) = root_with("this file was rewritten and says nothing of the sort\n");
    let stale = anchored(
        "the-stale-one",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    let mut s = store_with(&[stale]);

    let fresh = anchored("the-new-one", "noted.md", None);
    let err = store::declare_in(&mut s, "t", "t", "t", &fresh, Some(&root))
        .expect_err("a write into a rotten neighbourhood must be refused");

    let msg = err.to_string();
    assert!(msg.contains("the-stale-one"), "the refusal must NAME what to settle: {msg}");
    assert!(msg.contains("noted.md"), "and which target it is on: {msg}");
    assert!(
        store::show(&s, "the-new-one").is_err(),
        "nothing may be written when the toll refuses"
    );
}

/// The same write goes straight through once the neighbour is settled. If it
/// did not, the toll would be a wall rather than a toll.
#[test]
fn settling_the_neighbour_lets_the_write_through_unchanged() {
    let (_dir, root) = root_with("this file was rewritten and says nothing of the sort\n");
    let stale = anchored(
        "the-stale-one",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    let mut s = store_with(&[stale]);

    // Settling it the cheap way: retract, one call and a reason.
    store::retract(&mut s, "t", "t", "t", "the-stale-one", "the name it named is gone").unwrap();

    let fresh = anchored("the-new-one", "noted.md", None);
    store::declare_in(&mut s, "t", "t", "t", &fresh, Some(&root))
        .expect("with the neighbourhood settled the write must go through");
    assert!(store::show(&s, "the-new-one").is_ok());
}

/// THE DEFECT THIS PREVENTS: the toll turning into a global chore. A rotten
/// fact on some OTHER target is not this write's business, and charging for
/// it is how a gate teaches people to stop writing.
#[test]
fn rot_on_a_different_target_is_not_this_writes_problem() {
    let (_dir, root) = root_with("nothing here\n");
    let elsewhere = anchored(
        "rotten-elsewhere",
        "other.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "absent literal".into() }),
    );
    let mut s = store_with(&[elsewhere]);

    let fresh = anchored("unrelated", "noted.md", None);
    store::declare_in(&mut s, "t", "t", "t", &fresh, Some(&root))
        .expect("a write must not pay for rot on a target it never touches");
}

/// THE DEFECT THIS PREVENTS, and the one that would make this whole idea
/// backfire: charging people for the very act of maintenance. Correcting or
/// removing a fact must never be tolled, or the cheapest way to satisfy the
/// gate becomes to write nothing at all.
#[test]
fn revising_and_retracting_are_never_tolled() {
    let (_dir, root) = root_with("this file was rewritten and says nothing of the sort\n");
    let stale = anchored(
        "the-stale-one",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    let neighbour = anchored("also-here", "noted.md", None);
    let mut s = store_with(&[stale.clone(), neighbour.clone()]);

    // Revising the neighbour, while the rot is still sitting there.
    let existing = store::show(&s, "also-here").unwrap();
    let mut updated = existing.clone();
    updated.text = "something worth knowing about also-here, corrected".to_string();
    store::revise(&mut s, "t", "t", "t", &existing, &updated)
        .expect("a revise must never be tolled - it IS the maintenance");

    store::retract(&mut s, "t", "t", "t", "also-here", "no longer relevant")
        .expect("a retract must never be tolled either");

    // And the toll is still armed for a genuine new declaration.
    let fresh = anchored("brand-new", "noted.md", None);
    assert!(
        store::declare_in(&mut s, "t", "t", "t", &fresh, Some(&root)).is_err(),
        "the toll must still be in force - otherwise this test proves nothing"
    );
    let _ = root;
}

/// THE DEFECT THIS PREVENTS: the toll firing on every write made from the
/// wrong directory. A check that cannot run says "I do not know", and 2.0's
/// doctrine has always been that an unresolvable anchor is reported, never
/// blocking. If this ever starts refusing, every session in an unrelated
/// checkout stops being able to write anything.
#[test]
fn a_check_that_cannot_run_never_tolls_anything() {
    let dir = tempfile::tempdir().unwrap(); // deliberately EMPTY: no noted.md
    let unresolvable = anchored(
        "cannot-check-here",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "anything".into() }),
    );
    let mut s = store_with(&[unresolvable]);

    let fresh = anchored("from-another-checkout", "noted.md", None);
    store::declare_in(&mut s, "t", "t", "t", &fresh, Some(dir.path()))
        .expect("an anchor that does not resolve here must never block a write");
}

/// A neighbour whose proof HOLDS is exactly what a healthy target looks
/// like, and must cost nothing.
#[test]
fn a_neighbour_whose_proof_holds_costs_nothing() {
    let (_dir, root) = root_with("this file mentions the old name\n");
    let healthy = anchored(
        "the-healthy-one",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    let mut s = store_with(&[healthy]);

    let fresh = anchored("the-new-one", "noted.md", None);
    store::declare_in(&mut s, "t", "t", "t", &fresh, Some(&root))
        .expect("a target whose facts still prove themselves must cost nothing");
}

/// No root, no toll. Every other check in this system behaves this way when
/// it has nothing to run against, and a gate that guessed a root would be
/// deciding which repository the writer meant.
#[test]
fn without_a_root_there_is_no_toll_at_all() {
    let (_dir, _root) = root_with("this file was rewritten and says nothing of the sort\n");
    let stale = anchored(
        "the-stale-one",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    let mut s = store_with(&[stale]);

    let fresh = anchored("the-new-one", "noted.md", None);
    store::declare_in(&mut s, "t", "t", "t", &fresh, None)
        .expect("with no root there is nothing to check and nothing to refuse");
}

/// The toll only looks at TARGET bindings. A moment-bound fact shares no
/// target with anything, so it can never be held hostage by one.
#[test]
fn a_write_with_no_target_binding_is_never_tolled() {
    let (_dir, root) = root_with("this file was rewritten and says nothing of the sort\n");
    let stale = anchored(
        "the-stale-one",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    let mut s = store_with(&[stale]);

    let mut moment_bound = anchored("about-an-action", "noted.md", None);
    moment_bound.bindings = vec![Binding::Moment(intent::Action::Push)];
    store::declare_in(&mut s, "t", "t", "t", &moment_bound, Some(&root))
        .expect("a fact bound to an action shares no target and cannot be tolled");
}

/// THE DEFECT THIS PREVENTS: a path is only unique inside a project.
/// `README.md` names a different file in every checkout, so a fact about one
/// repository's README would otherwise hold a write to another's hostage -
/// and its check, run against the wrong root, reports a failure that says
/// nothing about the file being written. Found on 2026-08-06, one revise
/// after the first real checks were attached, before it could bite.
#[test]
fn rot_in_another_project_never_tolls_this_one() {
    let (_dir, root) = root_with("this file was rewritten and says nothing of the sort
");
    let mut stale = anchored(
        "stale-in-another-repo",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    stale.project = Some("some-other-repo".to_string());
    let mut s = store_with(&[stale]);

    let fresh = anchored("here-in-this-one", "noted.md", None);
    store::declare_in(&mut s, "t", "t", "t", &fresh, Some(&root))
        .expect("a fact from another project shares only the file NAME, never the file");
}

/// And the toll is still armed within one project, so the test above is not
/// quietly disabling it.
#[test]
fn rot_inside_the_same_project_still_tolls() {
    let (_dir, root) = root_with("this file was rewritten and says nothing of the sort
");
    let mut stale = anchored(
        "stale-here",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    stale.project = Some("this-repo".to_string());
    let mut s = store_with(&[stale]);

    let mut fresh = anchored("also-here", "noted.md", None);
    fresh.project = Some("this-repo".to_string());
    assert!(
        store::declare_in(&mut s, "t", "t", "t", &fresh, Some(&root)).is_err(),
        "within one project the toll must still fire"
    );
}

/// Belt and braces on the surface itself: the toll reports its findings as
/// data, so a caller can render them without parsing a sentence.
#[test]
fn the_unsettled_set_is_data_not_a_printed_line() {
    let (_dir, root) = root_with("this file was rewritten and says nothing of the sort\n");
    let stale = anchored(
        "the-stale-one",
        "noted.md",
        Some(Check::Contains { path: "noted.md".into(), literal: "the old name".into() }),
    );
    let s = store_with(&[stale]);

    let fresh = anchored("the-new-one", "noted.md", None);
    let found = store::unsettled_neighbours(&s, &fresh, &root).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "the-stale-one");
    assert!(found[0].target.contains("noted.md"), "{:?}", found[0].target);
}
