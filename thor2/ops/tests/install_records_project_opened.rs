//! Proof that a successful `install` run opens the project it just read in
//! the MEMORY itself, not only in the code-index sidecar.
//!
//! THE DEFECT THIS GUARDS AGAINST. `install` (no `--project`) reads a
//! repository's code under its resolved project name (`build_project_index`
//! in `../src/bin/install.rs`), but until `ops::install::record_project_opened`
//! existed, nothing ever told the MAIN store that name was open. The write
//! gate's own collection check (`mcp::refuse_a_new_collection`, in
//! `mcp/src/lib.rs`) passes a project only when a `.thor-project` marker
//! names it (which plain `install` never writes) or the scope catalogue
//! (`serve::lookup::catalog`) already lists it - and the code index is a
//! sidecar that catalogue never reads. So the very first well-formed note
//! filed under a freshly-installed project was refused as an unknown
//! collection.
//!
//! Mirrors `install_project_index.rs`'s own sandbox - read its header for why
//! HOME/USERPROFILE/LOCALAPPDATA/GIT_CONFIG_GLOBAL/GIT_CONFIG_NOSYSTEM are all
//! pinned to a throwaway directory, and why `--no-mcp` is passed throughout.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thor_core::event_store::EventStore;

/// A throwaway per-user environment for one test - see
/// `install_project_index.rs`'s identical helper for the full reasoning.
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

    fn db(&self) -> PathBuf {
        self.local_app_data().join("thor2").join("thor.db")
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path());
        cmd.env("USERPROFILE", self.home.path());
        cmd.env("LOCALAPPDATA", self.local_app_data());
        cmd.env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"));
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().expect("git must be on PATH");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

/// A throwaway git repository with one committed file - same shape
/// `install_project_index.rs`'s own `git_repo_with_a_file` builds.
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

/// THE DEFECT ITSELF, end to end: after a successful first install the store
/// must hold the project-opened record, `install`'s own output must say so,
/// and a `remember` filed under that same project must pass the write gate's
/// collection check with no naming question - driven through
/// `mcp::ThorMcpServer::apply_captured`, the exact entry point `ops::drain`
/// already uses to replay a captured call into the write gate from this
/// crate (see `../src/drain.rs`), rather than re-implementing the gate check.
#[tokio::test]
async fn a_successful_install_records_the_project_and_a_remember_under_it_passes_the_gate() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("demo-shop");
    std::fs::create_dir(&repo).unwrap();
    git_repo_with_a_file(&repo);
    let sandbox = Sandbox::new();

    let out = run_install(&repo, &sandbox, &[]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("recorded this project"),
        "install must say plainly that it recorded the project in the memory:\n{stdout}"
    );

    let db = sandbox.db();
    let store = EventStore::open_existing(&db).unwrap();
    let item = model::store::show(&store, "project-demo-shop-opened-by-install")
        .expect("the project-opened record must be live in the store");
    assert_eq!(item.kind, model::item::Kind::Report);
    assert_eq!(item.project.as_deref(), Some("demo-shop"));

    // No `--project` was given, so no `.thor-project` marker exists either -
    // the record above is the ONLY thing that can open the collection.
    assert!(!repo.join(".thor-project").exists());

    let server = mcp::ThorMcpServer::new(store).with_root(repo.clone());
    let op = thor_core::inbox::InboxOp::new(
        "remember",
        serde_json::json!({
            "id": "demo-shop-first-note",
            "kind": "report",
            "text": "The demo-shop checkout ships its estimator config from config/estimator.toml.",
            "project": "demo-shop",
        }),
    );
    match server.apply_captured(&op).await {
        mcp::CapturedOutcome::Applied(text) => assert!(text.starts_with("stored "), "{text}"),
        mcp::CapturedOutcome::NotApplied(text) => {
            panic!("the first well-formed note filed under demo-shop was refused: {text}")
        }
    }
}

