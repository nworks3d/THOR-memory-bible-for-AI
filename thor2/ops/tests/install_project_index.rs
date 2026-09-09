//! CLI-level proof for the installer's project scoping and code indexing,
//! run against the REAL compiled `install` binary - see `bin/install.rs`'s
//! own "THE DEFECT THIS CLOSES" doc comment on step 5 for the full story.
//!
//! THE DEFECT THIS FILE GUARDS AGAINST, measured 2026-09-09 in a sandboxed
//! first run that followed the setup page literally: running `install`
//! without `--project` inside a git repository wrote no `.thor-project`
//! marker and built no code index at all, and said nothing about it - both
//! lived behind `if let Some(key) = &cli.project`. A newcomer following the
//! setup page (which tells them to leave the flag off most of the time,
//! since a project is already named after its own folder) ended up with a
//! memory that could never answer `search_code`, with no error anywhere.
//!
//! Every test spawns the real `install.exe` with HOME, USERPROFILE and
//! LOCALAPPDATA pointed at a throwaway sandbox directory, plus
//! GIT_CONFIG_GLOBAL and GIT_CONFIG_NOSYSTEM, so the one git call this
//! binary makes with `--global` (wiring `core.hooksPath`, see
//! `ops::githooks::wire_hooks_path`) can never reach a real machine's own
//! git configuration - never the machine actually running this suite,
//! never the one running any other. `--no-mcp` is passed throughout: these
//! tests are about the project marker and the code index, not the tool
//! server registration `install_tool_server.rs`'s own tests already cover.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A throwaway per-user environment for one test: HOME/USERPROFILE/
/// LOCALAPPDATA all inside one temp directory nothing else on the machine
/// reads. `install` resolves its store, its settings.json and its code
/// index root from exactly these three variables (see `ops::install::
/// default_data_dir` and `default_settings_path`), so pointing them here is
/// what keeps a test run from ever touching a real machine's own THOR store
/// or `~/.claude`.
struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
    }

    fn local_app_data(&self) -> PathBuf {
        self.home.path().join("localappdata")
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path());
        cmd.env("USERPROFILE", self.home.path());
        cmd.env("LOCALAPPDATA", self.local_app_data());
        // Belt and braces beyond HOME/USERPROFILE: these two guarantee
        // `git config --global core.hooksPath ...` (the installer's one
        // `--global` write) reads and writes ONLY this sandbox's throwaway
        // file, never a real `.gitconfig`, however git on this machine
        // would otherwise have resolved the global config path.
        cmd.env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"));
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().expect("git must be on PATH");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

/// A throwaway git repository with one committed file, so
/// `codeindex::build_full` has a HEAD and a tree to read - the same shape
/// `serve/tests/status_and_search_code_cli.rs` builds for the same reason.
fn git_repo_with_a_file(dir: &Path) {
    git(dir, &["init", "--quiet"]);
    git(dir, &["config", "user.email", "test@example.invalid"]);
    git(dir, &["config", "user.name", "install cli tests"]);
    std::fs::write(dir.join("main.rs"), "fn main() { println!(\"a findable fixture sentence\"); }\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", "initial"]);
}

fn run_install(repo: &Path, sandbox: &Sandbox, extra_args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
    cmd.current_dir(repo).arg("--no-mcp").args(extra_args);
    sandbox.apply(&mut cmd);
    cmd.output().unwrap()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// THE DEFECT ITSELF: leaving `--project` off used to skip step 5 entirely,
/// silently. Fixed, this must read the code under the folder's own name
/// (`serve::project::resolve_project`'s git-root-basename fallback) and
/// write no marker - the folder name is not something this run decided, so
/// nothing should freeze it.
#[test]
fn no_project_flag_indexes_under_the_folder_name_and_writes_no_marker() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("sample-project");
    std::fs::create_dir(&repo).unwrap();
    git_repo_with_a_file(&repo);
    let sandbox = Sandbox::new();

    let out = run_install(&repo, &sandbox, &[]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("read this project's code"),
        "leaving --project off must still read the code, under the folder's own name:\n{stdout}"
    );

    assert!(!repo.join(".thor-project").exists(), "no --project must write no marker file");

    let index_db = sandbox.local_app_data().join("thor2").join("codeindex").join("sample-project.db");
    assert!(
        index_db.exists(),
        "the code must be indexed under the folder-derived name at {}",
        index_db.display()
    );
}

