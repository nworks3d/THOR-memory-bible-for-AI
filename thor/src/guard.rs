use serde_json::Value;
use std::collections::HashSet;
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
    // Same idea for COMMANDS: a typed gotcha/decision that names a distinctive
    // command token ("force-recreate", a host, a subcommand) fires when that
    // command is about to run - the class of drift (ssh-amnesia, hot-patch-as-
    // deploy) that prompt-recall can never see because the prompt shares no
    // words with the constraint.
    if let Some(mem) = command_memory_advisory(db, &hook) {
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

/// Up to this many PROSE chunks ride along after the memories in the file
/// advisory. Bounded separately so ingested docs can never crowd a typed
/// constraint out of its slot. Three, not two: a CHANGELOG names a busy file
/// in many paragraphs and the one that matters is rarely the bm25 top hit.
const DOC_CHUNK_HITS: usize = 3;

/// Fair-share char budget for the bodies of one advisory's entries. The fixed
/// 200-char snippet was the same decisive-details killer the courier's fixed
/// caps were (measured there: fair-share bought +9.5pp gold-term coverage):
/// the right CHANGELOG paragraph was served and then truncated past the point
/// of usefulness. One advisory fires at most once per (session, file), so the
/// bound is per-moment, not per-prompt.
const ADVISORY_BUDGET_CHARS: usize = 2400;

/// Per-entry snippet cap: the budget fair-shared over the entries, floored so
/// a crowded advisory still says something per entry, capped so one entry can
/// never be a wall of text.
fn advisory_snippet_cap(entries: usize) -> usize {
    (ADVISORY_BUDGET_CHARS / entries.max(1)).clamp(200, 1200)
}
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
    let project = hook
        .get("cwd")
        .and_then(|v| v.as_str())
        .and_then(|c| crate::repo::project_key(Path::new(c)));
    // The SYMBOL BRIDGE. A gotcha about `serve_deliberate` that never names
    // courier.rs was invisible to this advisory, yet touching courier.rs is
    // exactly the moment it must surface - the drift corpus measured 14 of 73
    // preventers reachable ONLY through action vocabulary the prompt lacks.
    // The sidecar knows which names this file defines; memories naming those
    // symbols are memories about this file. Distinctive names only: a memory
    // containing "main" or "test" is not about this file because of it.
    let symbols: Vec<String> = crate::symbols::SymbolStore::open_default(db)
        .map(|sy| sy.defined_in_file(name, project.as_deref()))
        .unwrap_or_default()
        .into_iter()
        .filter(|s| {
            s.contains('_')
                || s.chars().count() >= 6
                || (s.chars().any(|c| c.is_uppercase()) && s.chars().any(|c| c.is_lowercase()))
        })
        .take(12)
        .collect();
    // Query = the file's name + its nearest two directory names + the symbols
    // it defines, so bm25 can reach a memory that mentions any of them;
    // precision comes from the name-check below, not from ranking.
    let mut terms: Vec<&str> = vec![name];
    if stem != name {
        terms.push(stem);
    }
    for anc in p.ancestors().skip(1).take(2) {
        if let Some(d) = anc.file_name().and_then(|d| d.to_str()) {
            terms.push(d);
        }
    }
    terms.extend(symbols.iter().map(String::as_str));
    let query = terms.join(" ");

    let scope = crate::recall::RecallScope::current(project);
    let store = crate::event_store::EventStore::new(db).ok()?;

    // Anchor pass FIRST: a fact whose author declared this exact path (full
    // path, path suffix, or bare file name; slashes normalized) surfaces
    // regardless of the name heuristics below.
    let name_l = name.to_lowercase();
    let path_norm = file_path.replace('\\', "/").to_lowercase();
    let anchored = anchored_memories(&store, &scope, &|a| {
        let a = a.replace('\\', "/").to_lowercase();
        !a.is_empty()
            && (a == path_norm || a == name_l || path_norm.ends_with(&format!("/{a}")))
    });

    let hits = crate::recall::recall_memories_scoped(&store, &query, 6, &scope).ok()?;

    // Keep only memories that literally NAME the file (full name, or a stem of
    // >= 3 chars) OR one of the distinctive symbols this file defines -
    // "mentions the directory" is not "about this file", but "names a function
    // that lives in it" is.
    let stem_l = stem.to_lowercase();
    let symbols_l: Vec<String> = symbols.iter().map(|s| s.to_lowercase()).collect();
    let heuristic: Vec<_> = hits
        .into_iter()
        .filter(|h| {
            let b = h.body.to_lowercase();
            b.contains(&name_l)
                || (stem_l.chars().count() >= 3 && b.contains(&stem_l))
                || symbols_l.iter().any(|s| b.contains(s.as_str()))
        })
        .collect();
    let named = merge_anchored(anchored, heuristic, FILE_MEMORY_HITS);

    // The ingested prose knows files too: a CHANGELOG paragraph or design doc
    // that NAMES this file documents decisions the agent cannot see in the
    // file itself. Measured on the live drift corpus (2026-07-22): 32 of 59
    // preventers are chunks, invisible to a memories-only advisory. A
    // dedicated doc-chunks-only lane (never a shared pool: raw recall over a
    // tool call is 8/8 code chunks, measured, so prose would never survive
    // the crowding). Never a chunk OF the touched file - its content is
    // already on the agent's screen. Fail-soft: a chunk-pass error costs the
    // chunks, never the memory advisory. (Second O(n) fold per uncached
    // touch, same bounded class as the anchored_memories fold above.)
    let mut doc_chunks: Vec<crate::recall::RecallHit> =
        crate::recall::recall_doc_chunks_scoped(&store, &query, 8, &scope)
            .unwrap_or_default()
            .into_iter()
            .filter(|h| {
                crate::repo::chunk_rel(&h.entity_id)
                    .and_then(|r| Path::new(r).file_name())
                    .and_then(|f| f.to_str())
                    .map_or(true, |f| !f.eq_ignore_ascii_case(name))
            })
            .filter(|h| {
                let b = h.body.to_lowercase();
                b.contains(&name_l)
                    || (stem_l.chars().count() >= 3 && b.contains(&stem_l))
                    || symbols_l.iter().any(|s| b.contains(s.as_str()))
            })
            .collect();
    // A paragraph that spells out the file NAME beats one reached via a stem
    // or symbol coincidence - "about printers.js" over "mentions a printers
    // symbol". Stable sort keeps bm25 order within each class.
    doc_chunks.sort_by_key(|h| !h.body.to_lowercase().contains(&name_l));
    doc_chunks.truncate(DOC_CHUNK_HITS);

    if named.is_empty() && doc_chunks.is_empty() {
        // Cache the miss briefly (NEG_CACHE_SECS) so repeated touches of a
        // memory-less file stop re-paying the recall; a memory stored later
        // still surfaces once the negative entry expires. Per-key upsert: a
        // concurrent guard on ANOTHER file can no longer lose this entry.
        let now = crate::review::now_secs();
        crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!({ "ts": now, "neg": true }));
        return None;
    }

    let cap = advisory_snippet_cap(named.len() + doc_chunks.len());
    let lines: Vec<String> = named
        .iter()
        .chain(doc_chunks.iter())
        .map(|h| {
            let ty = h.fact_type.map(|t| format!("[{}] ", t.as_str())).unwrap_or_default();
            format!("{}{}: {}", ty, h.entity_id, crate::recall::snippet(&h.body, cap, &query))
        })
        .collect();

    let now = crate::review::now_secs();
    crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!(now));

    Some(format!(
        "stored memory about this file (verify before relying): {}",
        lines.join("  ||  ")
    ))
}

