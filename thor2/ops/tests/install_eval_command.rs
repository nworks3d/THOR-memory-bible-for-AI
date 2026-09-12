//! CLI-level proof that `install` writes the end-of-session evaluation
//! routine, and specifically that it does so even under `--no-mcp` - the one
//! part of `ops::install::seed_eval_command`'s contract that only shows up
//! at the binary level, since the library function itself takes no `no_mcp`
//! parameter at all and is simply called unconditionally. See
//! `seed_eval_command`'s own doc comment for why a read-only memory still
//! gets the file: so it is already there the day writes are turned on.
//!
//! Sandbox pattern copied from `install_project_index.rs`: HOME, USERPROFILE
//! and LOCALAPPDATA all point at one throwaway directory, so this can never
//! touch a real `~/.claude` on the machine actually running the suite.

use std::path::PathBuf;
use std::process::{Command, Output};

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

    fn eval_command_path(&self) -> PathBuf {
        self.home.path().join(".claude").join("commands").join("thor-eval.md")
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path());
        cmd.env("USERPROFILE", self.home.path());
        cmd.env("LOCALAPPDATA", self.local_app_data());
        // Belt and braces beyond HOME/USERPROFILE, same as
        // `install_project_index.rs`'s own sandbox: keeps the installer's one
        // `--global` git write inside this throwaway directory too.
        cmd.env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"));
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    }
}

fn run_install(repo: &std::path::Path, sandbox: &Sandbox, extra_args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
    cmd.current_dir(repo).args(extra_args);
    sandbox.apply(&mut cmd);
    cmd.output().unwrap()
}

/// THE DEFECT THIS GUARDS AGAINST: a version of this feature gated behind
/// `if !cli.no_mcp` would write nothing at all for the one flag combination
/// this test uses, and a read-only memory - the exact setup where a debt can
/// pile up silently until someone remembers to reinstall - would never get
/// the routine that settles it.
#[test]
fn no_mcp_still_writes_the_eval_command_with_both_placeholders_substituted() {
    let repo = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::new();

    let out = run_install(repo.path(), &sandbox, &["--no-mcp"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("wrote the evaluation routine to"),
        "a fresh install must report writing the eval command even under --no-mcp:\n{stdout}"
    );

    let path = sandbox.eval_command_path();
    assert!(path.exists(), "the eval command must be written under --no-mcp, at {}", path.display());

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("{{"), "no placeholder may survive substitution: {written}");

    let expected_db = sandbox.local_app_data().join("thor2").join("thor.db");
    assert!(
        written.contains(&expected_db.display().to_string()),
        "the store path must be substituted in: {written}"
    );
    let expected_bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_install")).parent().unwrap().display().to_string();
    assert!(
        written.contains(&expected_bin_dir),
        "the programs folder must be substituted in: {written}"
    );
}

/// A second run, still under `--no-mcp`, must recognise the file as already
/// there and leave it alone - the same "never overwrite an edited copy"
/// contract `seed_eval_command`'s own unit tests prove directly, checked
/// here again through the real binary.
#[test]
fn a_second_no_mcp_install_leaves_the_eval_command_untouched() {
    let repo = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::new();

    let first = run_install(repo.path(), &sandbox, &["--no-mcp"]);
    assert!(first.status.success(), "stderr: {}", String::from_utf8_lossy(&first.stderr));
    let path = sandbox.eval_command_path();
    let after_first = std::fs::read(&path).unwrap();

    let second = run_install(repo.path(), &sandbox, &["--no-mcp"]);
    assert!(second.status.success(), "stderr: {}", String::from_utf8_lossy(&second.stderr));
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("eval command already at"),
        "a second install must report the eval command as already there:\n{stdout}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        after_first,
        "a second install must not rewrite a file that is already there"
    );
}
