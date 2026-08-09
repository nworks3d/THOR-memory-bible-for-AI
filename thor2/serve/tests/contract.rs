//! CONTRACT.md's seven promises to the owner, each proven mechanically here -
//! not by a report, by a test. One test per promise, named after the promise
//! it enforces. Every fixture below is invented for this file: built on
//! self-made, in-memory stores and throwaway git repos, never on the real
//! migrated data - a test that depends on private data is not a test.
//!
//! Three of the seven have a companion note (see the doc comment on each)
//! recording how the promise was proven to be able to fail: the guarding
//! source line was temporarily changed to violate the promise, the specific
//! test was run and observed red, the change was reverted, and the test was
//! observed green again. That is not part of the automated suite - it is a
//! one-time exercise recorded here so the claim "this test can fail" is
//! itself checked, not asserted.

use intent::Action;
use model::item::{Binding, Item, Kind, Severity};
use model::store;
use serve::input::ServeInput;
use serve::{live, lookup, prompt, rank, render, session_start};
use thor_core::event_store::{EventKind, EventStore};

// --------------------------------------------------------------- fixtures

fn rule_always(id: &str, project: Option<&str>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("standing rule {id}"),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Irreversible),
        project: project.map(str::to_string),
        // Gate ground 11 asks every heavy rule whether it can refuse. These
        // fixtures exist to test DELIVERY, and a generic "standing rule N" has
        // no literal to catch, so the tag is the truthful answer.
        tags: vec![model::store::NO_LITERAL_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: Some(format!("standing rule {id} turns out to be wrong")),
        check: None,
    }
}

fn rule_moment(id: &str, action: Action) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("never {} without checking first", action.as_str()),
        bindings: vec![Binding::Moment(action)],
        severity: Some(Severity::Irreversible),
        project: None,
        // Same as rule_always: asked, and there is nothing literal to catch.
        tags: vec![model::store::NO_LITERAL_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out to be safe without checking first")),
        check: None,
    }
}

/// A report worded to be as tempting a false positive as possible: it repeats
/// the moment action's own name and the "always" vocabulary, so if any
/// surface fell back to closeness-style ranking instead of a real binding
/// match, this is the item that would leak.
fn tempting_report(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Report,
        text: "always remember: before you push, force, publish or deploy anything, read this report first"
            .to_string(),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec!["push".to_string(), "always".to_string()],
        expires: Some("2027-01-01".to_string()),
        key: None,
        // A falsifier is optional for a Report, but set here anyway so this
        // fixture's own declare() call never depends on whether Report is
        // (correctly) excluded from Kind::can_fire elsewhere - the promise
        // this fixture exists to test is about the READ/select path, not the
        // write gate's separate falsifier requirement.
        falsifier: Some("this report turns out to be unnecessary".to_string()),
        check: None,
    }
}

/// A chunk worded the same tempting way, standing in for "a piece of code
/// that resembles the standing rules and the action about to happen".
fn tempting_chunk(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Chunk,
        text: "always push force publish deploy: fn push_to_remote() { /* standing rule */ }".to_string(),
        bindings: vec![],
        severity: None,
        project: Some("example-repo".to_string()),
        tags: vec!["push".to_string(), "always".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    }
}

/// Capture the exact rendered output of the three injection surfaces for a
/// fixed set of inputs, so growth of the store can be compared byte for
/// byte. Empty string stands for "nothing rendered" (0 bytes).
fn capture_three_surfaces(db: &EventStore) -> (String, String, String) {
    let candidates = live::live_items(db);

    let start_items = session_start::select(&candidates, Some("contract-project"));
    let start_block = session_start::render(&start_items).unwrap_or_default();

    let mut moment_input = ServeInput::default();
    moment_input.add_moment(Action::Push);
    let moment_all = rank::select(&candidates, &moment_input);
    let moment_selection = render::cap(moment_all);
    let moment_block = render::render_text(&moment_selection, &moment_input.moments).unwrap_or_default();

    let prompt_text = "please run git push --force origin main right now";
    let prompt_input = prompt::resolve(prompt_text, &candidates);
    let prompt_all = rank::select(&candidates, &prompt_input);
    let prompt_selection = render::cap(prompt_all);
    let prompt_block = render::render_text(&prompt_selection, &prompt_input.moments).unwrap_or_default();

    (start_block, moment_block, prompt_block)
}