/// Live, in-scope, single-headed memories whose author-declared anchors
/// (remember's `anchors` param, footer field) match `pred`. The anchor pass
/// runs BEFORE the lexical heuristics: an anchor is exact declared intent and
/// must never depend on bm25 reaching the fact or on the fact being typed.
///
/// KNOWN COST: this fold duplicates the one recall_memories_scoped performs
/// internally right after, so a non-debounced advisory pays the O(n) log fold
/// twice. Bounded by design (once per file/command-token-set per session,
/// misses neg-cached) and it collapses to two cheap lookups when the
/// materialized heads table (M2) lands - measured before optimizing further.
fn anchored_memories(
    store: &crate::event_store::EventStore,
    scope: &crate::recall::RecallScope,
    pred: &dyn Fn(&str) -> bool,
) -> Vec<crate::recall::RecallHit> {
    // M2 fast path: walk the materialized heads (one indexed join) instead of
    // folding the whole log. Same selection rules as the fold path below;
    // stale projection = fall through to the authoritative fold.
    if store.heads_projection_current() {
        if let Ok(rows) = store.projected_head_events() {
            let mut out = Vec::new();
            for (head, head_count, project) in rows {
                if crate::repo::is_chunk_id(&head.entity_id) || head_count != 1 {
                    continue;
                }
                if matches!(head.kind, crate::event_store::EventKind::FactRetracted) {
                    continue;
                }
                if !scope.allows(project.as_deref()) {
                    continue;
                }
                if crate::footer::anchors(&head.body).iter().any(|a| pred(a)) {
                    out.push(crate::recall::RecallHit {
                        entity_id: head.entity_id.clone(),
                        rev: head.this_hash.clone(),
                        body: head.body.clone(),
                        kind: head.kind,
                        is_diverged: false, // head_count == 1 checked above
                        rank: 0.0,
                        project,
                        fact_type: crate::repo::fact_type(&head.body),
                        matched_and: true,
                    });
                }
            }
            out.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
            return out;
        }
    }
    let Ok(events) = store.get_all_events() else { return Vec::new() };
    let heads = crate::cas::compute_head_sets(&events);
    let projects = crate::cas::compute_projects(&events);
    let by_hash: std::collections::HashMap<&str, &crate::event_store::Event> =
        events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
    let mut out = Vec::new();
    for (id, hs) in &heads {
        if crate::repo::is_chunk_id(id) || hs.heads.len() != 1 {
            continue;
        }
        let Some(head) = by_hash.get(hs.heads.iter().next().expect("len checked").as_str())
        else {
            continue;
        };
        if matches!(head.kind, crate::event_store::EventKind::FactRetracted) {
            continue;
        }
        let effective = projects.get(id).and_then(|o| o.as_deref());
        if !scope.allows(effective) {
            continue;
        }
        if crate::footer::anchors(&head.body).iter().any(|a| pred(a)) {
            out.push(crate::recall::RecallHit {
                entity_id: id.clone(),
                rev: head.this_hash.clone(),
                body: head.body.clone(),
                kind: head.kind,
                is_diverged: hs.is_diverged,
                rank: 0.0,
                project: effective.map(str::to_string),
                fact_type: crate::repo::fact_type(&head.body),
                matched_and: true,
            });
        }
    }
    out.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
    out
}

