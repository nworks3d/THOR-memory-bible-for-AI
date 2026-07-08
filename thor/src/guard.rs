use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

/// One Guard rule: a moment-of-action reminder bound to a tool-call pattern.
/// Matching is dep-free substring logic (case-insensitive): the rule fires when
/// the tool matches AND every `all_of` substring is present AND (`any_of` is
/// empty OR at least one is present). This is the command-pattern half of Guard
/// M4 - the response-pattern (ssh-amnesia) class needs the Stop-hook spike and
/// is deliberately NOT handled here.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    /// Tool names to match (e.g. ["Bash","PowerShell"]); empty = any tool.
    pub tools: Vec<String>,
    pub all_of: Vec<String>,
    pub any_of: Vec<String>,
    /// If ANY of these substrings is present the rule does NOT fire - used to
    /// let a safe twin (e.g. a `grep -q` presence check) through where the
    /// dangerous form (printing the value) fires.
    pub none_of: Vec<String>,
    pub reminder: String,
}

/// Default rulebook next to the store, so the hook command needs no path.
pub fn default_rulebook_path() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return Path::new(&local).join("thor").join("guard-rulebook.json");
    }
    // Never fall back to a CWD-relative name: a project directory could plant a
    // guard-rulebook.json and inject reminders. An empty path fails to read ->
    // the guard stays silent (no rulebook), which is the safe default.
    PathBuf::new()
}

fn parse_rules(text: &str) -> Vec<Rule> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let arr = match value.as_array() {
        Some(a) => a,
        None => return vec![],
    };
    let strs = |v: &Value, key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    };
    arr.iter()
        .filter_map(|r| {
            let reminder = r.get("reminder").and_then(|v| v.as_str())?.to_string();
            // accept a "tools" array, or a single "tool" string, or neither (any).
            let mut tools = strs(r, "tools");
            if tools.is_empty() {
                if let Some(t) = r.get("tool").and_then(|v| v.as_str()) {
                    tools.push(t.to_string());
                }
            }
            Some(Rule {
                id: r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                tools,
                all_of: strs(r, "all_of"),
                any_of: strs(r, "any_of"),
                none_of: strs(r, "none_of"),
                reminder,
            })
        })
        .collect()
}

/// Flatten a tool_input JSON object into one lowercase haystack: the command,
/// file_path, content, and any other string values are all searchable, so a
/// pattern matches whether it appears in a Bash command or an Edit's path/body.
pub fn tool_input_text(input: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    walk(input, &mut parts);
    // Collapse runs of intra-line whitespace to a single space so multi-word
    // tokens ("docker cp", "git commit") match regardless of extra spaces a
    // shell treats identically; keep newlines between fields so a token cannot
    // span two unrelated fields.
    parts
        .join("\n")
        .to_lowercase()
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pure matcher: which rules fire for this (tool, haystack). Case-insensitive.
pub fn evaluate(rules: &[Rule], tool: &str, haystack_lower: &str) -> Vec<String> {
    let tool_l = tool.to_lowercase();
    let mut fired = Vec::new();
    for rule in rules {
        if !rule.tools.is_empty() && !rule.tools.iter().any(|t| t.to_lowercase() == tool_l) {
            continue;
        }
        let all_ok = rule
            .all_of
            .iter()
            .all(|s| haystack_lower.contains(&s.to_lowercase()));
        if !all_ok {
            continue;
        }
        let any_ok = rule.any_of.is_empty()
            || rule
                .any_of
                .iter()
                .any(|s| haystack_lower.contains(&s.to_lowercase()));
        if !any_ok {
            continue;
        }
        let blocked = rule
            .none_of
            .iter()
            .any(|s| haystack_lower.contains(&s.to_lowercase()));
        if blocked {
            continue;
        }
        fired.push(rule.reminder.clone());
    }
    fired
}

/// Run as a PreToolUse guard: read the hook JSON on stdin, and if any rule
/// fires, emit a hookSpecificOutput with additionalContext (advisory-only,
/// permissionDecision "allow" so the tool proceeds). HARD fail-open: any error
/// prints nothing and exits 0 - the guard must never block a tool call.
pub fn run_guard(rulebook: &Path) {
    let _ = try_guard(rulebook);
}

fn try_guard(rulebook: &Path) -> anyhow::Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let raw = raw.trim_start_matches('\u{feff}');
    if raw.trim().is_empty() {
        return Ok(());
    }
    let hook: Value = serde_json::from_str(raw)?;
    let tool = hook.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    let input = hook.get("tool_input").cloned().unwrap_or(Value::Null);
    let haystack = tool_input_text(&input);

    let rules = match std::fs::read_to_string(rulebook) {
        Ok(text) => parse_rules(&text),
        Err(_) => return Ok(()), // no rulebook -> silent, never block
    };
    let fired = evaluate(&rules, tool, &haystack);
    if fired.is_empty() {
        return Ok(());
    }

    let context = format!("[THOR guard] {}", fired.join("  ||  "));
    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "additionalContext": context
        }
    });
    // Fallible write: a broken stdout pipe must never panic the guard.
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{}", out);
    let _ = stdout.flush();
    Ok(())
}

