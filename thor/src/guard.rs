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
/// Never falls back to a CWD-relative name: a project directory could plant a
/// guard-rulebook.json and inject reminders. An empty path fails to read ->
/// the guard stays silent (no rulebook), which is the safe default.
pub fn default_rulebook_path() -> PathBuf {
    crate::ledger::data_dir()
        .map(|d| d.join("guard-rulebook.json"))
        .unwrap_or_default()
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

/// Run as a PreToolUse guard: read the hook JSON on stdin, and if any rulebook
/// rule fires OR a stored memory names the file being touched, emit a
/// hookSpecificOutput with additionalContext ONLY - advisory text for the model,
/// while the tool call itself goes through the NORMAL permission flow untouched.
/// Never emit a permissionDecision: "allow" would BYPASS the permission system
/// (auto-approve the very calls the guard flags as risky), and blocking is not
/// the guard's job. HARD fail-open: any error prints nothing and exits 0 - the
/// guard must never block a tool call.
pub fn run_guard(db: &Path, rulebook: &Path) {
    let _ = try_guard(db, rulebook);
}

fn try_guard(db: &Path, rulebook: &Path) -> anyhow::Result<()> {
    // The kill switch silences EVERY guard surface (rulebook advisories too),
    // matching the documented contract on ledger::flag_present.
    if crate::ledger::flag_present(db, "THOR-SILENT.flag") {
        return Ok(());
    }
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

    // Static rulebook advisories (a missing rulebook is silent, never an error -
    // and must not skip the memory advisory below).
    let rules = std::fs::read_to_string(rulebook).map(|t| parse_rules(&t)).unwrap_or_default();
    let mut parts = evaluate(&rules, tool, &haystack);

    // Memory advisory: the first time this session touches a file, surface the
    // stored memories that NAME it (memories only - never code chunks). Drift is
    // decided at the moment of action; the prompt often has zero overlap with a
    // gotcha written in code language, but the file path does.
    if let Some(mem) = file_memory_advisory(db, &hook) {
        parts.push(mem);
    }
    if parts.is_empty() {
        return Ok(());
    }

    let context = format!("[THOR guard] {}", parts.join("  ||  "));
    // additionalContext WITHOUT a permissionDecision: the advisory reaches the
    // model and the permission system decides about the tool call as if no hook
    // existed. ("allow" would auto-approve past allowlists and permission mode.)
    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
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

/// How many file-naming memories to surface per (session, file).
const FILE_MEMORY_HITS: usize = 2;
/// How long a "no memory names this file" answer is cached (seconds). Without
/// it every tool call on a memory-less file - the overwhelmingly common case -
/// re-pays a full store open + O(n) recall on the per-tool-call hot path. Short
/// enough that a memory stored mid-session still surfaces within minutes.
const NEG_CACHE_SECS: u64 = 15 * 60;

/// The memory-backed half of the guard: for a tool call carrying a file_path,
/// recall MEMORIES (never chunks) that literally name the file, at most once per
/// (session, file) via the fail-open guard-seen ledger. Returns None to stay
/// silent. Rationale is measured, not assumed: recall over the raw tool call
/// returns 8/8 code chunks and 0 memories, so the candidate set must exclude
/// chunks up front, and a hit must name the file to count as "about" it.
fn file_memory_advisory(db: &Path, hook: &Value) -> Option<String> {
    if crate::ledger::flag_present(db, "THOR-SILENT.flag") {
        return None; // the THOR kill switch silences the file advisory too
    }
    let file_path = hook.get("tool_input")?.get("file_path")?.as_str()?.trim();
    if file_path.is_empty() {
        return None;
    }
    let session_id = hook.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
    if session_id.is_empty() {
        // No session identity: a shared "|<file>" key would suppress the
        // advisory across DIFFERENT sessions for the whole prune window. Match
        // capture_nudge: no debounce identity -> stay silent.
        return None;
    }
    let key = format!("{}|{}", session_id, file_path);
    match crate::ledger::get(db, "guard-seen", &key) {
        // already advised for this file this session
        Some(v) if v.is_u64() => return None,
        // fresh negative answer: skip the store open + recall entirely
        Some(v) => {
            let neg_ts = v.get("ts").and_then(|t| t.as_u64()).unwrap_or(0);
            if crate::review::now_secs().saturating_sub(neg_ts) <= NEG_CACHE_SECS {
                return None;
            }
        }
        None => {}
    }

    let p = Path::new(file_path);
    let name = p.file_name()?.to_str()?;
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    // Query = the file's name + its nearest two directory names, so bm25 can
    // reach a memory that mentions any of them; precision comes from the
    // name-check below, not from ranking.
    let mut terms: Vec<&str> = vec![name];
    if stem != name {
        terms.push(stem);
    }
    for anc in p.ancestors().skip(1).take(2) {
        if let Some(d) = anc.file_name().and_then(|d| d.to_str()) {
            terms.push(d);
        }
    }
    let query = terms.join(" ");

    let project = hook
        .get("cwd")
        .and_then(|v| v.as_str())
        .and_then(|c| crate::repo::project_key(Path::new(c)));
    let scope = crate::recall::RecallScope::current(project);
    let store = crate::event_store::EventStore::new(db).ok()?;
    let hits = crate::recall::recall_memories_scoped(&store, &query, 6, &scope).ok()?;

    // Keep only memories that literally NAME the file (full name, or a stem of
    // >= 3 chars) - "mentions the directory" is not "about this file".
    let name_l = name.to_lowercase();
    let stem_l = stem.to_lowercase();
    let named: Vec<_> = hits
        .into_iter()
        .filter(|h| {
            let b = h.body.to_lowercase();
            b.contains(&name_l) || (stem_l.chars().count() >= 3 && b.contains(&stem_l))
        })
        .take(FILE_MEMORY_HITS)
        .collect();
    if named.is_empty() {
        // Cache the miss briefly (NEG_CACHE_SECS) so repeated touches of a
        // memory-less file stop re-paying the recall; a memory stored later
        // still surfaces once the negative entry expires. Per-key upsert: a
        // concurrent guard on ANOTHER file can no longer lose this entry.
        let now = crate::review::now_secs();
        crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!({ "ts": now, "neg": true }));
        return None;
    }

    let lines: Vec<String> = named
        .iter()
        .map(|h| {
            let ty = h.fact_type.map(|t| format!("[{}] ", t.as_str())).unwrap_or_default();
            format!("{}{}: {}", ty, h.entity_id, crate::recall::snippet(&h.body, 200, &query))
        })
        .collect();

    let now = crate::review::now_secs();
    crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!(now));

    Some(format!(
        "stored memory about this file (verify before relying): {}",
        lines.join("  ||  ")
    ))
}