/// Anchored hits first, then the heuristic hits that are not already among
/// them, capped - the shared merge for both advisories.
fn merge_anchored(
    anchored: Vec<crate::recall::RecallHit>,
    heuristic: Vec<crate::recall::RecallHit>,
    cap: usize,
) -> Vec<crate::recall::RecallHit> {
    let mut out = anchored;
    for h in heuristic {
        if !out.iter().any(|a| a.entity_id == h.entity_id) {
            out.push(h);
        }
    }
    out.truncate(cap);
    out
}

/// Eval seam: lets the drift-eval harness (examples/drift_eval.rs) drive the
/// REAL file-memory path without stdin. Hidden from docs - not a public API.
#[doc(hidden)]
pub fn file_memory_advisory_for_eval(db: &Path, hook: &Value) -> Option<String> {
    file_memory_advisory(db, hook)
}

/// Eval seam for the command advisory, same contract as above.
#[doc(hidden)]
pub fn command_memory_advisory_for_eval(db: &Path, hook: &Value) -> Option<String> {
    command_memory_advisory(db, hook)
}

/// Shell verbs and generic words too common to identify a constraint: see
/// `vocab::COMMAND_NOISE` (a salient token must clear these AND the stopwords).
use crate::vocab::COMMAND_NOISE;

