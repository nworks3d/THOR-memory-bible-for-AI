//! CONTRACT gap 3, proven against the REAL compiled `serve` binary: the
//! `status` command reports kind counts and the never-fired count, plus the
//! code index's own commit/drift when one is named; `search-code` answers a
//! code query with provenance on every hit, and reports a real error - never
//! a panic - when the index or repository cannot be reached. These are
//! human-facing lookups/diagnostics, not injection surfaces, so a real error
//! is allowed to be loud (unlike `hook`).

use model::item::{Binding, Item, Kind};
use model::store;
use std::process::Command;
use thor_core::event_store::EventStore;

fn item(id: &str, kind: Kind) -> Item {
    Item {
        id: id.to_string(),
        kind,
        // Text includes `id` so two calls with different ids (the only way
        // this helper is used with the same kind, see the "r1"/"r2" call
        // site below) never collide with model::store's near-duplicate
        // refusal, which compares normalised text within the same kind.
        text: format!("do the {id} thing"),
        bindings: if kind == Kind::Rule { vec![Binding::Always] } else { vec![] },
        severity: None,
        project: None,
        tags: vec![],
        expires: if kind == Kind::Report { Some("2027-01-01".to_string()) } else { None },
        key: if kind == Kind::Lookup { Some(format!("{id}-key")) } else { None },
        falsifier: if kind.can_fire() { Some("this synthetic fixture item turns out to be wrong".to_string()) } else { None },
        check: None,
    }
}

#[test]
fn status_cli_reports_kind_counts_and_never_fired_with_no_code_index_named() {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("store.db");
    {
        let mut store = EventStore::new(&db_path).unwrap();
        // Neither rule is ever delivered here (no hook run, no record_delivery
        // call) - that absence is exactly what "declared, never fired: 2 of 2"
        // below asserts.
        store::declare(&mut store, "s", "l", "a", &item("r1", Kind::Rule)).unwrap();
        store::declare(&mut store, "s", "l", "a", &item("r2", Kind::Rule)).unwrap();
    }

    let out = Command::new(env!("CARGO_BIN_EXE_serve")).arg("--db").arg(&db_path).arg("status").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Rule"), "expected a Rule row: {stdout}");
    assert!(stdout.contains("declared, never fired: 2 of 2"), "expected both undelivered rules counted: {stdout}");
    assert!(stdout.contains("not configured"), "no --index-db/--repo were given, so the code index must say so plainly: {stdout}");
}

#[test]
fn status_cli_reports_the_code_indexs_commit_when_configured() {
    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(repo.path()).args(args).output().expect("git must be on PATH");
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "test@example.invalid"]);
    run(&["config", "user.name", "status cli tests"]);
    std::fs::write(repo.path().join("a.txt"), "hello from the status test fixture").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "initial"]);
    let commit_out = Command::new("git").arg("-C").arg(repo.path()).args(["rev-parse", "HEAD"]).output().unwrap();
    let commit = String::from_utf8_lossy(&commit_out.stdout).trim().to_string();

    let index_dir = tempfile::tempdir().unwrap();
    let index_db = index_dir.path().join("index.db");
    {
        let mut store = codeindex::Store::open(&index_db).unwrap();
        codeindex::build_full(&mut store, repo.path()).unwrap();
    }

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("store.db");
    EventStore::new(&db_path).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db_path)
        .arg("status")
        .arg("--index-db")
        .arg(&index_db)
        .arg("--repo")
        .arg(repo.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains(&commit), "expected the real indexed commit in the status line: {stdout}");
    assert!(stdout.contains("0 file(s) differ"), "a freshly built index must report zero drift: {stdout}");
}

#[test]
fn search_code_cli_reports_a_hit_with_its_commit_and_provenance() {
    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(repo.path()).args(args).output().expect("git must be on PATH");
        assert!(out.status.success());
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "test@example.invalid"]);
    run(&["config", "user.name", "search code cli tests"]);
    std::fs::write(repo.path().join("greeter.txt"), "a very findable fixture sentence").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "initial"]);

    let index_dir = tempfile::tempdir().unwrap();
    let index_db = index_dir.path().join("index.db");
    {
        let mut store = codeindex::Store::open(&index_db).unwrap();
        codeindex::build_full(&mut store, repo.path()).unwrap();
    }

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("store.db");
    EventStore::new(&db_path).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db_path)
        .arg("search-code")
        .arg("--index-db")
        .arg(&index_db)
        .arg("--repo")
        .arg(repo.path())
        .arg("--limit")
        .arg("5")
        .arg("findable fixture")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("greeter.txt"), "expected the matching file's path: {stdout}");
    assert!(stdout.contains("a very findable fixture sentence"), "expected the matching text verbatim: {stdout}");
    assert!(stdout.contains("code index at commit"), "expected the provenance line before the hits: {stdout}");
}

#[test]
fn search_code_cli_reports_a_named_error_not_a_panic_for_an_unbuilt_index() {
    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new("git").arg("-C").arg(repo.path()).args(args).output().expect("git must be on PATH")
    };
    run(&["init", "--quiet"]);

    let index_dir = tempfile::tempdir().unwrap();
    let index_db = index_dir.path().join("never-built.db");
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("store.db");
    EventStore::new(&db_path).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db_path)
        .arg("search-code")
        .arg("--index-db")
        .arg(&index_db)
        .arg("--repo")
        .arg(repo.path())
        .arg("anything")
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0), "an unbuilt index must be a real, non-zero exit");
    assert!(!out.stderr.is_empty(), "the error must be reported plainly, never swallowed");
}