/// Eval seam: lets the drift-eval harness (examples/drift_eval.rs) drive the
/// REAL file-memory path without stdin. Hidden from docs - not a public API.
#[doc(hidden)]
pub fn file_memory_advisory_for_eval(db: &Path, hook: &Value) -> Option<String> {
    file_memory_advisory(db, hook)
}

/// Post-compaction reset for the file-touch advisories: drop every guard-seen
/// entry of this session (keys are "session_id|file_path"), positive AND
/// negative. A compaction destroys the advisory text along with the context, so
/// the next touch of a constrained file must re-advise - that window is exactly
/// what the memory-backed guard exists for. Fail-open: errors leave the ledger
/// as-is.
pub fn clear_session_guard_seen(db: &Path, session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    crate::ledger::remove_prefix(db, "guard-seen", &format!("{}|", session_id));
}

/// Response-rulebook for the Stop guard (the capability-amnesia / ssh-amnesia
/// class), separate from the PreToolUse command rulebook. Same no-CWD-fallback
/// rule as default_rulebook_path.
pub fn default_response_rulebook_path() -> PathBuf {
    crate::ledger::data_dir()
        .map(|d| d.join("guard-response-rulebook.json"))
        .unwrap_or_default()
}

/// Run as a Stop guard: read the Stop-hook JSON on stdin and, if the assistant's
/// final message matches a response rule (e.g. it asked the user to do something
/// it could do itself) OR looks like it contains an unstored durable fact (the
/// capture nudge), emit `{"decision":"block","reason":...}` so the model
/// reconsiders BEFORE it yields. Unlike the advisory PreToolUse guard this DOES
/// block the stop - that is the point for this class. HARD fail-open: any error
/// (or the loop-guard) prints nothing and exits 0, so a stop is never wrongly held.
pub fn run_stop_guard(db: &Path, rulebook: &Path) {
    let _ = try_stop_guard(db, rulebook);
}

