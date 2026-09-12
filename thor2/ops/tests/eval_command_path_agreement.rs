//! Proof that `ops::install::eval_command_path_from` and `serve::usefulness::
//! eval_command_path_from` agree - required because `ops` depends on `serve`,
//! never the other way round (`ops/Cargo.toml` names `serve` as a
//! dependency; the reverse would be a cycle Cargo refuses outright), so the
//! Stop hook's own evaluation debt (`serve/src/bin/serve.rs`) cannot call
//! into `ops` to find this file and carries its own copy of the exact same
//! resolution rule instead. This is the guard against that copy silently
//! drifting from the original.
//!
//! PURE FIXTURES ONLY, no `std::env::set_var`/`remove_var`: both functions
//! read `USERPROFILE`/`HOME` only through their own I/O wrapper
//! (`default_eval_command_path`), never inside the pure rule this file
//! actually drives, so every combination below is exercised with plain
//! strings and never touches this test binary's own shared process
//! environment - see `serve/src/reentry.rs`'s own module doc comment for why
//! that matters (`std::env::set_var` is process-wide and races across
//! parallel test threads).

#[test]
fn ambient_environment_resolutions_agree() {
    // No fixture at all: whatever this test runner's own USERPROFILE/HOME
    // happen to be, the two I/O wrappers must already agree with no
    // environment manipulation.
    assert_eq!(
        ops::install::default_eval_command_path(),
        serve::usefulness::default_eval_command_path(),
        "the two resolutions must agree under the ambient environment"
    );
}

#[test]
fn userprofile_only_agrees() {
    assert_eq!(
        ops::install::eval_command_path_from(Some("C:\\Users\\fixture"), None),
        serve::usefulness::eval_command_path_from(Some("C:\\Users\\fixture"), None),
    );
}

#[test]
fn home_only_agrees() {
    assert_eq!(
        ops::install::eval_command_path_from(None, Some("/home/fixture")),
        serve::usefulness::eval_command_path_from(None, Some("/home/fixture")),
    );
}

#[test]
fn userprofile_wins_over_home_in_both() {
    let ops_path = ops::install::eval_command_path_from(Some("C:\\Users\\fixture"), Some("/home/fixture"));
    let serve_path = serve::usefulness::eval_command_path_from(Some("C:\\Users\\fixture"), Some("/home/fixture"));
    assert_eq!(ops_path, serve_path);
    assert_eq!(ops_path.unwrap(), std::path::PathBuf::from("C:\\Users\\fixture").join(".claude").join("commands").join("thor-eval.md"));
}

#[test]
fn neither_set_agrees_on_none() {
    assert_eq!(ops::install::eval_command_path_from(None, None), None);
    assert_eq!(serve::usefulness::eval_command_path_from(None, None), None);
}
