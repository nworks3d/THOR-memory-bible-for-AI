//! `why`'s argument parsing (`TargetArgs` in `bin/serve.rs`), proven end to
//! end against the real compiled `serve` binary: the bare positional path is
//! `--file`'s own shorthand, and the withheld-item hint
//! (`render::render_text`, built by `render::why_invocation`) names a flag
//! this parser genuinely accepts - not merely plausible-looking text.
//!
//! THE DEFECT THIS PREVENTS. The withheld-item hint used to read
//! "run `serve why`" with nothing after it - not a flag `why`'s own parser
//! required, and not even the file that made the block fire in the first
//! place, so following it verbatim re-asked an EMPTY question instead of the
//! one just answered. One session's own report on trying to guess past it:
//! "the help command for what fires here had a different flag than the hint
//! said: --file, not a path." `render`'s own test module
//! (`serve/src/render.rs`) proves the hint TEXT names the right flag at the
//! unit level; this file proves the two things a unit test cannot: the real
//! compiled parser actually accepts both forms, and running exactly what the
//! hint says reproduces the same items `check` already knew were withheld.

use intent::Action;
use model::item::{Binding, Item, Kind, TargetKind};
use model::store;
use std::path::Path;
use std::process::{Command, Output};
use thor_core::event_store::EventStore;

/// Five genuinely different sentences, never a templated "fixture rule named
/// X" - `model::store`'s near-duplicate refusal compares NORMALISED text
/// within the same kind, and text that differs by only one embedded token
/// (an id or a number) still reads as the same fact said twice. Same fixture
/// shape `bin/serve.rs`'s own `crowd_a_moment` test helper uses for exactly
/// this reason (several Rules competing for one pool without tripping that
/// refusal).
const DISTINCT_TEXTS: [&str; 5] = [
    "a webhook retry backs off before it gives up entirely",
    "the estimator rounds a quote up to whole cents",
    "a spool label carries the batch it came from",
    "the scheduler skips a printer that is on hold",
    "an invoice number never restarts inside a year",
];

/// A globally-scoped Rule anchored at `path` - deliberately a doc file
/// extension (`.md`), never a source extension (`.rs`/`.py`/...): a GLOBAL
/// item anchored on program source is refused at write time (ground 19,
/// `model::gate`, "a global anchor on src/main.rs fires in every repo that
/// has such a file"), and this fixture has no project to scope it under.
fn declare_path_rule(store: &mut EventStore, id: &str, text: &str, path: &str) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Path, value: path.to_string() }],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out not to matter after all")),
        check: None,
    };
    store::declare(store, "s", "l", "a", &item).expect("fixture must store");
}

/// A globally-scoped Rule bound to `action`, never to a Path/Dir/Command
/// target - since 2.2.1 (2026-09-05) the write gate refuses a Path/Dir/
/// Command anchor that already holds `model::item::MAX_ITEMS` live rivals at
/// ANY weight (`model::store::capacity`'s `Full` case), so five items all
/// anchored to the same file could never be declared at all. A Moment
/// binding is deliberately exempt (`capacity`'s own doc comment: "A Moment
/// binding still only ever gets the warning") - this fixture uses that door
/// to get more than `MAX_ITEMS` real candidates onto one surface, the same
/// way `bin/serve.rs`'s own `crowd_a_moment` test helper does for a Command
/// moment.
fn declare_moment_rule(store: &mut EventStore, id: &str, text: &str, action: Action) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Moment(action)],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out not to matter after all")),
        check: None,
    };
    store::declare(store, "s", "l", "a", &item).expect("fixture must store");
}

fn run_why(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("why")
        .args(args)
        .output()
        .expect("spawn serve why")
}