/// A throwaway git repository under a tempdir, for the code-index half of
/// promise 7 - the same minimal fixture shape `serve::lookup`'s own tests use.
struct TestRepo {
    dir: tempfile::TempDir,
}

impl TestRepo {
    fn init() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(dir.path()).args(args).output().expect("git must be on PATH");
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@example.invalid"]);
        run(&["config", "user.name", "contract tests"]);
        TestRepo { dir }
    }

    fn write(&self, rel_path: &str, content: &str) {
        std::fs::write(self.dir.path().join(rel_path), content).unwrap();
    }

    fn commit(&self, message: &str) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(self.dir.path()).args(["add", "-A"]).output().unwrap();
        assert!(out.status.success());
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(self.dir.path())
            .args(["commit", "--quiet", "-m", message])
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = std::process::Command::new("git").arg("-C").arg(self.dir.path()).args(["rev-parse", "HEAD"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

// ------------------------------------------------------------- promise 1
//
// "Er verschijnt NOOIT een verslag of een code-brok in een injectie."
//
// Proven capable of failing: `model::item::Kind::can_fire` was temporarily
// changed from `matches!(self, Kind::Rule | Kind::Orientation)` to also
// include `Kind::Report`, and this test alone (`cargo test --release --test
// contract a_report_or_a_chunk_never_appears_in_an_injection_surface`) was
// run in isolation. Observed RED: `bypassed_report_with_a_real_binding`
// (constructed with a genuinely matching `Moment(Push)` binding, appended
// directly to the log so the write gate's own, separate ground-3 protection
// is never in the path at all) appeared in the moment and prompt surfaces'
// hits. The change was reverted and the same command was observed GREEN
// again.

#[test]
fn a_report_or_a_chunk_never_appears_in_an_injection_surface() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_always("always-1", None)).unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_moment("moment-1", Action::Push)).unwrap();
    store::declare(&mut db, "s", "l", "a", &tempting_report("report-1")).unwrap();
    store::declare(&mut db, "s", "l", "a", &tempting_chunk("chunk-1")).unwrap();

    // The write gate already refuses a Report/Chunk carrying a binding
    // (ground 3) - constructed here as if that check had never run (appended
    // directly, never through `store::declare`), WITH a binding that
    // genuinely matches the moment/prompt fixtures below, so this proves the
    // read/select path is a SECOND, independent lock on the door by kind, not
    // merely a side effect of archive material never carrying a binding in
    // practice.
    let bypassed_report_with_a_real_binding = Item {
        id: "bypassed-report-1".to_string(),
        kind: Kind::Report,
        text: "a report that would genuinely match if kind were not checked".to_string(),
        bindings: vec![Binding::Moment(Action::Push)],
        severity: None,
        project: None,
        tags: vec![],
        expires: Some("2027-01-01".to_string()),
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&bypassed_report_with_a_real_binding).unwrap();
    db.append_event("s", "l", "a", EventKind::FactCreated, &bypassed_report_with_a_real_binding.id, None, &body).unwrap();

    let candidates = live::live_items(&db);
    let archive_ids = ["report-1", "chunk-1", "bypassed-report-1"];

    // Surface 1: session start.
    let start_items = session_start::select(&candidates, None);
    assert!(
        start_items.iter().all(|r| !archive_ids.contains(&r.id.as_str())),
        "a Report or Chunk must never reach session start"
    );

    // Surface 2: moment of action.
    let mut moment_input = ServeInput::default();
    moment_input.add_moment(Action::Push);
    let moment_hits = rank::select(&candidates, &moment_input);
    assert!(
        moment_hits.iter().all(|r| !archive_ids.contains(&r.id.as_str())),
        "a Report or Chunk must never reach the moment surface, even one with a genuinely matching binding"
    );

    // Surface 3: per prompt.
    let prompt_input = prompt::resolve("please run git push --force origin main right now", &candidates);
    let prompt_hits = rank::select(&candidates, &prompt_input);
    assert!(
        prompt_hits.iter().all(|r| !archive_ids.contains(&r.id.as_str())),
        "a Report or Chunk must never reach the prompt surface, even one with a genuinely matching binding"
    );
}