/// THE FLAG'S SURVIVING JOB: with `--project`, the marker is still written
/// and the index still follows it - under the NAME THE FLAG GAVE, not the
/// folder's own name, which is why the fixture folder and the `--project`
/// value below deliberately differ.
#[test]
fn the_project_flag_still_writes_the_marker_and_indexes_under_it() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("sample-project");
    std::fs::create_dir(&repo).unwrap();
    git_repo_with_a_file(&repo);
    let sandbox = Sandbox::new();

    let out = run_install(&repo, &sandbox, &["--project", "custom-name"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("this folder now has its own memory"), "{stdout}");
    assert!(stdout.contains("read this project's code"), "{stdout}");

    let marker = repo.join(".thor-project");
    assert!(marker.exists(), "--project must still write the marker");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "custom-name");

    let index_db = sandbox.local_app_data().join("thor2").join("codeindex").join("custom-name.db");
    assert!(
        index_db.exists(),
        "the code must be indexed under the name --project gave, not the folder's own: {}",
        index_db.display()
    );
    let folder_named_db = sandbox.local_app_data().join("thor2").join("codeindex").join("sample-project.db");
    assert!(!folder_named_db.exists(), "the folder's own name must not also get an index");
}

/// THE OTHER HALF OF THE FIX: with no marker, no git root anywhere above,
/// and no `--project`, there is honestly no name to index under - this must
/// stay a no-op, but say so, rather than repeat the old silence.
#[test]
fn outside_any_git_repository_nothing_is_indexed_and_the_installer_says_so() {
    // Deliberately not git-initialised: matches
    // `serve/src/project.rs`'s own `no_marker_and_no_git_root_yields_no_project`
    // fixture, which relies on the OS temp directory sitting outside any
    // repository.
    let dir = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::new();

    let out = run_install(dir.path(), &sandbox, &[]);
    assert!(
        out.status.success(),
        "a folder naming no project must not fail the whole install: stderr {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.lines().any(|l| l.contains("no git repository was found above this folder")),
        "leaving --project off outside any git repository must say plainly that nothing was read, not stay silent:\n{stdout}"
    );

    let codeindex_dir = sandbox.local_app_data().join("thor2").join("codeindex");
    assert!(
        !codeindex_dir.exists(),
        "nothing must be indexed when no project could be named: {}",
        codeindex_dir.display()
    );
}

/// THE EXISTING REFUSAL, still exercised at the CLI level after the step 5
/// refactor: re-scoping a folder that already has a marker must still be
/// refused, never silently overwritten - see `ops::install::
/// write_project_marker`'s own unit test `the_marker_refuses_to_rescope_a_folder`
/// for the same refusal proven directly against the library function.
#[test]
fn the_project_flag_still_refuses_to_rescope_an_existing_marker() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("sample-project");
    std::fs::create_dir(&repo).unwrap();
    git_repo_with_a_file(&repo);
    let sandbox = Sandbox::new();

    let first = run_install(&repo, &sandbox, &["--project", "first-key"]);
    assert!(first.status.success(), "stderr: {}", String::from_utf8_lossy(&first.stderr));

    let second = run_install(&repo, &sandbox, &["--project", "second-key"]);
    assert!(
        !second.status.success(),
        "re-scoping an existing marker must be refused, not silently accepted"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("first-key") && stderr.contains("second-key"),
        "the refusal must name both the key that is there and the one that was asked for: {stderr}"
    );

    assert_eq!(
        std::fs::read_to_string(repo.join(".thor-project")).unwrap().trim(),
        "first-key",
        "a refused re-scope must leave the marker exactly as it was"
    );
}
