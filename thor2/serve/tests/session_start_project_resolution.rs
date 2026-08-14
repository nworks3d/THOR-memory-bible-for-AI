//! CONTRACT gap 1, proven against the REAL compiled `serve` binary, not just
//! `project::resolve_project` as a library function: session start (the CLI
//! preview AND the real `hook` channel) must resolve "the current project"
//! from a filesystem location on its own, with no `--project`/`"project"`
//! field required any more - and the hook channel's new project-resolution
//! step must still fail open on a corrupt store, exactly like every other
//! read on that boundary (R5).

use model::item::{Binding, Item, Kind, Severity};
use model::store;
use std::io::Write;
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

fn rule_always_for_project(id: &str, project: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("standing rule for {project}"),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Irreversible),
        project: Some(project.to_string()),
        // Gate ground 11: this file tests project scoping, and a generic
        // standing rule has no literal to catch.
        tags: vec![format!("{}a test fixture with nothing literal to catch", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some(format!("the standing rule for {project} turns out to be wrong")),
        check: None,
    }
}

/// A repository directory (a real `.git` entry, no marker file) whose
/// basename is the project key `project::resolve_project` must derive.
fn make_repo_dir(parent: &std::path::Path, repo_name: &str) -> std::path::PathBuf {
    let repo = parent.join(repo_name);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    repo
}

#[test]
fn session_start_cli_resolves_the_project_from_the_process_working_directory() {
    let workdir = tempfile::tempdir().unwrap();
    let repo = make_repo_dir(workdir.path(), "sample-widget-repo");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("store.db");
    {
        let mut store = EventStore::new(&db_path).unwrap();
        store::declare(&mut store, "s", "l", "a", &rule_always_for_project("proj-rule", "sample-widget-repo")).unwrap();
    }

    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db_path)
        .arg("session-start")
        // deliberately no --project: the CLI must resolve it from cwd alone
        .current_dir(&repo)
        .output()
        .expect("failed to run the serve binary");

    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.starts_with("project: sample-widget-repo"), "expected the resolved project on the first line, got: {stdout}");
    assert!(stdout.contains("standing rule for sample-widget-repo"), "expected the project-scoped rule in the block: {stdout}");
}

fn run_hook(db_path: &std::path::Path, cwd: &std::path::Path, stdin_payload: &str) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db_path)
        .arg("hook")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start the serve binary");
    let _ = child.stdin.take().unwrap().write_all(stdin_payload.as_bytes());
    let out = child.wait_with_output().expect("failed to wait on the serve binary");
    (out.status, out.stdout, out.stderr)
}

#[test]
fn the_hook_channel_resolves_the_project_from_the_payloads_cwd_field() {
    let workdir = tempfile::tempdir().unwrap();
    let repo = make_repo_dir(workdir.path(), "another-sample-repo");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("store.db");
    {
        let mut store = EventStore::new(&db_path).unwrap();
        store::declare(&mut store, "s", "l", "a", &rule_always_for_project("proj-rule-2", "another-sample-repo")).unwrap();
    }

    let payload = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "SessionStart",
        "cwd": repo.to_string_lossy(),
    })
    .to_string();

    // Run from an unrelated directory: only the payload's own "cwd" field
    // may drive the resolution, never the process' actual working directory.
    let (status, stdout, stderr) = run_hook(&db_path, workdir.path(), &payload);
    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty());
    let out = String::from_utf8(stdout).unwrap();
    assert!(out.contains("standing rule for another-sample-repo"), "expected the project-scoped rule via the hook, got: {out}");
}

#[test]
fn a_different_projects_always_item_never_leaks_through_the_hook_channel() {
    let workdir = tempfile::tempdir().unwrap();
    let repo = make_repo_dir(workdir.path(), "repo-a");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("store.db");
    {
        let mut store = EventStore::new(&db_path).unwrap();
        store::declare(&mut store, "s", "l", "a", &rule_always_for_project("rule-b", "repo-b")).unwrap();
    }

    let payload = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "SessionStart",
        "cwd": repo.to_string_lossy(),
    })
    .to_string();

    let (status, stdout, _stderr) = run_hook(&db_path, workdir.path(), &payload);
    assert_eq!(status.code(), Some(0));
    assert!(stdout.is_empty(), "repo-b's rule must never appear while resolved into repo-a's session, got: {:?}", stdout);
}

#[test]
fn the_sessionstart_hook_still_fails_open_on_a_corrupt_store_after_resolving_a_real_project() {
    // R5, extended to the new project-resolution step: a corrupt store must
    // still make the hook exit 0 and print nothing, even when the cwd it
    // resolves from is a perfectly real git repository.
    let workdir = tempfile::tempdir().unwrap();
    let repo = make_repo_dir(workdir.path(), "corrupt-store-repo");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("corrupt.db");
    std::fs::write(&db_path, b"this is not a sqlite database, just garbage bytes").unwrap();

    let payload = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "SessionStart",
        "cwd": repo.to_string_lossy(),
    })
    .to_string();

    let (status, stdout, stderr) = run_hook(&db_path, workdir.path(), &payload);
    assert_eq!(status.code(), Some(0), "hook must exit 0 even against a corrupt store");
    assert!(stdout.is_empty(), "hook must print zero bytes on failure, got: {:?}", stdout);
    assert!(stderr.is_empty());
}