// ------------------------------------------------------------- promise 2
//
// "Een niet-technisch onderwerp is vindbaar via opzoeken, ook vanuit een
// sessie die in een project zit."

#[test]
fn a_non_technical_topic_is_findable_via_lookup_even_from_a_session_inside_a_project() {
    let mut db = EventStore::in_memory().unwrap();
    // A plainly non-technical fact, filed globally (no project at all) -
    // nothing about code, paths or commands.
    let topic = Item {
        id: "office-1".to_string(),
        kind: Kind::Report,
        text: "the office thermostat is kept at 21 degrees C over winter".to_string(),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec!["office".to_string()],
        expires: Some("2027-01-01".to_string()),
        key: None,
        falsifier: None,
        check: None,
    };
    store::declare(&mut db, "s", "l", "a", &topic).unwrap();

    // A session standing inside a real project (a real .git root, no marker
    // file) - the same resolution `serve::project::resolve_project` performs
    // for session start and the hook channel.
    let workdir = tempfile::tempdir().unwrap();
    let repo = workdir.path().join("some-code-project");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let resolved_project = serve::project::resolve_project(&repo);
    assert_eq!(resolved_project.as_deref(), Some("some-code-project"), "fixture sanity: the session really is inside a project");

    // Lookup takes no project argument at all (see serve::lookup's own doc
    // comment) - the resolved project above has no bearing on it whatsoever.
    let hits = lookup::search(&db, "thermostat");
    assert_eq!(hits.len(), 1, "a non-technical topic filed globally must be findable regardless of the session's own project");
    assert_eq!(hits[0].id, "office-1");
}

// ------------------------------------------------------------- promise 3
//
// "Sessiestart levert de staande regels van de globale laag plus het huidige
// project, volledig en niet afgekapt."
//
// Proven capable of failing: `serve::session_start::select` was temporarily
// given an extra `.take(2)` after its filters, and this test alone was run
// in isolation. Observed RED: only 2 of the 8 expected standing rules came
// back. The change was reverted and the same command was observed GREEN
// again.

#[test]
fn session_start_delivers_the_full_global_and_project_standing_rules_uncapped() {
    let mut db = EventStore::in_memory().unwrap();
    // More than render::MAX_ITEMS (4) on each side, so truncation to that
    // cap would be caught here too, not only a total absence of capping.
    for i in 0..5 {
        store::declare(&mut db, "s", "l", "a", &rule_always(&format!("global-{i}"), None)).unwrap();
    }
    for i in 0..5 {
        store::declare(&mut db, "s", "l", "a", &rule_always(&format!("project-{i}"), Some("widget-factory"))).unwrap();
    }
    // A different project's standing rule must never leak in either.
    store::declare(&mut db, "s", "l", "a", &rule_always("other-project-rule", Some("some-other-project"))).unwrap();

    let candidates = live::live_items(&db);
    let items = session_start::select(&candidates, Some("widget-factory"));
    assert_eq!(items.len(), 10, "expected every global plus every widget-factory rule, none capped, none missing");

    let block = session_start::render(&items).expect("a non-empty selection must render a block");
    for i in 0..5 {
        assert!(block.contains(&format!("standing rule global-{i}")), "missing global-{i} in the block: {block}");
        assert!(block.contains(&format!("standing rule project-{i}")), "missing project-{i} in the block: {block}");
    }
    assert!(!block.contains("other-project-rule"), "a different project's rule must never leak into this block: {block}");
}

// ------------------------------------------------------------- promise 4
//
// "Het moment-oppervlak vuurt nog steeds voor een handeling."

#[test]
fn the_moment_surface_still_fires_for_a_real_action() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_moment("push-guard", Action::Push)).unwrap();

    let mut input = ServeInput::default();
    input.add_command("git push --force origin main");
    let served = serve::serve(&db, &input);

    assert!(
        served.selection.shown.iter().any(|r| r.id == "push-guard"),
        "a real, matching action must still make the moment surface fire"
    );
    let block = render::render_text(&served.selection, &input.moments).expect("a real match must render a block");
    assert!(block.contains("never push without checking first"), "expected the rule's own text in the block: {block}");
}

