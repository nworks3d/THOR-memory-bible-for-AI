//! Installs THOR's hooks into an agent's `settings.json`: SessionStart,
//! PreToolUse and UserPromptSubmit, each pointing at the same `serve hook`
//! command (see `serve/src/bin/serve.rs`'s `cmd_hook`, which branches on the
//! payload's own `hook_event_name`).
//!
//! Non-negotiables (all enforced structurally, not by convention - CONTRACT
//! R1/R7): a back-up is written before anything else touches the file; the
//! file must already be valid JSON or the whole run refuses (never silently
//! starts a fresh settings file over a file the agent could not parse); a
//! second run adds nothing that is already there; and any hook this tool did
//! not put there is never touched, moved, or removed - only appended past.

use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// One hook this installer knows how to place: which event fires it, an
/// optional matcher (Claude Code's PreToolUse groups carry one; SessionStart
/// and UserPromptSubmit do not), and the exact command line to run.
#[derive(Debug, Clone)]
pub struct HookSpec {
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub command: String,
}

/// The three hooks the CONTRACT asks for: session start, before a tool call,
/// and per prompt - all three calling the same `serve hook --db <db>` command,
/// exactly like `serve/src/bin/serve.rs`'s own `hook` subcommand expects
/// (it tells the events apart by the JSON payload's `hook_event_name`, not by
/// which command line ran it).
///
/// The `--db` goes BEFORE the subcommand, because that is where `serve`'s CLI
/// puts it (`serve --db <DB> hook`, a global option on the parser, not on the
/// subcommand). This file had it the other way round until 2026-08-03 and was
/// caught the first time anyone actually RAN the command it writes: every hook
/// would have exited with "unexpected argument '--db'". Hooks fail open by
/// design, so nothing would have complained - the memory would simply never
/// have spoken again, which is this project's own worst failure class.
pub fn standard_hooks(serve_exe: &str, db: &str) -> Vec<HookSpec> {
    let command = format!("\"{serve_exe}\" --db \"{db}\" hook");
    vec![
        HookSpec { event: "SessionStart", matcher: None, command: command.clone() },
        HookSpec { event: "PreToolUse", matcher: Some("*"), command: command.clone() },
        HookSpec { event: "UserPromptSubmit", matcher: None, command: command.clone() },
        // Surface 5, the Response Guard. The `hook` command branches on the
        // payload's own `hook_event_name`, so the Stop hook runs the SAME
        // command - it is the payload, not the command line, that tells them
        // apart. Leaving this one out is exactly the regression that let a
        // whole session of untidy replies through (see serve::respond).
        HookSpec { event: "Stop", matcher: None, command },
    ]
}

/// What happened to one hook on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    Added,
    AlreadyPresent,
}

#[derive(Debug, Clone)]
pub struct InstallReport {
    pub results: Vec<(String, HookOutcome)>,
    pub backup_path: Option<std::path::PathBuf>,
}

/// True iff `group` (one element of `hooks.<event>`) already contains a
/// `{"type":"command","command": command}` entry, regardless of its matcher.
/// Matching on the command line is deliberate: it is the one field that
/// identifies "this is THOR's hook" without also depending on how a matcher
/// happened to be phrased.
fn group_has_command(group: &Value, command: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("type").and_then(Value::as_str) == Some("command")
                    && h.get("command").and_then(Value::as_str) == Some(command)
            })
        })
        .unwrap_or(false)
}

fn new_group(spec: &HookSpec) -> Value {
    let mut group = json!({
        "hooks": [ { "type": "command", "command": spec.command } ]
    });
    if let Some(matcher) = spec.matcher {
        group["matcher"] = json!(matcher);
    }
    group
}

