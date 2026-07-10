use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The global Claude Code settings.json (where mimir also installs its hooks).
pub fn default_settings_path() -> PathBuf {
    if let Ok(up) = std::env::var("USERPROFILE") {
        return Path::new(&up).join(".claude").join("settings.json");
    }
    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home).join(".claude").join("settings.json");
    }
    PathBuf::from(".claude/settings.json")
}

/// Wire THOR's hooks into a Claude Code settings.json the same way mimir does:
/// one command, no hand-editing. Safe by construction - it refuses to touch a
/// settings.json that is not valid JSON, backs the file up first, only ADDS
/// THOR's own hook entries (never removes or rewrites mimir's or anyone else's),
/// is idempotent (re-running adds nothing), and always writes valid JSON.
///
/// Default: only the Stop response guard (universally correct - "you have the
/// tools, do it yourself, don't ask the user"). `with_guard` also installs the
/// PreToolUse command guard (opt-in: its rulebook is project-specific, so global
/// install would give wrong deploy advice in unrelated projects). `with_courier`
/// also installs the UserPromptSubmit recall courier (runs alongside mimir).
pub fn run_install(
    settings: &Path,
    with_guard: bool,
    with_courier: bool,
    with_daemon: bool,
    backup_repo: Option<&Path>,
) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().replace('\\', "\\\\");
    let cmd = |sub: &str| format!("\"{}\" {}", exe.replace("\\\\", "\\"), sub);

    // Read existing settings (or start fresh). Refuse to proceed on invalid JSON
    // so we can never clobber a file we do not understand.
    let existing = std::fs::read_to_string(settings).unwrap_or_default();
    let mut root: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&existing).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); refusing to touch it. Fix or remove it, then re-run.",
                settings.display()
            )
        })?
    };
    if !root.is_object() {
        anyhow::bail!("{} root is not a JSON object; refusing to touch it.", settings.display());
    }

    // Backup BEFORE any write.
    if settings.exists() {
        let bak = settings.with_extension("json.thor-bak");
        std::fs::copy(settings, &bak)?;
    } else if let Some(dir) = settings.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let obj = root.as_object_mut().unwrap();
    let hooks = obj.entry("hooks").or_insert(json!({}));
    if !hooks.is_object() {
        anyhow::bail!("\"hooks\" in settings.json is not an object; refusing to touch it.");
    }
    let hooks = hooks.as_object_mut().unwrap();

    // Add one hook group to an event array unless an identical command is already
    // present (idempotent). Returns true if it added.
    fn add(hooks: &mut serde_json::Map<String, Value>, event: &str, group: Value, command: &str) -> bool {
        let arr = hooks.entry(event.to_string()).or_insert(json!([]));
        let arr = match arr.as_array_mut() {
            Some(a) => a,
            None => return false, // someone put a non-array here; leave it alone
        };
        let present = arr.iter().any(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .map(|hs| hs.iter().any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command)))
                .unwrap_or(false)
        });
        if present {
            return false;
        }
        arr.push(group);
        true
    }

    let mut added: Vec<&str> = Vec::new();

    let stop_cmd = cmd("stop-guard");
    if add(
        hooks,
        "Stop",
        json!({ "hooks": [ { "type": "command", "command": stop_cmd } ] }),
        &stop_cmd,
    ) {
        added.push("Stop (response guard)");
    }

    if with_guard {
        let guard_cmd = cmd("guard");
        if add(
            hooks,
            "PreToolUse",
            json!({ "matcher": "*", "hooks": [ { "type": "command", "command": guard_cmd } ] }),
            &guard_cmd,
        ) {
            added.push("PreToolUse (command guard)");
        }
    }

    if with_courier {
        let courier_cmd = cmd("courier");
        if add(
            hooks,
            "UserPromptSubmit",
            json!({ "hooks": [ { "type": "command", "command": courier_cmd } ] }),
            &courier_cmd,
        ) {
            added.push("UserPromptSubmit (recall courier)");
        }
        // The courier pairs with two SessionStart hooks: `warm` pre-warms the
        // semantic embedder so the first prompt is fast, and `session-start`
        // refreshes a known project's index in the background (or offers to set up
        // a new one). Both are no-ops when not applicable, so they are safe to add.
        let warm_cmd = cmd("warm");
        if add(
            hooks,
            "SessionStart",
            json!({ "hooks": [ { "type": "command", "command": warm_cmd } ] }),
            &warm_cmd,
        ) {
            added.push("SessionStart (pre-warm embedder)");
        }
        let session_cmd = cmd("session-start");
        if add(
            hooks,
            "SessionStart",
            json!({ "hooks": [ { "type": "command", "command": session_cmd } ] }),
            &session_cmd,
        ) {
            added.push("SessionStart (project refresh + onboarding cue)");
        }
    }

    if with_daemon {
        // Opt-in: ensure the warm injection daemon at session start. Debounced
        // and detached inside ensure-daemon; without the daemon the courier
        // falls back to its cold path unchanged.
        let daemon_cmd = cmd("ensure-daemon");
        if add(
            hooks,
            "SessionStart",
            json!({ "hooks": [ { "type": "command", "command": daemon_cmd } ] }),
            &daemon_cmd,
        ) {
            added.push("SessionStart (warm injection daemon ensure-start)");
        }
    }

    if let Some(repo) = backup_repo {
        let backup_cmd = cmd(&format!("backup --repo \"{}\"", repo.display()));
        if add(
            hooks,
            "SessionStart",
            json!({ "hooks": [ { "type": "command", "command": backup_cmd } ] }),
            &backup_cmd,
        ) {
            added.push("SessionStart (daily GitHub backup, debounced 20h)");
        }
    }

    let out = serde_json::to_string_pretty(&root)? + "\n";
    std::fs::write(settings, out)?;

    println!("THOR hooks installed into {}", settings.display());
    if added.is_empty() {
        println!("  (nothing to add - THOR hooks were already present)");
    } else {
        for a in &added {
            println!("  + {}", a);
        }
        println!("  backup: {}", settings.with_extension("json.thor-bak").display());
    }
    println!("Existing hooks (e.g. mimir's) were left untouched.");
    println!("Restart Claude Code for the hooks to take effect.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};
    static TEST_CTR: AtomicU32 = AtomicU32::new(0);

    fn unique_dir(tag: &str) -> PathBuf {
        // unique per call so parallel tests never share (and delete) a dir
        let n = TEST_CTR.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("thor-{}-{}-{}", tag, std::process::id(), n))
    }

    fn install_into(json_in: &str, with_guard: bool, with_courier: bool, with_daemon: bool) -> Value {
        let dir = unique_dir("install");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, json_in).unwrap();
        run_install(&p, with_guard, with_courier, with_daemon, None).unwrap();
        let out: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    fn stop_cmds(v: &Value) -> Vec<String> {
        v["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            .filter_map(|h| h["command"].as_str().map(String::from))
            .collect()
    }

    #[test]
    fn test_preserves_existing_hooks_and_adds_thor() {
        // a settings.json that already has a mimir Stop hook
        let input = r#"{
            "permissions": {"allow": ["mcp__mimir__recall"]},
            "hooks": {
                "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": "mimir-recall.ps1" } ] } ],
                "Stop": [ { "hooks": [ { "type": "command", "command": "mimir-checkpoint.ps1" } ] } ]
            }
        }"#;
        let out = install_into(input, true, false, false);
        // mimir's Stop hook survives
        let stops = stop_cmds(&out);
        assert!(stops.iter().any(|c| c.contains("mimir-checkpoint")), "mimir Stop hook preserved");
        // THOR's stop-guard was added alongside
        assert!(stops.iter().any(|c| c.contains("stop-guard")), "thor stop-guard added");
        // permissions + the mimir UserPromptSubmit are untouched
        assert_eq!(out["permissions"]["allow"][0], "mcp__mimir__recall");
        assert_eq!(out["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"], "mimir-recall.ps1");
        // PreToolUse guard added (with_guard=true)
        assert!(out["hooks"]["PreToolUse"].is_array());
        // courier NOT added (with_courier=false) -> mimir UserPromptSubmit stays length 1
        assert!(out["hooks"].get("UserPromptSubmit").unwrap().as_array().unwrap().len() == 1);
    }

    #[test]
    fn test_idempotent_no_duplicates() {
        let input = r#"{"hooks":{}}"#;
        let once = install_into(input, true, true, true);
        // re-run on the once-installed output
        let dir = unique_dir("idem");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, serde_json::to_string(&once).unwrap()).unwrap();
        run_install(&p, true, true, true, None).unwrap();
        let twice: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        // exactly one thor stop-guard, not two
        let n = twice["hooks"]["Stop"].as_array().unwrap().len();
        assert_eq!(n, 1, "re-running must not duplicate the Stop hook");
        // with_courier also wires SessionStart warm + session-start, once each (idempotent)
        let ss: Vec<String> = twice["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            .filter_map(|h| h["command"].as_str().map(String::from))
            .collect();
        assert_eq!(ss.iter().filter(|c| c.ends_with("warm")).count(), 1, "one warm hook");
        assert_eq!(ss.iter().filter(|c| c.contains("session-start")).count(), 1, "one session-start hook");
        assert_eq!(ss.iter().filter(|c| c.contains("ensure-daemon")).count(), 1, "one ensure-daemon hook");
    }

    #[test]
    fn test_starts_from_empty_settings() {
        let out = install_into("", true, false, false);
        assert!(stop_cmds(&out).iter().any(|c| c.contains("stop-guard")));
        assert!(out["hooks"]["PreToolUse"].is_array());
    }

    #[test]
    fn test_default_installs_only_the_response_guard() {
        // no flags: Stop response guard only, no PreToolUse, no courier changes
        let out = install_into(r#"{}"#, false, false, false);
        assert!(stop_cmds(&out).iter().any(|c| c.contains("stop-guard")), "response guard installed");
        assert!(out["hooks"].get("PreToolUse").is_none(), "command guard NOT installed by default");
        assert!(out["hooks"].get("UserPromptSubmit").is_none(), "courier NOT installed by default");
        assert!(out["hooks"].get("SessionStart").is_none(), "daemon hook NOT installed by default");
    }
}