/// THE OTHER PROOF: with no commit at all, `build_project_index` refuses to
/// read the code (`git rev-parse HEAD` has nothing to resolve), so nothing
/// was ever successfully read, and no record may appear under any name.
#[test]
fn a_repository_with_no_commit_gets_no_record() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("no-commit-yet");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet"]); // a repository, but with no commit: no HEAD to read
    let sandbox = Sandbox::new();

    let out = run_install(&repo, &sandbox, &[]);
    assert!(out.status.success(), "a folder naming a project that cannot be read must not fail the whole install: stderr {}", String::from_utf8_lossy(&out.stderr));
    let stdout = stdout_of(&out);
    assert!(
        stdout.lines().any(|l| l.contains("the code here could not be read")),
        "a failed code read must say so plainly:\n{stdout}"
    );

    let db = sandbox.db();
    let store = EventStore::open_existing(&db).unwrap();
    assert!(
        model::store::show(&store, "project-no-commit-yet-opened-by-install").is_err(),
        "a failed code read must never leave a project-opened record behind"
    );
}

/// THE IDEMPOTENCY PROOF AT THE CLI LEVEL (the unit-level proof lives beside
/// `record_project_opened` itself in `../src/install.rs`): running the real
/// binary twice over the same repository must not duplicate the record.
#[test]
fn a_second_install_run_does_not_duplicate_the_record() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("demo-shop");
    std::fs::create_dir(&repo).unwrap();
    git_repo_with_a_file(&repo);
    let sandbox = Sandbox::new();

    let first = run_install(&repo, &sandbox, &[]);
    assert!(first.status.success(), "stderr: {}", String::from_utf8_lossy(&first.stderr));

    let db = sandbox.db();
    let events_after_first = EventStore::open_existing(&db).unwrap().get_all_events().unwrap().len();

    let second = run_install(&repo, &sandbox, &[]);
    assert!(second.status.success(), "stderr: {}", String::from_utf8_lossy(&second.stderr));
    assert!(
        !stdout_of(&second).contains("recorded this project"),
        "a rerun must not announce recording the project a second time:\n{}",
        stdout_of(&second)
    );

    let events_after_second = EventStore::open_existing(&db).unwrap().get_all_events().unwrap().len();
    assert_eq!(events_after_first, events_after_second, "a second install run appended an event to the store");
}

/// THE UPGRADE GAP ITSELF: a sidecar already sits at the right path (built by
/// an earlier install, from before `record_project_opened` existed, or lost
/// its record some other way) but the main store holds no record naming the
/// project open. This is what an owner sees after upgrading THOR and
/// re-running `install` over a repository it had already indexed: the
/// already-read branch (`Ok(Some(None))` in `../src/bin/install.rs`) used to
/// print its one line and stop, so the gap never closed on its own. Proven
/// here by running install once to build the sidecar for real, then removing
/// only the main store - the sidecar lives at an entirely different path
/// (`codeindex/<key>.db`, beside the store, never inside it), so this
/// reproduces "sidecar present, record absent" without touching any private
/// API.
#[test]
fn an_already_read_sidecar_with_no_record_gets_recorded_on_the_next_install() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("demo-shop");
    std::fs::create_dir(&repo).unwrap();
    git_repo_with_a_file(&repo);
    let sandbox = Sandbox::new();

    let first = run_install(&repo, &sandbox, &[]);
    assert!(first.status.success(), "stderr: {}", String::from_utf8_lossy(&first.stderr));
    let index_db = sandbox.local_app_data().join("thor2").join("codeindex").join("demo-shop.db");
    assert!(index_db.exists(), "the first run must have built the sidecar");

    let db = sandbox.db();
    std::fs::remove_file(&db).unwrap();
    assert!(!db.exists(), "the record's store must be gone before the second run");
    assert!(index_db.exists(), "removing the store must never touch the sidecar beside it");

    let second = run_install(&repo, &sandbox, &[]);
    assert!(second.status.success(), "stderr: {}", String::from_utf8_lossy(&second.stderr));
    let stdout = stdout_of(&second);
    assert!(
        stdout.contains("this project's code was already read"),
        "the sidecar was already there, so this run must take the already-read branch:\n{stdout}"
    );
    assert!(
        stdout.contains("recorded this project"),
        "the already-read branch must record the project too, closing the upgrade gap:\n{stdout}"
    );

    let store = EventStore::open_existing(&db).unwrap();
    let item = model::store::show(&store, "project-demo-shop-opened-by-install")
        .expect("the project-opened record must be live after the second install");
    assert_eq!(item.kind, model::item::Kind::Report);
    assert_eq!(item.project.as_deref(), Some("demo-shop"));
    assert!(
        item.text.contains("(1 file(s))"),
        "the file count must come from the real sidecar, not be invented: {}",
        item.text
    );
}