/// Install `specs` into the settings JSON at `path`.
///
/// - Missing file: starts from `{}` (a fresh install has nothing to preserve).
/// - Existing file that is not valid JSON, or not a JSON object, or whose
///   `hooks` key (or one `hooks.<event>` key) is not the shape this tool
///   expects: refused with a reason. Nothing is written.
/// - Otherwise: the file is backed up to `<path>.bak` first (copied verbatim,
///   before any parse result is acted on), then for each spec, its event's
///   array gets exactly one new group appended IF no existing group in that
///   array already carries the same command - every other group (anyone
///   else's hook, or THOR's own from an earlier install) is left byte-for-
///   byte as it was.
pub fn install_hooks(path: &Path, specs: &[HookSpec]) -> anyhow::Result<InstallReport> {
    let existed = path.exists();
    let raw = if existed { fs::read_to_string(path)? } else { "{}".to_string() };

    let mut root: Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid JSON ({e}) - refusing to touch it; fix the JSON first, or point --settings at a different file",
            path.display()
        )
    })?;
    anyhow::ensure!(
        root.is_object(),
        "{} does not contain a JSON object at the top level - refusing to touch it",
        path.display()
    );

    if root.get("hooks").is_some() {
        anyhow::ensure!(
            root["hooks"].is_object(),
            "{}'s \"hooks\" key is not a JSON object - refusing to touch a shape this tool does not recognise",
            path.display()
        );
    } else {
        root["hooks"] = json!({});
    }

    for spec in specs {
        if let Some(existing) = root["hooks"].get(spec.event) {
            anyhow::ensure!(
                existing.is_array(),
                "{}'s \"hooks.{}\" key is not a JSON array - refusing to touch a shape this tool does not recognise",
                path.display(),
                spec.event
            );
        }
    }

    let backup_path = if existed {
        let backup = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => format!("{ext}.bak"),
            None => "bak".to_string(),
        });
        fs::write(&backup, &raw)?;
        Some(backup)
    } else {
        None
    };

    let mut results = Vec::new();
    for spec in specs {
        let array = root["hooks"]
            .as_object_mut()
            .unwrap()
            .entry(spec.event)
            .or_insert_with(|| json!([]));
        let array = array.as_array_mut().expect("checked above: this key is an array");

        let already = array.iter().any(|group| group_has_command(group, &spec.command));
        if already {
            results.push((spec.event.to_string(), HookOutcome::AlreadyPresent));
        } else {
            array.push(new_group(spec));
            results.push((spec.event.to_string(), HookOutcome::Added));
        }
    }

    let pretty = serde_json::to_string_pretty(&root)?;
    fs::write(path, pretty + "\n")?;

    Ok(InstallReport { results, backup_path })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> Vec<HookSpec> {
        standard_hooks("C:\\fake\\serve.exe", "C:\\fake\\thor.db")
    }

    /// The defect this guards against, and it is the worst kind this project
    /// has: a hook command that is written correctly-looking but does not
    /// parse. `serve` takes `--db` as a GLOBAL option, before the subcommand.
    /// With it after, every hook exits 1 with "unexpected argument '--db'" -
    /// and hooks fail open, so nothing complains and the memory simply stops
    /// speaking. Caught only by running the command this function writes.
    #[test]
    fn the_command_puts_db_before_the_subcommand() {
        let specs = standard_hooks("C:\\thor2\\bin\\serve.exe", "C:\\thor2\\thor.db");
        for spec in &specs {
            assert_eq!(
                spec.command, "\"C:\\thor2\\bin\\serve.exe\" --db \"C:\\thor2\\thor.db\" hook",
                "the {} hook must call serve the way serve's own CLI parses it",
                spec.event
            );
            let db_at = spec.command.find("--db").expect("the command must name --db");
            let sub_at = spec.command.find(" hook").expect("the command must call the hook subcommand");
            assert!(db_at < sub_at, "--db is a global option, so it comes first: {}", spec.command);
        }
    }

    #[test]
    fn a_fresh_settings_file_gets_all_four_hooks_added() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let report = install_hooks(&path, &specs()).unwrap();
        assert_eq!(report.results.len(), 4);
        assert!(report.results.iter().all(|(_, o)| *o == HookOutcome::Added));
        assert!(report.backup_path.is_none(), "nothing existed yet, so there is nothing to back up");

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["UserPromptSubmit"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["PreToolUse"][0]["matcher"], json!("*"));
        assert!(written["hooks"]["SessionStart"][0].get("matcher").is_none());
        // Surface 5: the Response Guard on the Stop hook - the one that was
        // missing and let a whole session of untidy replies through.
        assert_eq!(written["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    /// The defect this guards against: a naive installer appends its group
    /// unconditionally, so running `install` twice (a re-run, a second agent
    /// setup pass) leaves TWO identical hook entries firing the same command
    /// on every session start.
    #[test]
    fn a_second_install_adds_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_hooks(&path, &specs()).unwrap();
        let second = install_hooks(&path, &specs()).unwrap();

        assert!(
            second.results.iter().all(|(_, o)| *o == HookOutcome::AlreadyPresent),
            "the second run must recognise every hook as already installed: {:?}",
            second.results
        );
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for event in ["SessionStart", "PreToolUse", "UserPromptSubmit", "Stop"] {
            assert_eq!(
                written["hooks"][event].as_array().unwrap().len(),
                1,
                "event {event} must still carry exactly one group after a second install"
            );
        }
    }

    /// The other defect this guards against: an installer that "cleans up"
    /// or rewrites the hooks array wholesale would silently delete another
    /// tool's hook (or an earlier hand-written one) the moment it runs.
    #[test]
    fn install_leaves_a_foreign_hook_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": [
                        { "hooks": [ { "type": "command", "command": "\"C:\\\\some\\\\other-tool.exe\" backup" } ] }
                    ]
                },
                "permissions": { "allow": ["Bash(ls:*)"] }
            }))
            .unwrap(),
        )
        .unwrap();

        let report = install_hooks(&path, &specs()).unwrap();
        assert_eq!(
            report.results.iter().find(|(e, _)| e == "SessionStart").unwrap().1,
            HookOutcome::Added,
            "our own SessionStart hook must still be added alongside the foreign one"
        );

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = written["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 2, "the foreign hook must survive, plus our own");
        assert!(
            session_start.iter().any(|g| group_has_command(g, "\"C:\\\\some\\\\other-tool.exe\" backup")),
            "the pre-existing foreign hook must be byte-identical, not merged or rewritten"
        );
        // Unrelated top-level keys must survive completely untouched too.
        assert_eq!(written["permissions"]["allow"][0], json!("Bash(ls:*)"));
    }

    /// Refusal test: invalid JSON must never be silently replaced with a
    /// fresh `{}` (that would look like a successful install while quietly
    /// discarding every setting the file used to hold).
    #[test]
    fn install_refuses_invalid_json_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = "{ this is not valid json ,,, ";
        fs::write(&path, original).unwrap();

        let err = install_hooks(&path, &specs());
        assert!(err.is_err(), "invalid JSON must be refused, not repaired or replaced");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "a refused install must leave the file exactly as it was"
        );
    }

    #[test]
    fn install_refuses_a_non_object_hooks_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, serde_json::to_string(&json!({ "hooks": "not-an-object" })).unwrap()).unwrap();
        let err = install_hooks(&path, &specs());
        assert!(err.is_err(), "an unrecognised \"hooks\" shape must be refused, not overwritten");
    }

    #[test]
    fn install_backs_up_the_original_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = serde_json::to_string_pretty(&json!({ "hooks": {}, "marker": "original" })).unwrap();
        fs::write(&path, &original).unwrap();

        let report = install_hooks(&path, &specs()).unwrap();
        let backup = report.backup_path.expect("an existing file must be backed up");
        assert_eq!(fs::read_to_string(&backup).unwrap(), original, "the backup must hold the pre-install content");
    }
}