/// THE CORE PROOF: the shortest form a new user will actually try (a bare
/// path, no flag remembered) answers the identical question `--file` does -
/// byte for byte, not merely "both non-empty".
#[test]
fn a_bare_positional_path_and_the_file_flag_produce_the_same_output() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        declare_path_rule(&mut store, "readme-rule", DISTINCT_TEXTS[0], "README.md");
    }

    let positional = run_why(&db, &["README.md"]);
    assert!(positional.status.success(), "serve why <path> must be accepted: {positional:?}");
    let flagged = run_why(&db, &["--file", "README.md"]);
    assert!(flagged.status.success(), "serve why --file <path> must be accepted: {flagged:?}");

    let positional_out = String::from_utf8(positional.stdout).unwrap();
    let flagged_out = String::from_utf8(flagged.stdout).unwrap();
    assert_eq!(positional_out, flagged_out, "the shorthand and the flag must answer the identical question");
    assert!(positional_out.contains("readme-rule"), "expected the fixture rule to actually apply: {positional_out}");
}

/// The positional is `--file`'s own shorthand, not a second, independent
/// target - giving both at once must be refused rather than silently
/// picking one (see `TargetArgs::path`'s `conflicts_with = "file"`).
#[test]
fn giving_both_the_positional_and_the_file_flag_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    EventStore::new(&db).unwrap();

    let out = run_why(&db, &["README.md", "--file", "README.md"]);
    assert!(!out.status.success(), "the parser must refuse two file targets at once, not silently pick one");
    assert!(!out.stderr.is_empty(), "the refusal must be explained: {out:?}");
}

/// THE FULL LOOP, past what a unit test can prove: not just that the hint's
/// own text NAMES `--file` (`render`'s own test module proves that in
/// isolation), but that running exactly what it says reproduces the SAME
/// items `check` already knew were being withheld - the flag is a genuinely
/// working invocation, not merely plausible-looking text.
///
/// Triggered through a `.env`-shaped path (`intent::from_path`'s own
/// Credentials rule, "(^|/)\.env(\.|$)") rather than a Path-target anchor:
/// the fixture rules are Moment-bound (see `declare_moment_rule`'s own doc
/// comment for why), so what actually seats them is the Credentials moment
/// `--file app/.env` derives - `render::why_invocation` still names `--file`
/// in the hint regardless, because it reads what was ASKED
/// (`ServeInput::file`), never how each item happened to match.
#[test]
fn the_flag_named_in_the_withheld_hint_actually_reproduces_the_withheld_items() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        // One more than MAX_ITEMS (4), so the moment-of-action preview
        // (`check`) withholds at least one and prints the hint this proves.
        for (i, text) in DISTINCT_TEXTS.iter().enumerate() {
            declare_moment_rule(&mut store, &format!("rule-{i}"), text, Action::Credentials);
        }
    }

    let check_out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db)
        .arg("check")
        .arg("--file")
        .arg("app/.env")
        .output()
        .expect("spawn serve check");
    assert!(check_out.status.success());
    let check_text = String::from_utf8(check_out.stdout).unwrap();
    assert!(
        check_text.contains("why --file \"app/.env\"` to see them"),
        "expected the exact working invocation in the hint: {check_text}"
    );
    // THE DEFECT THIS PREVENTS, reported 2026-09-12: the hint named the
    // right flag but still opened with the bare word `serve`, which is
    // never on PATH for this deployment - following it verbatim answered
    // "command not found" on every machine, this one included. The real
    // compiled binary must now name ITS OWN absolute path (exactly the path
    // this test used to spawn it, `CARGO_BIN_EXE_serve`, since
    // `std::env::current_exe` inside that spawned process resolves to the
    // same path it was launched from) and the exact `--db` it was opened
    // with - not a unit-level plausible string, the real running program's
    // own answer.
    let expected_self_invocation = if cfg!(windows) {
        format!("& \"{}\" --db \"{}\" why --file \"app/.env\"", env!("CARGO_BIN_EXE_serve"), db.display())
    } else {
        format!("\"{}\" --db \"{}\" why --file \"app/.env\"", env!("CARGO_BIN_EXE_serve"), db.display())
    };
    assert!(
        check_text.contains(&expected_self_invocation),
        "expected the real binary's own absolute path and its real --db in the hint: {check_text}"
    );

    let why_out = run_why(&db, &["--file", "app/.env"]);
    assert!(why_out.status.success());
    let why_text = String::from_utf8(why_out.stdout).unwrap();
    for i in 0..5 {
        assert!(why_text.contains(&format!("rule-{i}")), "expected every fixture rule listed by why: {why_text}");
    }
}