// ------------------------------------------------------------- promise 5
//
// "500 archief-items toevoegen laat de drie injectieblokken byte-identiek."

#[test]
fn adding_five_hundred_archive_items_leaves_the_three_injection_blocks_byte_identical() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_always("always-1", Some("contract-project"))).unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_moment("moment-1", Action::Push)).unwrap();

    let before = capture_three_surfaces(&db);
    assert!(
        !before.0.is_empty() && !before.1.is_empty() && !before.2.is_empty(),
        "the fixture must actually produce output on all three surfaces before growth, or this test proves nothing: {before:?}"
    );

    for i in 0..500 {
        let item = Item {
            id: format!("archive-{i}"),
            kind: if i % 2 == 0 { Kind::Report } else { Kind::Chunk },
            // Three tokens that each embed `i` as part of the word itself
            // (`entry{i}`, `marker{i}`, `moment{i}`), not as a separately
            // repeated bare number - model::store's near-duplicate check
            // compares normalised WORD SETS, so the same number written
            // twice in one string (the original shape here) collapses to a
            // single shared token and every Report (or every Chunk) ends up
            // a Jaccard near-duplicate of the last one declared. Three
            // distinct varying tokens against a fixed common template of 9
            // words keeps every pairwise Jaccard at 9/15 = 0.6, safely under
            // the 0.8 refusal threshold, while every text is still genuinely
            // distinct - this loop is about proving bulk, not repeating one
            // string 500 times.
            text: format!(
                "archive filler entry{i} marker{i}: always push force publish deploy standing rule moment{i}"
            ),
            bindings: vec![],
            severity: None,
            project: None,
            tags: vec!["push".to_string(), "always".to_string(), format!("tag-{i}")],
            expires: if i % 2 == 0 { Some("2027-01-01".to_string()) } else { None },
            key: None,
            falsifier: None,
            check: None,
        };
        store::declare(&mut db, "s", "l", "a", &item).unwrap();
    }

    let after = capture_three_surfaces(&db);
    assert_eq!(
        before, after,
        "adding 500 archive items must not change any injection surface's output at all - the store may grow without getting louder"
    );
}

// ------------------------------------------------------------- promise 6
//
// "Niets gaat verloren: elk item dat door de poort komt is daarna
// vindbaar."
//
// Proven capable of failing: `model::store::declare` was temporarily changed
// to append the event under the literal id `"wrong-id"` instead of
// `&item.id`, and this test alone was run in isolation. Observed RED: the
// item could no longer be read back under its own id (`store::show`
// returned `NotFound`), even though `declare` itself still returned `Ok`.
// The change was reverted and the same command was observed GREEN again -
// exactly the "the tool does less than it promised, in silence" defect
// class CONTRACT.md names as the reason this project exists.