/// Distinctive tokens of a shell command: what could plausibly NAME a stored
/// constraint. Kept when len >= 5 and not noise/stopword, or when the token
/// carries structure (a '-'/'.'/':' composite like "force-recreate", a host, a
/// flag name) - single common words never qualify.
fn salient_command_tokens(command: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let push = |t: String, seen: &mut HashSet<String>, out: &mut Vec<String>| {
        let ok = t.chars().count() >= 5
            && t.chars().any(|c| c.is_alphabetic())
            && !COMMAND_NOISE.contains(&t.as_str())
            && (t.contains('-')
                || t.contains('.')
                || t.contains(':')
                || t.contains('/')
                || t.contains('@')
                || t.chars().count() >= 6);
        if ok && seen.insert(t.clone()) {
            out.push(t);
        }
    };
    for raw in command.split(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '|' | ';' | '&' | '(' | ')' | '<' | '>' | '=' | ',')
    }) {
        let t = raw.trim_matches(|c: char| matches!(c, '-' | '/' | '.' | ':')).to_lowercase();
        if t.is_empty() {
            continue;
        }
        push(t.clone(), &mut seen, &mut out);
        // Composite tokens (remote specs, paths) rarely appear verbatim in a
        // memory body - their PARTS do. Also emit the path leaf and the host
        // segment: "deploy@storage.internal:/srv/x" -> "storage.internal";
        // "//fileserver/data/orders.db" -> "orders.db".
        if t.contains('/') || t.contains('@') || t.contains(':') {
            if let Some(leaf) = t.rsplit('/').next() {
                let leaf = leaf.split(':').next().unwrap_or(leaf);
                push(leaf.to_string(), &mut seen, &mut out);
            }
            let host_part = t.rsplit('@').next().unwrap_or(&t);
            let host = host_part.split([':', '/']).find(|s| !s.is_empty()).unwrap_or("");
            push(host.to_string(), &mut seen, &mut out);
        }
        if out.len() >= 10 {
            break;
        }
    }
    out.truncate(10);
    out
}