/// Hard capture triggers (EN + NL), per the user's own capture rule: a new
/// project, a move/relocation of code or data, a major decision or direction
/// change, a confirmed gotcha. Deliberately conservative substrings - a false
/// positive costs a forced extra turn (the Stop guard is installed by DEFAULT),
/// so casual usage must not fire: bare "gotcha" ("Gotcha, I'll fix that") and
/// bare "decided to" ("I decided to grep first") are excluded; the marker
/// forms ("gotcha:", "we decided") are kept.
const CAPTURE_TRIGGERS: &[&str] = &[
    "besloten",
    "beslissing:",
    "besluit:",
    "afgesproken",
    "harde regel",
    "voortaan",
    "decision:",
    "we decided",
    "from now on",
    "gotcha:",
    "verplaatst naar",
    "gemigreerd naar",
    "migrated to",
    "nieuw project",
    "new project",
];

/// The capture triggers, user-tunable as a rulebook file next to the store
/// (`guard-capture-triggers.json`: a JSON array of lowercase substrings) - the
/// same customize-without-recompiling contract as the command/response
/// rulebooks. The built-in list is the fallback so the nudge works with zero
/// setup; a missing, malformed, or empty file falls back too (fail-open).
fn capture_triggers(db: &Path) -> Vec<String> {
    let path = db.with_file_name("guard-capture-triggers.json");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(Value::Array(entries)) = serde_json::from_str(&raw) {
            let list: Vec<String> = entries
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if !list.is_empty() {
                return list;
            }
        }
    }
    CAPTURE_TRIGGERS.iter().map(|s| s.to_string()).collect()
}

fn try_stop_guard(db: &Path, rulebook: &Path) -> anyhow::Result<()> {
    // The kill switch silences the response rules too - a silenced THOR must
    // never actively BLOCK a stop.
    if crate::ledger::flag_present(db, "THOR-SILENT.flag") {
        return Ok(());
    }
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
    // A missing rulebook leaves the response rules empty but must NOT skip the
    // capture nudge below.
    let haystack = tool_input_text(&Value::String(msg.to_string()));
    let rules = std::fs::read_to_string(rulebook).map(|t| parse_rules(&t)).unwrap_or_default();
    let fired = evaluate(&rules, "response", &haystack);

    let reason = if !fired.is_empty() {
        format!("[THOR] {}", fired.join("  ||  "))
    } else if let Some(r) = capture_nudge(db, &hook, &haystack) {
        r
    } else {
        return Ok(());
    };
    let out = serde_json::json!({ "decision": "block", "reason": reason });
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{}", out);
    let _ = stdout.flush();
    Ok(())
}