#[test]
fn nothing_is_lost_every_item_that_passes_the_gate_is_findable_afterward() {
    let mut db = EventStore::in_memory().unwrap();

    let rule = rule_always("findable-rule", None);
    let mut orientation = rule_moment("findable-orientation", Action::Configure);
    orientation.kind = Kind::Orientation;
    orientation.text = "a distinctive orientation about the widget subsystem".to_string();
    let report = Item {
        id: "findable-report".to_string(),
        kind: Kind::Report,
        text: "a distinctive report about the widget rollout".to_string(),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec![],
        expires: Some("2027-01-01".to_string()),
        key: None,
        falsifier: None,
        check: None,
    };
    let chunk = Item {
        id: "findable-chunk".to_string(),
        kind: Kind::Chunk,
        text: "fn widget_distinctive_marker() {}".to_string(),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let lookup_item = Item {
        id: "findable-lookup".to_string(),
        kind: Kind::Lookup,
        text: "the widget release checklist lives in RELEASE.md".to_string(),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: Some("widget-release-checklist".to_string()),
        falsifier: None,
        check: None,
    };

    for item in [&rule, &orientation, &report, &chunk, &lookup_item] {
        store::declare(&mut db, "s", "l", "a", item).unwrap();
    }

    // The universal door: every kind, read back by its own id exactly as
    // declared, regardless of kind.
    for item in [&rule, &orientation, &report, &chunk, &lookup_item] {
        let back = store::show(&db, &item.id).unwrap_or_else(|e| panic!("{} must be findable via show: {e}", item.id));
        assert_eq!(&back, item, "{} must read back identical to what was declared", item.id);
    }

    // The search door: every kind except Lookup answers a text search.
    for (id, term) in [
        ("findable-rule", "standing rule findable-rule"),
        ("findable-orientation", "widget subsystem"),
        ("findable-report", "widget rollout"),
        ("findable-chunk", "widget_distinctive_marker"),
    ] {
        let hits = lookup::search(&db, term);
        assert!(hits.iter().any(|h| h.id == id), "{id} must be findable via lookup::search on '{term}': {:?}", hits.iter().map(|h| &h.id).collect::<Vec<_>>());
    }

    // The Lookup's own door: answered only by its key, never by search.
    let by_key = lookup::by_key(&db, "widget-release-checklist");
    assert_eq!(by_key.map(|h| h.id), Some("findable-lookup".to_string()), "the Lookup must be findable by its own key");
}

// ------------------------------------------------------------- promise 7
//
// "Code is vindbaar via opzoeken met zijn herkomst erbij, en onbereikbaar
// voor de drie injectie-oppervlakken."

#[test]
fn code_is_findable_via_lookup_with_its_provenance_and_unreachable_by_the_three_injection_surfaces() {
    // Half one: the code index itself, reached only through lookup::search_code,
    // with its provenance (indexed commit vs. current commit, drift count).
    let repo = TestRepo::init();
    repo.write("widget.rs", "fn widget_marker_9f2a() -> &'static str { \"a very findable fixture\" }");
    let commit = repo.commit("initial");

    let index_dir = tempfile::tempdir().unwrap();
    let index_db = index_dir.path().join("index.db");
    {
        let mut index_store = codeindex::Store::open(&index_db).unwrap();
        codeindex::build_full(&mut index_store, repo.path()).unwrap();
    }

    let answer = lookup::search_code(&index_db, repo.path(), "widget_marker_9f2a", 10).unwrap();
    assert_eq!(answer.hits.len(), 1, "expected exactly one code hit");
    assert_eq!(answer.hits[0].path, "widget.rs");
    assert_eq!(answer.hits[0].commit_id, commit);
    assert_eq!(answer.provenance.indexed_commit, commit);
    assert_eq!(answer.provenance.current_commit, Ok(commit));
    assert_eq!(answer.provenance.files_differ, Some(0), "a freshly built index must report zero drift");

    // Half two: a Chunk carrying that same code (the fact-store's own copy of
    // "a piece of source code pulled from a repo") must never reach any of
    // the three injection surfaces, while remaining fully reachable through
    // lookup::search - the same door as everything else archive.
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_always("always-1", None)).unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_moment("moment-1", Action::Push)).unwrap();
    let code_chunk = Item {
        id: "code-chunk-1".to_string(),
        kind: Kind::Chunk,
        text: "fn widget_marker_9f2a() -> &'static str { \"a very findable fixture\" }".to_string(),
        bindings: vec![],
        severity: None,
        project: Some("widget-repo".to_string()),
        tags: vec!["rust".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    store::declare(&mut db, "s", "l", "a", &code_chunk).unwrap();

    // Reachable via lookup (surface 4).
    let hits = lookup::search(&db, "widget_marker_9f2a");
    assert!(hits.iter().any(|h| h.id == "code-chunk-1"), "the code chunk must be findable via lookup::search");

    // Unreachable via the three injection surfaces.
    let candidates = live::live_items(&db);
    let start_items = session_start::select(&candidates, None);
    assert!(start_items.iter().all(|r| r.id != "code-chunk-1"), "code must never reach session start");

    let mut moment_input = ServeInput::default();
    moment_input.add_moment(Action::Push);
    let moment_hits = rank::select(&candidates, &moment_input);
    assert!(moment_hits.iter().all(|r| r.id != "code-chunk-1"), "code must never reach the moment surface");

    let prompt_input = prompt::resolve("please run git push --force origin main right now", &candidates);
    let prompt_hits = rank::select(&candidates, &prompt_input);
    assert!(prompt_hits.iter().all(|r| r.id != "code-chunk-1"), "code must never reach the prompt surface");
}