/// The command half of the memory-backed guard: on a Bash/PowerShell call,
/// recall TYPED constraint memories (gotcha/decision/preference - never notes,
/// never chunks) that literally contain one of the command's distinctive
/// tokens. Debounced per (session, token-set) with the same negative-cache
/// pattern as the file advisory; every failure path is silent (fail-open).
fn command_memory_advisory(db: &Path, hook: &Value) -> Option<String> {
    if crate::ledger::flag_present(db, "THOR-SILENT.flag") {
        return None;
    }
    let tool = hook.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(tool, "Bash" | "PowerShell") {
        return None;
    }
    let command = hook.get("tool_input")?.get("command")?.as_str()?;
    let session_id = hook.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
    if session_id.is_empty() {
        return None; // no debounce identity -> stay silent (match the file guard)
    }
    let tokens = salient_command_tokens(command);
    // Debounce on the token SET, not the raw command: trivially-different
    // invocations (an extra flag value, another filename) share the key. A
    // token-less command (all shell noise, e.g. "docker compose up") still
    // gets the ANCHOR pass below - an author-declared anchor is exactly the
    // kind of constraint that hides behind noise words - debounced on the raw
    // command instead.
    let mut sorted = tokens.clone();
    sorted.sort();
    let key = if sorted.is_empty() {
        let raw: String = command.trim().to_lowercase().chars().take(80).collect();
        format!("{}|cmd-raw:{}", session_id, raw)
    } else {
        format!("{}|cmd:{}", session_id, sorted.join(","))
    };
    match crate::ledger::get(db, "guard-seen", &key) {
        Some(v) if v.is_u64() => return None,
        Some(v) => {
            let neg_ts = v.get("ts").and_then(|t| t.as_u64()).unwrap_or(0);
            if crate::review::now_secs().saturating_sub(neg_ts) <= NEG_CACHE_SECS {
                return None;
            }
        }
        None => {}
    }

    let project = hook
        .get("cwd")
        .and_then(|v| v.as_str())
        .and_then(|c| crate::repo::project_key(Path::new(c)));
    let scope = crate::recall::RecallScope::current(project);
    let store = crate::event_store::EventStore::new(db).ok()?;

    // Anchor pass FIRST: a fact whose author declared an exact command string
    // ("docker compose up") surfaces the moment that string appears in the
    // command - no typed requirement, no token heuristics.
    let cmd_l = command.to_lowercase();
    let anchored = anchored_memories(&store, &scope, &|a| {
        let a = a.to_lowercase();
        a.chars().count() >= 3 && cmd_l.contains(&a)
    });

    let query = tokens.join(" ");
    // Precision filter (heuristic leg; token-less commands skip it): TYPED
    // constraints only, and the match must be more than one shared plain word
    // - "same topic" is not "about this command". Measured against the live
    // store: single generic-word matches ("semantic", "origin", "upload")
    // fired on loosely-related decisions on 4 of 12 benign commands. A hit
    // qualifies only via a STRUCTURED token (a composite like
    // "force-recreate", a host, a path leaf - near-unique by construction) or
    // via >= 2 distinct shared tokens.
    let heuristic: Vec<_> = if tokens.is_empty() {
        Vec::new()
    } else {
        let hits = crate::recall::recall_memories_scoped(&store, &query, 6, &scope).ok()?;
        let structured = |t: &str| {
            t.contains('-')
                || t.contains('.')
                || t.contains(':')
                || t.contains('/')
                || t.contains('@')
        };
        hits.into_iter()
            .filter(|h| h.fact_type.is_some())
            .filter(|h| {
                let b = h.body.to_lowercase();
                let matched: Vec<&String> =
                    tokens.iter().filter(|t| b.contains(t.as_str())).collect();
                matched.iter().any(|t| structured(t)) || matched.len() >= 2
            })
            .collect()
    };
    let named = merge_anchored(anchored, heuristic, FILE_MEMORY_HITS);
    let now = crate::review::now_secs();
    if named.is_empty() {
        crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!({ "ts": now, "neg": true }));
        return None;
    }
    let cap = advisory_snippet_cap(named.len());
    let lines: Vec<String> = named
        .iter()
        .map(|h| {
            let ty = h.fact_type.map(|t| format!("[{}] ", t.as_str())).unwrap_or_default();
            format!("{}{}: {}", ty, h.entity_id, crate::recall::snippet(&h.body, cap, &query))
        })
        .collect();
    crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!(now));
    Some(format!(
        "stored memory about this command (verify before relying): {}",
        lines.join("  ||  ")
    ))
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
         not yet stored: store it in THOR now (remember - concise and self-contained), and ask \
         yourself WHEN it should fire: pass those task words (commands, file names, error \
         strings) as triggers. If it is already stored or not durable, just finish. This nudge \
         fires at most once per session."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_salient_command_tokens_precision() {
        // structured composites and rare words survive; shell verbs and short
        // generic tokens never do
        assert_eq!(
            salient_command_tokens("docker compose -p app up -d --force-recreate"),
            vec!["force-recreate"]
        );
        let t = salient_command_tokens("ssh user@nas.example.lan \"touch /srv/share/deploy.flag\"");
        assert!(t.iter().any(|x| x.contains("nas.example.lan")), "hostnames are salient: {t:?}");
        assert!(t.iter().any(|x| x.contains("deploy.flag")), "path leaves are salient: {t:?}");
        assert!(salient_command_tokens("git status && git log").is_empty(), "pure shell noise -> no tokens");
        assert!(salient_command_tokens("ls -la").is_empty());
    }

    #[test]
    fn test_command_memory_advisory_typed_only_once_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            use crate::event_store::EventKind;
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "g1", None,
                    "GOTCHA: never run --force-recreate against the prod stack; use the deploy route\n\n[memory/gotcha | tags: deploy | project: global]",
                )
                .unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "n1", None,
                    "plain note that also mentions force-recreate once",
                )
                .unwrap();
        }
        let hook = json!({
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": { "command": "docker compose -p app up -d --force-recreate" }
        });
        let adv = command_memory_advisory(&db, &hook).expect("typed gotcha naming the token fires");
        assert!(adv.contains("g1"), "{adv}");
        assert!(adv.contains("[gotcha]"), "{adv}");
        assert!(!adv.contains("n1"), "untyped notes never fire the command guard: {adv}");
        // debounced per (session, token-set)
        assert!(command_memory_advisory(&db, &hook).is_none(), "once per session per token-set");
        // a different session advises again
        let hook2 = json!({
            "session_id": "s2",
            "tool_name": "Bash",
            "tool_input": { "command": "docker compose up --force-recreate" }
        });
        assert!(command_memory_advisory(&db, &hook2).is_some());
        // non-shell tools and noise-only commands stay silent
        let edit = json!({ "session_id": "s1", "tool_name": "Edit", "tool_input": { "command": "x --force-recreate y" } });
        assert!(command_memory_advisory(&db, &edit).is_none(), "only Bash/PowerShell");
        let noise = json!({ "session_id": "s3", "tool_name": "Bash", "tool_input": { "command": "git status" } });
        assert!(command_memory_advisory(&db, &noise).is_none(), "no salient tokens -> silent");
    }

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
    fn test_symbol_bridge_surfaces_a_memory_that_never_names_the_file() {
        use crate::event_store::{EventKind, EventStore};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // The file's chunk defines the symbol...
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:src/serving.rs#0", None,
                    "pub fn serve_widgets() {\n    render();\n}",
                )
                .unwrap();
            // ...and the gotcha names ONLY the symbol - never the file, never
            // the directory. Before the bridge this was invisible at the exact
            // moment it mattered: the drift corpus measured 14 of 73
            // preventers reachable only through action vocabulary like this.
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:mem-sym", None,
                    "gotcha: serve_widgets must never run during a rebuild - it reads the half-written index\n\n[memory/gotcha | tags: rendering | project: Proj]",
                )
                .unwrap();
            let mut sy =
                crate::symbols::SymbolStore::open(&crate::symbols::default_symbols_path(&db))
                    .unwrap();
            sy.rebuild(&store).unwrap();
        }
        let proj = dir.path().join("Proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        let hook = json!({
            "session_id": "s-bridge",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/serving.rs").to_string_lossy() }
        });
        let adv = file_memory_advisory(&db, &hook).expect("the symbol bridge must advise");
        assert!(adv.contains("Proj:mem-sym"), "the symbol-only gotcha surfaces: {adv}");
        assert!(adv.contains("never run during a rebuild"), "with its content: {adv}");

        // Control: a file defining nothing stays silent - the bridge must not
        // turn every first touch into an advisory.
        let hook2 = json!({
            "session_id": "s-bridge2",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/unrelated.rs").to_string_lossy() }
        });
        assert!(file_memory_advisory(&db, &hook2).is_none(), "no symbols, no fire");
    }

    #[test]
    fn test_anchored_fact_beats_every_heuristic_on_both_advisories() {
        use crate::event_store::{EventKind, EventStore};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // FILE anchor: the body never NAMES the file (the heuristic can
            // never match it) - only the anchors field ties it to the path
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:mem-anch-f", None,
                    "always validate with sh -n after editing; a syntax error makes the watcher silently do nothing\n\n[memory/gotcha | tags: deploy | anchors: deploy/watcher.sh | project: Proj]",
                )
                .unwrap();
            // COMMAND anchor: an UNTYPED note (the heuristic leg requires
            // typed) anchored on a phrase made of pure shell-noise words
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:mem-anch-c", None,
                    "bring the stack up only after the config check has passed\n\n[memory/note | tags: | anchors: docker compose up | project: Proj]",
                )
                .unwrap();
            // control twins: same texts, same words as TAGS, no anchors field
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:mem-ctrl-f", None,
                    "always validate with sh -n after edits; syntax errors make watchers silently do nothing\n\n[memory/gotcha | tags: watcher.sh deploy | project: Proj]",
                )
                .unwrap();
        }
        let proj = dir.path().join("Proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();

        // file advisory: the anchored fact surfaces on its exact path...
        let hook = json!({
            "session_id": "s1",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("deploy/watcher.sh").to_string_lossy() }
        });
        let adv = file_memory_advisory(&db, &hook).expect("anchored fact must advise");
        assert!(adv.contains("Proj:mem-anch-f"), "anchored fact first: {adv}");
        // ...and an unrelated file stays silent (anchors are exact, not fuzzy)
        let other = json!({
            "session_id": "s1",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("deploy/other.sh").to_string_lossy() }
        });
        assert!(file_memory_advisory(&db, &other).is_none(), "no anchor, no name match = silent");

        // command advisory: all-noise command, untyped fact - only the anchor fires
        let cmd = json!({
            "session_id": "s2",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Bash",
            "tool_input": { "command": "docker compose up -d" }
        });
        let adv = command_memory_advisory(&db, &cmd).expect("anchored command must advise");
        assert!(adv.contains("Proj:mem-anch-c"), "untyped anchored fact surfaces: {adv}");
        // same session, same command: debounced
        assert!(command_memory_advisory(&db, &cmd).is_none(), "raw-command debounce holds");
        // an unanchored noise-only command stays silent
        let bare = json!({
            "session_id": "s2",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Bash",
            "tool_input": { "command": "docker compose down" }
        });
        assert!(command_memory_advisory(&db, &bare).is_none(), "no anchor in it = silent");
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
    fn test_file_advisory_serves_doc_chunks_naming_the_file() {
        use crate::event_store::{EventKind, EventStore};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // a CHANGELOG paragraph that documents a decision about printers.js
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:docs/CHANGELOG.md#0", None,
                    "Removed the hardcoded VALID_MODELS list from printers.js in favor of a DB query.",
                )
                .unwrap();
            // a code chunk that also names it: still never served
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:src/other.js#0", None,
                    "const routes = require('./printers.js'); // wiring",
                )
                .unwrap();
            // a chunk OF the touched file: its content is on the agent's screen
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:src/printers.js#0", None,
                    "module.exports = { validate }; // printers.js body",
                )
                .unwrap();
        }
        let proj = dir.path().join("Proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        let hook = json!({
            "session_id": "s1",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/printers.js").to_string_lossy() }
        });

        // No memory names the file - the doc chunk alone must carry the advisory.
        let adv = file_memory_advisory(&db, &hook).expect("a doc chunk naming the file advises");
        assert!(adv.contains("Proj:docs/CHANGELOG.md#0"), "the CHANGELOG paragraph serves: {adv}");
        assert!(adv.contains("VALID_MODELS"), "snippet carries the decision: {adv}");
        assert!(!adv.contains("Proj:src/other.js#0"), "code chunks still never serve: {adv}");
        assert!(
            !adv.contains("Proj:src/printers.js#0"),
            "a chunk of the touched file itself never serves: {adv}"
        );
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