/// Response-rulebook for the Stop guard (the capability-amnesia / ssh-amnesia
/// class), separate from the PreToolUse command rulebook.
pub fn default_response_rulebook_path() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return Path::new(&local).join("thor").join("guard-response-rulebook.json");
    }
    PathBuf::new()
}

/// Run as a Stop guard: read the Stop-hook JSON on stdin and, if the assistant's
/// final message matches a response rule (e.g. it asked the user to do something
/// it could do itself), emit `{"decision":"block","reason":...}` so the model
/// reconsiders BEFORE it yields. Unlike the advisory PreToolUse guard this DOES
/// block the stop - that is the point for this class. HARD fail-open: any error
/// (or the loop-guard) prints nothing and exits 0, so a stop is never wrongly held.
pub fn run_stop_guard(rulebook: &Path) {
    let _ = try_stop_guard(rulebook);
}

fn try_stop_guard(rulebook: &Path) -> anyhow::Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let raw = raw.trim_start_matches('\u{feff}');
    if raw.trim().is_empty() {
        return Ok(());
    }
    let hook: Value = serde_json::from_str(raw)?;
    // Loop-prevention: if a Stop guard already fired this turn, never re-block
    // (else a still-imperfect revised answer could loop forever). The exact
    // field is under-documented; treat any truthy `stop_hook_active` as "stop".
    if hook.get("stop_hook_active").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(());
    }
    let msg = hook
        .get("last_assistant_message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if msg.trim().is_empty() {
        return Ok(());
    }
    // Reuse the command matcher over the assistant's message as the haystack.
    let haystack = tool_input_text(&Value::String(msg.to_string()));
    let rules = match std::fs::read_to_string(rulebook) {
        Ok(text) => parse_rules(&text),
        Err(_) => return Ok(()), // no rulebook -> never block
    };
    let fired = evaluate(&rules, "response", &haystack);
    if fired.is_empty() {
        return Ok(());
    }
    let reason = format!("[THOR] {}", fired.join("  ||  "));
    let out = serde_json::json!({ "decision": "block", "reason": reason });
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{}", out);
    let _ = stdout.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Synthetic fixtures only: no real host, container, or secret names live in
    // committed code (the real rulebook holding those is gitignored). "app-prod"
    // / "app-dev" / "MY_SECRET" stand in for the machine/business internals.
    fn seed_rules() -> Vec<Rule> {
        parse_rules(
            r#"[
              {"id":"force-recreate","tool":"Bash","all_of":["--force-recreate"],"reminder":"no force-recreate on prod"},
              {"id":"docker-cp-prod","tool":"Bash","all_of":["docker cp"],"none_of":["-dev:"],"reminder":"docker cp is not a deploy"},
              {"id":"secret-echo","tool":"Bash","all_of":["echo"],"any_of":["my_secret","app_key"],"reminder":"do not print secrets"}
            ]"#,
        )
    }

    #[test]
    fn test_fires_on_dangerous_and_silent_on_safe() {
        let rules = seed_rules();
        // dangerous: force-recreate on prod
        let hay = tool_input_text(&json!({"command": "docker compose -p app up -d --force-recreate"}));
        assert_eq!(evaluate(&rules, "Bash", &hay), vec!["no force-recreate on prod"]);
        // safe twin: plain up, no force
        let hay2 = tool_input_text(&json!({"command": "docker compose -p app up -d"}));
        assert!(evaluate(&rules, "Bash", &hay2).is_empty());
    }

    #[test]
    fn test_docker_cp_needs_prod_context() {
        let rules = seed_rules();
        // fires on ANY docker cp except one whose container ref is a dev container
        let prod = tool_input_text(&json!({"command": "docker cp fix.js app-prod:/app/"}));
        assert_eq!(evaluate(&rules, "Bash", &prod).len(), 1, "docker cp to prod fires");
        // dev container ref ("...-dev:") is excluded by the colon-anchored none_of
        let dev = tool_input_text(&json!({"command": "docker cp fix.js app-dev:/app/"}));
        assert!(evaluate(&rules, "Bash", &dev).is_empty(), "docker cp to a dev container is safe");
    }

    #[test]
    fn test_docker_cp_dev_exclusion_is_colon_anchored() {
        // regression: the dev exclusion is anchored to the container-ref colon
        // ("-dev:"), NOT the bare substring "dev" - so a prod cp whose SOURCE path
        // contains "dev" (./dev/, /dev/null) OR a file named with "-dev" must STILL
        // fire; only a real "...-dev:" container target is suppressed.
        let rules = seed_rules();
        let src_path = tool_input_text(&json!({"command": "docker cp ./dev/fix.js app-prod:/app/server.js"}));
        assert_eq!(evaluate(&rules, "Bash", &src_path).len(), 1, "prod fires despite ./dev/ in source path");
        let dev_in_filename = tool_input_text(&json!({"command": "docker cp fix-dev.js app-prod:/app/server.js"}));
        assert_eq!(evaluate(&rules, "Bash", &dev_in_filename).len(), 1, "prod fires despite '-dev' in the file name");
    }

    #[test]
    fn test_multiword_token_survives_extra_whitespace() {
        // regression: a shell treats "docker  cp" (two spaces) as "docker cp";
        // the matcher must too (haystack whitespace is collapsed).
        let rules = seed_rules();
        let hay = tool_input_text(&json!({"command": "docker  cp   fix.js app-prod:/app/"}));
        assert_eq!(evaluate(&rules, "Bash", &hay).len(), 1, "collapsed whitespace still matches 'docker cp'");
    }

    #[test]
    fn test_secret_echo_and_tool_scoping() {
        let rules = seed_rules();
        let hay = tool_input_text(&json!({"command": "echo $MY_SECRET"}));
        assert_eq!(evaluate(&rules, "Bash", &hay), vec!["do not print secrets"]);
        // same text under a non-Bash tool must not fire the Bash-scoped rule
        assert!(evaluate(&rules, "Edit", &hay).is_empty());
    }

    #[test]
    fn test_none_of_lets_the_safe_twin_through() {
        // one rule that fires on a secret name but NOT on a presence-check
        let rules = parse_rules(
            r#"[{"id":"secret","any_of":["my_secret"],"none_of":["grep -q","[ -n"],"reminder":"no secret print"}]"#,
        );
        // dangerous: prints the value
        let danger = tool_input_text(&json!({"command": "docker exec c env | grep MY_SECRET"}));
        assert_eq!(evaluate(&rules, "Bash", &danger).len(), 1);
        // safe twin: presence check, blocked by none_of
        let safe = tool_input_text(&json!({"command": "[ -n \"$MY_SECRET\" ] && echo set"}));
        assert!(evaluate(&rules, "Bash", &safe).is_empty(), "presence-check must not fire");
        let safe2 = tool_input_text(&json!({"command": "grep -q MY_SECRET .env"}));
        assert!(evaluate(&rules, "Bash", &safe2).is_empty());
    }

    #[test]
    fn test_tools_list_matches_multiple() {
        let rules = parse_rules(
            r#"[{"id":"copy","tools":["Bash","PowerShell"],"any_of":["web/site"],"reminder":"x"}]"#,
        );
        let hay = tool_input_text(&json!({"command": "robocopy site //fileserver/web/site"}));
        assert_eq!(evaluate(&rules, "PowerShell", &hay).len(), 1);
        assert_eq!(evaluate(&rules, "Bash", &hay).len(), 1);
        assert!(evaluate(&rules, "Edit", &hay).is_empty());
    }

    #[test]
    fn test_case_insensitive_and_no_rules() {
        let rules = seed_rules();
        let hay = tool_input_text(&json!({"command": "DOCKER COMPOSE UP --FORCE-RECREATE"}));
        assert_eq!(evaluate(&rules, "bash", &hay).len(), 1);
        assert!(evaluate(&[], "Bash", &hay).is_empty(), "no rules -> no fire");
    }

    #[test]
    fn test_response_guard_matches_capability_amnesia() {
        // the Stop-guard reuses the matcher over the assistant's last message.
        // Real drift (the maintainer 2026-07-08): asking which branch to push instead of
        // doing the commit+push itself.
        let rules = parse_rules(
            r#"[{"id":"ask-branch","any_of":["welke branch","which branch"],"reminder":"push it yourself"}]"#,
        );
        let drift = tool_input_text(&json!("Committen + naar welke branch pushen?"));
        assert_eq!(evaluate(&rules, "response", &drift).len(), 1, "asking which branch fires");
        // a legitimate status report must NOT fire
        let ok = tool_input_text(&json!("Done - committed and pushed to main, deploy triggered."));
        assert!(evaluate(&rules, "response", &ok).is_empty(), "a status report must not block");
    }
}