/// The capture safety net: when the final reply contains a hard trigger
/// (decision / gotcha / migration / new-project language), block the stop ONCE
/// per session with a "store it or say it's stored" reason. Keyword-gated and
/// loop-safe (the stop_hook_active check above catches the retry), so model
/// proactivity is no longer the only thing standing between a durable fact and
/// oblivion. Fail-open at every step.
fn capture_nudge(db: &Path, hook: &Value, haystack_lower: &str) -> Option<String> {
    if crate::ledger::flag_present(db, "THOR-SILENT.flag") {
        return None; // the THOR kill switch silences the nudge too
    }
    let session_id = hook.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
    if session_id.is_empty() {
        return None; // no session identity -> cannot debounce -> stay silent
    }
    if !capture_triggers(db).iter().any(|t| haystack_lower.contains(t.as_str())) {
        return None;
    }
    // Atomic once-per-session claim: two concurrent Stop hooks can both pass a
    // contains-check, but only one INSERT wins - "at most one nudge" is exact.
    let now = crate::review::now_secs();
    if !crate::ledger::insert_once(db, "capture", session_id, &serde_json::json!(now)) {
        return None;
    }
    Some(
        "[THOR capture] This reply looks like it contains a durable decision, gotcha, or \
         milestone (project start, code/data move, direction change). If it is durable and \
         not yet stored: store it in THOR now (remember - concise and self-contained). If it \
         is already stored or not durable, just finish. This nudge fires at most once per session."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_capture_triggers_rulebook_overrides_and_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        // no file -> built-in list
        assert!(capture_triggers(&db).iter().any(|t| t == "gotcha:"));
        // a valid rulebook replaces the list entirely
        std::fs::write(db.with_file_name("guard-capture-triggers.json"), r#"["mijn eigen trigger"]"#).unwrap();
        let list = capture_triggers(&db);
        assert_eq!(list, vec!["mijn eigen trigger"]);
        // malformed / empty -> fallback (fail-open)
        std::fs::write(db.with_file_name("guard-capture-triggers.json"), "not json").unwrap();
        assert!(capture_triggers(&db).iter().any(|t| t == "we decided"));
        std::fs::write(db.with_file_name("guard-capture-triggers.json"), "[]").unwrap();
        assert!(capture_triggers(&db).iter().any(|t| t == "besloten"));
    }

    #[test]
    fn test_clear_session_guard_seen_only_this_session() {
        // Review finding: post-compaction only the courier ledger was reset, so
        // file-touch advisories stayed suppressed exactly when their text had
        // just been destroyed with the context.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        crate::ledger::upsert(&db, "guard-seen", "s1|a.rs", &json!(100));
        crate::ledger::upsert(&db, "guard-seen", "s1|b.rs", &json!({"ts": 100, "neg": true}));
        crate::ledger::upsert(&db, "guard-seen", "s2|a.rs", &json!(100));
        clear_session_guard_seen(&db, "s1");
        assert!(crate::ledger::get(&db, "guard-seen", "s1|a.rs").is_none(), "positive entry cleared");
        assert!(crate::ledger::get(&db, "guard-seen", "s1|b.rs").is_none(), "negative-cache entry cleared");
        assert!(crate::ledger::get(&db, "guard-seen", "s2|a.rs").is_some(), "other sessions stay untouched");
        // an empty session id never wipes anything
        clear_session_guard_seen(&db, "");
        assert!(crate::ledger::get(&db, "guard-seen", "s2|a.rs").is_some());
    }

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
    fn test_capture_nudge_fires_once_per_session_and_is_keyword_gated() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let hook = |sid: &str| json!({ "session_id": sid, "last_assistant_message": "x" });

        // a durable-decision reply fires...
        let hay = tool_input_text(&json!("We hebben besloten de NAS-sync via ship/recv te doen."));
        assert!(capture_nudge(&db, &hook("s1"), &hay).is_some(), "decision language fires");
        // ...but only once per session
        assert!(capture_nudge(&db, &hook("s1"), &hay).is_none(), "second stop in s1 is silent");
        // a new session gets its own nudge
        assert!(capture_nudge(&db, &hook("s2"), &hay).is_some(), "a fresh session nudges again");
        // plain status text never fires
        let plain = tool_input_text(&json!("Tests are green, pushed to main."));
        assert!(capture_nudge(&db, &hook("s3"), &plain).is_none(), "no trigger -> no nudge");
        // no session id -> cannot debounce -> silent
        assert!(capture_nudge(&db, &json!({}), &hay).is_none(), "sessionless stop stays silent");
        // casual usage must NOT fire (the Stop guard is installed by default)
        let casual = tool_input_text(&json!("Gotcha, I'll rename the file and rerun the tests."));
        assert!(capture_nudge(&db, &hook("s4"), &casual).is_none(), "bare 'gotcha' is casual, not a fact");
        let casual2 = tool_input_text(&json!("I decided to grep the logs first."));
        assert!(capture_nudge(&db, &hook("s4"), &casual2).is_none(), "bare 'decided to' is prose");
        // the marker form still fires
        let marked = tool_input_text(&json!("GOTCHA: nginx strips X-Forwarded-Proto here."));
        assert!(capture_nudge(&db, &hook("s5"), &marked).is_some(), "'gotcha:' marker fires");
        // the THOR kill switch silences the nudge
        std::fs::write(dir.path().join("THOR-SILENT.flag"), "").unwrap();
        assert!(capture_nudge(&db, &hook("s6"), &hay).is_none(), "THOR-SILENT.flag silences capture");
    }

    #[test]
    fn test_file_memory_advisory_memories_only_once_per_session() {
        use crate::event_store::{EventKind, EventStore};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // a code chunk AND a gotcha memory both naming courier.rs
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:src/courier.rs#0", None,
                    "fn courier() {} // courier.rs code chunk",
                )
                .unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:mem-1", None,
                    "GOTCHA: courier.rs must stay hard fail-open - never add a path that can error a prompt",
                )
                .unwrap();
        }
        let proj = dir.path().join("Proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        let hook = json!({
            "session_id": "s1",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/courier.rs").to_string_lossy() }
        });

        let adv = file_memory_advisory(&db, &hook).expect("first touch must advise");
        assert!(adv.contains("Proj:mem-1"), "the gotcha memory surfaces: {adv}");
        assert!(adv.contains("[gotcha]"), "typed tag rendered: {adv}");
        assert!(adv.contains("fail-open"), "snippet carries the constraint: {adv}");
        assert!(!adv.contains("courier.rs#0"), "code chunks NEVER appear in the advisory: {adv}");

        // second touch of the same file in the same session: silent
        assert!(file_memory_advisory(&db, &hook).is_none(), "once per (session, file)");
        // a different session advises again
        let hook2 = json!({
            "session_id": "s2",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/courier.rs").to_string_lossy() }
        });
        assert!(file_memory_advisory(&db, &hook2).is_some());
        // a file no memory names: silent, and the miss is negative-cached so
        // repeated touches skip the store open (a second call is also None)
        let other = json!({
            "session_id": "s1",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/other.rs").to_string_lossy() }
        });
        assert!(file_memory_advisory(&db, &other).is_none());
        let neg_key = format!("s1|{}", proj.join("src/other.rs").to_string_lossy());
        let neg = crate::ledger::get(&db, "guard-seen", &neg_key);
        assert!(
            neg.as_ref().and_then(|v| v.get("neg")).is_some(),
            "a miss writes a short-TTL negative entry: {neg:?}"
        );
        assert!(file_memory_advisory(&db, &other).is_none(), "cached miss stays silent");

        // sessionless hooks stay silent (no debounce identity, no 48h cross-
        // session suppression via a shared \"|<file>\" key)
        let sessionless = json!({
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/courier.rs").to_string_lossy() }
        });
        assert!(file_memory_advisory(&db, &sessionless).is_none(), "no session_id -> silent");

        // the THOR kill switch silences the advisory
        std::fs::write(dir.path().join("THOR-SILENT.flag"), "").unwrap();
        let hook3 = json!({
            "session_id": "s3",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/courier.rs").to_string_lossy() }
        });
        assert!(file_memory_advisory(&db, &hook3).is_none(), "THOR-SILENT.flag silences the guard advisory");
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
