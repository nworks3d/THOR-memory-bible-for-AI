//! The `mark` subcommand, proven against the REAL compiled `serve` binary,
//! not just the library function: running `serve mark <id>` on a decayed
//! item must cancel its decay, and a real failure (an unopenable store) must
//! be reported plainly and exit non-zero - `mark` is a human-facing, "write
//! and declare" command (R5), unlike `hook`'s silent telemetry.

use intent::Action;
use model::item::{Binding, Item, Kind, Severity};
use model::store;
use serve::input::ServeInput;
use serve::{decay, deliver};
use std::process::Command;
use thor_core::event_store::EventStore;

fn moment_rule(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("never {id} without checking first"),
        bindings: vec![Binding::Moment(Action::Push)],
        severity: Some(Severity::Irreversible),
        project: None,
        // Gate ground 11: this tests the mark CLI, not teeth, and a generic
        // "never <id> without checking first" has no literal to catch.
        tags: vec![model::store::NO_LITERAL_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out to be safe without checking first")),
        check: None,
    }
}

#[test]
/// The mark command must work end to end through the real binary: it writes a
/// usefulness mark into the log and says so. It no longer changes what applies
/// - silence stopped removing anything on 2026-08-03, see `serve::decay`'s own
/// module doc - so this pins the WRITE and the confirmation, and that a mark
/// never removes anything either.
fn the_mark_cli_records_a_usefulness_mark_through_the_real_binary() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("store.db");
    {
        let mut store_handle = EventStore::new(&db_path).unwrap();
        store::declare(&mut store_handle, "s", "l", "a", &moment_rule("cli-mark-1")).unwrap();
        for i in 0..decay::STALE_AFTER_SERVINGS {
            deliver::record_delivery(
                &mut store_handle, "s", "l", "serve", &format!("2026-08-{:02}T00:00:00Z", i + 1),
                &["cli-mark-1".to_string()],
            );
        }
    }

    // Sanity: served far past the old threshold, never marked, and it applies.
    {
        let store_handle = EventStore::new(&db_path).unwrap();
        let mut input = ServeInput::default();
        input.add_moment(Action::Push);
        let served = serve::serve(&store_handle, &input);
        assert!(served.all.iter().any(|r| r.id == "cli-mark-1"), "fixture sanity: it applies before the mark");
    }

    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db_path)
        .arg("mark")
        .arg("cli-mark-1")
        .output()
        .expect("failed to run the serve binary");
    assert!(out.status.success(), "mark must exit 0 on a real store: {:?}", out);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("cli-mark-1"), "expected confirmation naming the id, got: {stdout}");

    let store_handle = EventStore::new(&db_path).unwrap();
    let mut input = ServeInput::default();
    input.add_moment(Action::Push);
    let served = serve::serve(&store_handle, &input);
    assert!(
        served.all.iter().any(|r| r.id == "cli-mark-1"),
        "and it still applies after the mark - a mark never removes anything either"
    );
}

#[test]
fn the_mark_cli_reports_a_real_error_plainly_and_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("no").join("such").join("dir").join("store.db");

    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db_path)
        .arg("mark")
        .arg("whatever")
        .output()
        .expect("failed to run the serve binary");
    assert!(!out.status.success(), "mark against an unopenable store must not exit 0");
    assert!(!out.stderr.is_empty(), "a human-facing write command must report the failure, never stay silent");
}
