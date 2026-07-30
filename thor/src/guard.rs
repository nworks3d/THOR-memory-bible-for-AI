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
    /// Length floor on the haystack, in characters. 0 (absent) = no floor.
    /// Exists for the RESPONSE rulebook, where the haystack is the assistant's
    /// whole message: a rule about the SHAPE of a report ("open with a plain
    /// summary") should not fire on a two-line status note that happens to
    /// contain one technical word. Measured 2026-07-26: a 230-character closing
    /// line was blocked for containing the word "pushed" - correct by the
    /// letter, wrong by the intent. A word list cannot tell those apart;
    /// length can.
    pub min_chars: usize,
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
                min_chars: r
                    .get("min_chars")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .try_into()
                    .unwrap_or(usize::MAX),
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
        // Too short to be the thing this rule is about (see Rule::min_chars).
        if rule.min_chars > 0 && haystack_lower.chars().count() < rule.min_chars {
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

    // Auto-echo experiment: remember what this session actually touches, so
    // the Stop moment can settle judgment debt mechanically. No-op (zero
    // writes) unless THOR-EXP-AUTO-ECHO.flag is present.
    record_touched(db, &hook, &input);

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

/// How many file-naming memories to surface per (session, file). Read by
/// `consolidate`'s anchor-crowding report, so the report can never drift from
/// the cap the guard actually applies.
pub(crate) const FILE_MEMORY_HITS: usize = 2;

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
/// Stems that name a file's ROLE, never its subject: prose containing them is
/// almost never about that file. Measured 2026-07-25 with the precision probe:
/// touching `main.rs` served two doc paragraphs about the git branch "main" -
/// the stem is a perfectly good English word here, so it identifies nothing.
/// Deliberately tiny and structural (no topic words): a real subject stem like
/// `cas` or `guard` measured GOOD and must keep working.
const ROLE_STEMS: [&str; 8] = ["main", "lib", "mod", "test", "tests", "index", "util", "utils"];

/// Does this already-lowercased prose name the touched file? One rule for both
/// lanes (memories and doc chunks), because they asked the same question in two
/// places and only one of them ever got fixed.
///
/// Three legs, each measured:
///  - the full file name ("courier.rs") - always evidence.
///  - the stem of >= 3 chars, unless it is a ROLE_STEM.
///  - the stem with `_` read as a word break, so prose about "the event store"
///    counts for `event_store.rs`. Without this leg, tightening the doc lane took
///    event_store.rs from three chunks to zero: its underscored stem appears
///    verbatim in code but never in a sentence, and it was reachable only through
///    the generic symbol match that measured as noise elsewhere.
fn names_file(body_lower: &str, name_l: &str, stem_l: &str) -> bool {
    if body_lower.contains(name_l) {
        return true;
    }
    if stem_l.chars().count() < 3 || ROLE_STEMS.contains(&stem_l) {
        return false;
    }
    if body_lower.contains(stem_l) {
        return true;
    }
    stem_l.contains('_')
        && [" ", "-"].iter().any(|sep| body_lower.contains(&stem_l.replace('_', sep)))
}

/// Shortest token worth treating as a cited code name.
const STALE_LITERAL_MIN_CHARS: usize = 5;
/// How far back to look for a negation cue before a cited literal.
const NEGATION_WINDOW_CHARS: usize = 60;
/// Cues that turn "this fact cites X" into "this fact says X is gone". The one
/// surviving false-positive class in the measurement: the fact DOCUMENTS the
/// removal it is being warned about.
const NEGATION_CUES: &[&str] = &[
    "niet meer", "no longer", "instead of", "i.p.v.", "vervangt", "vervangen", "verwijderd",
    "removed", "replaced", "voorheen", "formerly", "used to", "dropped", "was ", "niet langer",
];

/// Code literals a fact CITES: tokens carrying an underscore.
///
/// The underscore is the whole trick, and it is measured rather than guessed
/// (2026-07-25, over the live store). A pattern that accepted any SHOUTED word
/// also caught Dutch emphasis - GOTCHA, AANLEIDING, DATAVERLIES - and fired on
/// 42% of (fact, file) pairs. Underscores are produced by code and almost never
/// by prose, so `MAX_HITS`, `TS_AUTHKEY` and `event_store` survive while the
/// emphasis words drop out. Returns each literal with its byte offset, because
/// the negation check needs to look at the text just before it.
fn cited_code_literals(body: &str) -> Vec<(usize, String)> {
    fn keep(out: &mut Vec<(usize, String)>, body: &str, s: usize, e: usize) {
        let tok = &body[s..e];
        if tok.chars().count() < STALE_LITERAL_MIN_CHARS
            || !tok.contains('_')
            || tok.starts_with('_')
            || tok.ends_with('_')
            || !tok.chars().any(|c| c.is_ascii_alphabetic())
        {
            return;
        }
        if !out.iter().any(|(_, t)| t == tok) {
            out.push((s, tok.to_string()));
        }
    }
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in body.char_indices() {
        match (c.is_ascii_alphanumeric() || c == '_', start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                keep(&mut out, body, s, i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        keep(&mut out, body, s, body.len());
    }
    out
}

/// Does the text just before a cited literal say it is GONE? Then the fact is
/// documenting the removal and warning about it would be backwards.
fn negated_before(body: &str, at: usize) -> bool {
    let window_start = body[..at]
        .char_indices()
        .rev()
        .take(NEGATION_WINDOW_CHARS)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let window = body[window_start..at].to_lowercase();
    NEGATION_CUES.iter().any(|cue| window.contains(cue))
}

/// TRANSITION EVIDENCE - the measured core of the stale-fact signal.
///
/// A literal missing from the file on disk means nothing on its own: most of
/// them were never in that file. It only counts when the literal ALSO appears in
/// an OLDER revision of that file's chunks, because then it was really taken
/// out. Nothing new has to be stored for this - the append-only log already
/// keeps every revision a chunk ever had.
///
/// Measured 2026-07-25 over 85 (fact, file) pairs: the naive "the anchored file
/// changed since the fact" fired on 75% of pairs (useless - a guard that fires
/// on three of four gets ignored within a day), the narrowed literal rule still
/// fired on 66%, and this one fired on 2.4%.
fn literal_was_removed(events: &[crate::event_store::Event], rel: &str, literal: &str) -> bool {
    let needle = format!(":{rel}#");
    events
        .iter()
        .any(|e| e.entity_id.contains(&needle) && e.body.contains(literal))
}

/// Read the touched file as ingest would have seen it. Compares against DISK,
/// never against the stored chunk: an ingest that has not caught up yet would
/// otherwise be reported as code drift.
fn read_touched_file(path: &Path) -> Option<String> {
    const MAX_BYTES: u64 = (crate::repo::MAX_FILE_CHARS as u64) * 4;
    match std::fs::metadata(path) {
        Ok(m) if m.len() > MAX_BYTES => return None,
        Ok(_) => {}
        Err(_) => return None,
    }
    let mut text = std::fs::read_to_string(path).ok()?;
    crate::repo::truncate_to_max_file_chars(&mut text);
    Some(text)
}

/// For each anchored fact, the code names it cites that this file once had and
/// no longer does. Advisory only - it never blocks and never removes a hit.
///
/// Ordered cheapest-first on purpose: the expensive step (folding the log for
/// this file's chunk history) only runs when a literal is actually missing from
/// disk, which the measurement puts at a few percent of touches.
fn vanished_citations(
    store: &crate::event_store::EventStore,
    hits: &[crate::recall::RecallHit],
    file_path: &str,
    cwd: Option<&str>,
) -> Vec<(String, String)> {
    if hits.is_empty() {
        return Vec::new();
    }
    let abs = Path::new(file_path);
    let Some(cwd) = cwd else { return Vec::new() };
    let Some(root) = crate::repo::project_root(Path::new(cwd)) else { return Vec::new() };
    let Ok(rel_path) = abs.strip_prefix(&root) else { return Vec::new() };
    let rel = rel_path.to_string_lossy().replace('\\', "/");
    let Some(current) = read_touched_file(abs) else { return Vec::new() };
    // Cheap pass: which cited names are absent from the file as it is now?
    let mut candidates: Vec<(String, String)> = Vec::new();
    for h in hits {
        for (at, lit) in cited_code_literals(&h.body) {
            if !current.contains(&lit) && !negated_before(&h.body, at) {
                candidates.push((h.entity_id.clone(), lit));
            }
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    // Only now pay for the history: absence alone is not evidence of removal.
    let events = match store.get_all_events() {
        Ok(e) => e,
        Err(_) => return Vec::new(), // fail-soft: no signal beats a wrong one
    };
    candidates.retain(|(_, lit)| literal_was_removed(&events, &rel, lit));
    candidates
}

/// "Has anything been written to the store since?", answered without opening
/// it: the newest mtime of the database and its write-ahead log, in
/// MILLISECONDS - seconds are too coarse, and not only in a fast test. A rule
/// corrected in the same second as the advisory that showed its old text is
/// exactly the moment this is for: you read the block, see it is wrong, and fix
/// it right then.
/// A syscall or two, against the alternative of a full store open plus recall
/// on a hot path that runs for every tool call. Fails HIGH (0 means "unknown"
/// only when both stats fail, and a caller comparing `<= seen` then re-checks) -
/// erring toward one extra recall rather than toward silence.
fn store_watermark(db: &Path) -> u64 {
    let secs = |p: std::path::PathBuf| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    };
    let wal = db.with_extension(
        db.extension().and_then(|e| e.to_str()).map(|e| format!("{e}-wal")).unwrap_or_default(),
    );
    secs(db.to_path_buf()).max(secs(wal))
}

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
        // Legacy shape (a bare number), written by an older binary earlier in
        // this same session: already advised, stay quiet.
        Some(v) if v.is_u64() => return None,
        // Already advised for this file this session - UNLESS the store has
        // been written to since (2026-07-30). Correcting a rule mid-session used
        // to be pointless on the file it governs: the debounce returned here,
        // before any recall could notice the text had changed, so the agent kept
        // working from the version it saw an hour ago. The watermark is the
        // store's own mtime, so the common case (nothing written since) still
        // costs one ledger read and no store open at all; only after a write
        // does the recall run again, and the content-fingerprint dedup then
        // makes sure only a fact that ACTUALLY changed gets served twice.
        Some(v) if v.get("wm").is_some() => {
            let seen = v.get("wm").and_then(|w| w.as_u64()).unwrap_or(0);
            if store_watermark(db) <= seen {
                return None;
            }
        }
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
    // An underscored stem is a code token, not a phrase: `event_store` appears
    // verbatim in source and NEVER in a sentence, so prose that discusses "the
    // event store" could not even enter the candidate pool - measured, after the
    // doc lane was tightened event_store.rs went to zero chunks while three doc
    // paragraphs about it existed. Adding the word form is a RECALL widening
    // only; names_file still decides what may be served.
    let stem_words = stem.replace('_', " ");
    if stem_words != stem {
        terms.push(&stem_words);
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

    // Keep only memories that literally NAME the file (see names_file) OR one of
    // the distinctive symbols this file defines - "mentions the directory" is not
    // "about this file", but "names a function that lives in it" is.
    let stem_l = stem.to_lowercase();
    let symbols_l: Vec<String> = symbols.iter().map(|s| s.to_lowercase()).collect();
    let heuristic: Vec<_> = hits
        .into_iter()
        .filter(|h| {
            let b = h.body.to_lowercase();
            names_file(&b, &name_l, &stem_l) || symbols_l.iter().any(|s| b.contains(s.as_str()))
        })
        .collect();
    let named = merge_anchored(anchored, heuristic);

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
    //
    // NO symbol leg here, unlike the memory filter above. Measured 2026-07-25
    // over every thor/src/*.rs + docs/*.md file in this repo: the doc-chunk
    // lane fired 103 times (23 by
    // name, 66 by stem, 14 by a defined symbol). Reading all 14 symbol-only
    // hits, at least 11 were tangential - a chunk merely sharing a generic
    // identifier fragment ("access", "before", "current", "existing",
    // "conflict") with the touched file, pulled in from an unrelated
    // paragraph - against 2-3 that were genuinely on topic. The name/stem
    // legs measured the opposite on the same run: touching strength.rs,
    // cas.rs or guard.rs served a genuinely relevant paragraph through stem
    // alone every time. Every fired item lands in the agent's context, so
    // this lane keeps the precise legs and drops the one that mostly wasn't.
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
            .filter(|h| names_file(&h.body.to_lowercase(), &name_l, &stem_l))
            .collect();
    // A paragraph that spells out the file NAME beats one reached via a stem
    // coincidence - "about printers.js" over "mentions printers in passing".
    // Stable sort keeps bm25 order within each class.
    doc_chunks.sort_by_key(|h| !h.body.to_lowercase().contains(&name_l));
    doc_chunks.truncate(DOC_CHUNK_HITS);

    // A TRUE MISS AND AN ALREADY-SHOWN SET ARE NOT THE SAME THING, and only the
    // first may be cached as one (split 2026-07-30, after an adversarial
    // verifier caught the file lane still conflating them - the command lane
    // below had the split, this one never got it). "Nothing in the store names
    // this file" is worth remembering for a quarter of an hour. "I already said
    // this earlier in the session" is not: cache it and a fact stored a minute
    // later stays silent for fifteen, on a file the store demonstrably knows.
    // So the question is asked BEFORE the dedup, on what actually matched.
    let nothing_matches = named.is_empty() && doc_chunks.is_empty();
    let named = dedup_then_cap(db, session_id, "file", named, FILE_MEMORY_HITS);

    if nothing_matches {
        // Cache the miss briefly (NEG_CACHE_SECS) so repeated touches of a
        // memory-less file stop re-paying the recall; a memory stored later
        // still surfaces once the negative entry expires. Per-key upsert: a
        // concurrent guard on ANOTHER file can no longer lose this entry.
        let now = crate::review::now_secs();
        crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!({ "ts": now, "neg": true }));
        return None;
    }
    if named.is_empty() && doc_chunks.is_empty() {
        // Everything that matched was already served this session: stay quiet,
        // and cache NOTHING. Deliberate, and it is the more expensive branch:
        // the next touch of this file re-pays the recall. The alternative costs
        // more - a negative entry would silence a fact stored a minute from now
        // for a quarter of an hour on a file the store demonstrably knows, and
        // a positive one would silence it for the whole session. This branch
        // only fires when every match was already shown, so the repeated cost
        // is bounded by how often that one file is touched.
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
    // Carries the watermark, so the next touch of this file can tell "nothing
    // has happened since" from "the store was written to - look again".
    crate::ledger::upsert(
        db,
        "guard-seen",
        &key,
        &serde_json::json!({ "ts": now, "wm": store_watermark(db) }),
    );
    record_advised(db, session_id, "file", &named, now);

    // Stale-citation note. Only for the ANCHORED facts: their author declared
    // this file, so a code name they cite that this file once had and no longer
    // has is worth a word. Advisory, never blocking, and it never removes a hit
    // - the fact may still be right about everything else it says.
    let stale = vanished_citations(&store, &named, file_path, hook.get("cwd").and_then(|v| v.as_str()));
    let mut out = format!(
        "stored memory about this file (verify before relying): {}",
        lines.join("  ||  ")
    );
    if !stale.is_empty() {
        let notes: Vec<String> = stale
            .iter()
            .map(|(id, lit)| format!("{id} cites `{lit}`"))
            .collect();
        out.push_str(&format!(
            "  ||  HEADS UP - this file no longer contains what these facts name, and it did \
             once: {}. Check them against the file before you build on them; they may simply \
             be out of date.",
            notes.join("; ")
        ));
    }
    Some(out)
}

/// One serving per (session, entity, surface) ACROSS advisory keys. The
/// per-key debounces stop identical repeats, but the same FACT is reachable
/// through many keys: three consecutive, slightly-different version-bump
/// commands produced three different token-sets and served the version-stamp
/// rule three times as a full block (measured, eval #6, 2026-07-24), and a
/// broadly-anchored fact re-fires on every command its anchor happens to hit.
/// Entities this session has already been advised on (on the SAME surface -
/// file and command stay independent, an anchor may legitimately fire once on
/// each) are dropped; the entity keys live under the same "{session}|" prefix,
/// so the post-compaction clear re-arms them exactly when the context that
/// held the advisory text is gone. Memories only - doc chunks keep their own
/// per-file behavior.
/// PER CONTENT, not per entity (2026-07-30). Keying on the id alone meant a
/// fact REVISED after its first serving stayed silent for the rest of the
/// session - including on a different file it had never served for. The agent
/// then kept acting on the version it saw an hour ago, which is the stale-fact
/// failure this whole channel exists to prevent. The key carries a fingerprint
/// of the fact's CONTENT (footer stripped), so a corrected body re-arms exactly
/// once, while an anchor fix, a tag sweep or an expiry change - metadata edits
/// that leave the text alone - stay deduped and cost nothing.
///
/// THE BOUNDARY, narrower than it first reads (spelled out after an adversarial
/// verifier called the wording bigger than the code): the per-(session, target)
/// debounce at the top of each advisory returns before any of this runs, so a
/// correction does NOT re-fire on a target that already had its one advisory
/// this session. What this reaches is the fact travelling to a target it has
/// not served for yet - the case where the agent holds the stale text and the
/// corrected one has no other way in. Widening it further would mean reopening
/// that debounce, which is a noise decision, not a bug fix.
fn content_fingerprint(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(crate::footer::strip(body).trim().as_bytes());
    format!("{:x}", h.finalize())[..12].to_string()
}

fn drop_already_advised(
    db: &Path,
    session_id: &str,
    surface: &str,
    hits: Vec<crate::recall::RecallHit>,
) -> Vec<crate::recall::RecallHit> {
    hits.into_iter()
        .filter(|h| {
            crate::ledger::get(
                db,
                "guard-seen",
                &format!(
                    "{session_id}|ent-{surface}:{}:{}",
                    h.entity_id,
                    content_fingerprint(&h.body)
                ),
            )
            .is_none()
        })
        .collect()
}

fn record_advised(
    db: &Path,
    session_id: &str,
    surface: &str,
    hits: &[crate::recall::RecallHit],
    now: u64,
) {
    for h in hits {
        crate::ledger::upsert(
            db,
            "guard-seen",
            &format!(
                "{session_id}|ent-{surface}:{}:{}",
                h.entity_id,
                content_fingerprint(&h.body)
            ),
            &serde_json::json!(now),
        );
    }
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
    // Expiry holds on this surface too: recall filters expired facts at rank
    // time, but this pass folds heads directly, so without its own check an
    // expired report with an anchor kept firing at the moment of action
    // forever - measured live 2026-07-26 (retro-anchored milestones whose
    // expiry had already passed). Same strict compare as recall: expired
    // means expires < today, and get/history still serve the fact in full.
    let today = crate::footer::today();
    let expired =
        |body: &str| crate::footer::expires(body).is_some_and(|d| d.as_str() < today.as_str());
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
                if expired(&head.body) {
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
        if expired(&head.body) {
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
/// them - the shared merge for both advisories. UNCAPPED on purpose since
/// 2026-07-30: the cap has to be applied AFTER `drop_already_advised`, never
/// before, or facts that were already served this session hold slots they no
/// longer need and starve one that was never shown (see the call sites).
fn merge_anchored(
    anchored: Vec<crate::recall::RecallHit>,
    heuristic: Vec<crate::recall::RecallHit>,
) -> Vec<crate::recall::RecallHit> {
    let mut out = anchored;
    for h in heuristic {
        if !out.iter().any(|a| a.entity_id == h.entity_id) {
            out.push(h);
        }
    }
    out
}

/// Drop what this session already saw, THEN cap - the order the two steps have
/// to run in, kept in one place so a call site cannot get it wrong again.
///
/// The bug this ends (found by an audit swarm 2026-07-30, present since the
/// dedup landed): capping first meant the slots were handed out by entity id
/// among ALL matching facts, and only afterwards were the already-served ones
/// removed. Three facts anchored to one file, two of them served earlier in the
/// session, left an EMPTY advisory - the third, never shown, had been truncated
/// away before the filter ever saw it. Worse, the file advisory then wrote a
/// "no memory names this file" negative cache entry, asserting the opposite of
/// the truth for the next quarter of an hour.
fn dedup_then_cap(
    db: &Path,
    session_id: &str,
    surface: &str,
    hits: Vec<crate::recall::RecallHit>,
    cap: usize,
) -> Vec<crate::recall::RecallHit> {
    let mut out = drop_already_advised(db, session_id, surface, hits);
    out.truncate(cap);
    out
}

/// Eval seam: lets a drift-eval harness of your own drive the
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
    let named = merge_anchored(anchored, heuristic);
    let now = crate::review::now_secs();
    if named.is_empty() {
        // Nothing matches this command at all - a true miss, worth caching.
        // Checked on the UNCAPPED merge, so this can never mean "the cap hid
        // everything" the way it could if the cap ran first.
        crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!({ "ts": now, "neg": true }));
        return None;
    }
    // Cross-key dedup: the same fact must not re-serve as a full block on
    // every slightly-different command (see drop_already_advised). Cap AFTER
    // that filter - see dedup_then_cap.
    let named = dedup_then_cap(db, session_id, "cmd", named, FILE_MEMORY_HITS);
    if named.is_empty() {
        // Everything relevant was already advised this session under another
        // key - remember THIS key too so the next identical call exits early.
        crate::ledger::upsert(db, "guard-seen", &key, &serde_json::json!(now));
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
    record_advised(db, session_id, "cmd", &named, now);
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
    // Auto-echo settlement runs at every Stop (mid-session is where honest
    // judgment lives - the transcript is still present); idempotent per
    // (session, entity), so repeated stops cost only the check.
    auto_echo(db, hook.get("session_id").and_then(|v| v.as_str()).unwrap_or(""));

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
    } else if let Some(r) = hygiene_gate(db, &haystack) {
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

// ---- Weekly hygiene gate ------------------------------------------------------

/// How long the store may go without its keeper hearing about the cleanup list.
const HYGIENE_INTERVAL_SECS: u64 = 7 * 24 * 3600;

/// Once a week: hold the stop until the reply has TOLD the user what maintenance
/// the store needs.
///
/// Why a gate and not a reminder. Measured three times on this user's own
/// sessions: a rule that only lives in an injected block is ignored - the
/// no-background-poll rule was in the store AND injected and was still broken;
/// the mark nudge sat in every recall block for a whole session unread; the
/// post-compaction advisory was skipped entirely next to one user question.
/// Hook output is background data to a model, and background always loses to the
/// user's actual question. The one thing that DID change behaviour was a Stop
/// gate: the plain-language-TLDR rule blocked twice and was obeyed ever after.
/// So maintenance that must reach the user is a gate, not a note.
///
/// The clock lives in the ledger, not in the session or the model: an agent
/// cannot forget it, and skipping a week is impossible because the timer only
/// advances when a reply actually carried the report. Fires at most once per
/// week, is satisfied by one sentence, and stays silent when there is nothing to
/// report. `THOR-SILENT.flag` switches it off with everything else.
fn hygiene_gate(db: &Path, haystack_lower: &str) -> Option<String> {
    if crate::ledger::flag_present(db, "THOR-SILENT.flag")
        || crate::ledger::flag_present(db, "THOR-NO-HYGIENE-GATE.flag")
    {
        return None;
    }
    let now = crate::review::now_secs();
    let last = crate::ledger::get(db, "hygiene", "last-reported")
        .and_then(|v| v.get("ts").and_then(|t| t.as_u64()))
        .unwrap_or(0);
    if last != 0 && now.saturating_sub(last) < HYGIENE_INTERVAL_SECS {
        return None;
    }
    // Only ask when there IS something: an empty worklist must never cost a turn.
    let events = crate::event_store::EventStore::new(db).ok()?.get_all_events().ok()?;
    let counts = crate::consolidate::worklist_counts(&events);
    if counts.total() == 0 {
        // Nothing to report is itself a week served: reset the clock so a clean
        // store does not re-check the whole log on every stop for seven days.
        crate::ledger::upsert(db, "hygiene", "last-reported", &serde_json::json!({ "ts": now }));
        return None;
    }
    // The reply already told them - accept it and start the next week. The word
    // alone is not enough: a bare substring check accepted "niets over
    // onderhoud" ("nothing about maintenance") as a report, which is the exact
    // opposite sentence. Requiring a DIGIT alongside it asks for what the gate
    // is actually after - how many items - and a denial rarely carries one.
    if ["hygiene", "opruim", "consolidate", "onderhoud", "cleanup", "maintenance"]
        .iter()
        .any(|w| haystack_lower.contains(w))
        && haystack_lower.chars().any(|c| c.is_ascii_digit())
    {
        crate::ledger::upsert(db, "hygiene", "last-reported", &serde_json::json!({ "ts": now }));
        return None;
    }
    Some(format!(
        "[THOR hygiene] It has been a week since the store's maintenance list was reported to the \
         user, and it is not empty: {}. Before finishing, tell them in one plain sentence WITH THE \
         COUNT what is on it and offer to work through it - `thor consolidate` prints the detail. \
         Say it in your own words; naming the cleanup and how many items is what satisfies this \
         check, and it then stays quiet for another week.",
        counts.summary()
    ))
}

// ---- Auto-echo experiment (THOR-EXP-AUTO-ECHO.flag) ---------------------------

/// Cap on the per-session touched list: a bounded, predictable sidecar row
/// (same philosophy as the courier's SERVED_CAP).
const TOUCHED_CAP: usize = 200;
/// Per-item char bound: a path fits whole, a long command line is truncated.
const TOUCHED_ITEM_CHARS: usize = 300;

/// Record what this tool call touches (file path, command line) into the
/// session's ledger row, lowercase and deduplicated. Gated on the experiment
/// flag so default THOR never pays a ledger write per tool call. Fail-open.
fn record_touched(db: &Path, hook: &Value, input: &Value) {
    if !crate::ledger::flag_present(db, "THOR-EXP-AUTO-ECHO.flag") {
        return;
    }
    let session = hook.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
    if session.is_empty() {
        return;
    }
    let mut items: Vec<String> = Vec::new();
    if let Some(p) = input.get("file_path").and_then(|v| v.as_str()) {
        items.push(
            p.replace('\\', "/").to_lowercase().chars().take(TOUCHED_ITEM_CHARS).collect(),
        );
    }
    if let Some(c) = input.get("command").and_then(|v| v.as_str()) {
        items.push(c.to_lowercase().chars().take(TOUCHED_ITEM_CHARS).collect());
    }
    items.retain(|i| !i.trim().is_empty());
    if items.is_empty() {
        return;
    }
    let mut list: Vec<String> = crate::ledger::get(db, "touched", session)
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let before = list.len();
    for item in items {
        if list.len() >= TOUCHED_CAP {
            break;
        }
        if !list.contains(&item) {
            list.push(item);
        }
    }
    if list.len() > before {
        crate::ledger::upsert(db, "touched", session, &serde_json::json!(list));
    }
}

/// Auto-echo settlement (THOR-EXP-AUTO-ECHO.flag): at the Stop moment, compare
/// every memory the courier SERVED this session (the judgment-debt list)
/// against what the session actually TOUCHED, and on STRONG evidence only
/// append a fact_echoed event labeled auto. Evidence = a declared anchor
/// matching a touched item, or >= 2 distinct fires-when words appearing in the
/// touched set - deliberate metadata only, never body-term overlap: a guessed
/// echo pollutes the very signal this feeds (the S2-A lesson from the gate
/// A/B). At most one auto-echo per (session, entity), claimed atomically
/// BEFORE the append. Skipped entirely on a replica (THOR_CAPTURE_INBOX set):
/// a direct append there would fork the shipped log. The events count HALF and
/// only while the flag is present (see strength.rs) - deleting the flag makes
/// every auto event retroactively inert, the kill-switch that needs no store
/// restore.
fn auto_echo(db: &Path, session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    if !crate::ledger::flag_present(db, "THOR-EXP-AUTO-ECHO.flag") {
        return;
    }
    if std::env::var("THOR_CAPTURE_INBOX").is_ok() {
        return;
    }
    let touched: Vec<String> = match crate::ledger::get(db, "touched", session_id)
        .and_then(|v| serde_json::from_value(v).ok())
    {
        Some(t) => t,
        None => return,
    };
    if touched.is_empty() {
        return;
    }
    let served: Vec<String> = crate::ledger::get(db, "courier-seen", session_id)
        .and_then(|entry| {
            entry
                .get("served")
                .and_then(|v| v.as_object())
                .map(|m| m.keys().cloned().collect())
        })
        .unwrap_or_default();
    if served.is_empty() {
        return;
    }
    let Ok(mut store) = crate::event_store::EventStore::new(db) else { return };
    let now = crate::review::now_secs();
    for entity in served {
        let Some(body) = crate::courier::live_head_body(&store, &entity) else { continue };
        let Some(evidence) = echo_evidence(&body, &touched) else { continue };
        // Atomic once-per-(session, entity) claim: two concurrent Stop hooks
        // both computing evidence append at most one event.
        let key = format!("{}|{}", session_id, entity);
        if !crate::ledger::insert_once(db, "auto-echoed", &key, &serde_json::json!(now)) {
            continue;
        }
        let _ = store.append_event(
            "guard",
            session_id,
            "auto-echo",
            crate::event_store::EventKind::FactEchoed,
            &entity,
            None,
            &format!("auto-echo: {evidence}"),
        );
    }
}

/// The strong-evidence matcher behind auto_echo. Returns a short evidence
/// string (lands in the event body after the "auto-echo: " label) or None.
/// pub: the replay harness (examples/echo_replay.rs) measures THIS function
/// against hand-labeled sessions - never a reimplementation.
pub fn echo_evidence(body: &str, touched: &[String]) -> Option<String> {
    // Anchors: the fact's own exact file paths / command strings. A touched
    // item CONTAINING the anchor counts (a command line contains "git push";
    // an absolute touched path contains the anchored relative path).
    for anchor in crate::footer::anchors(body) {
        let a = anchor.replace('\\', "/").to_lowercase();
        if a.chars().count() < 3 {
            continue; // a 1-2 char anchor would match everything
        }
        if touched.iter().any(|t| t.contains(&a)) {
            return Some(format!("anchor '{anchor}' touched this session"));
        }
    }
    // fires-when: DISTINCT declared words (>= 4 chars each, so shell glue like
    // "git"/"run" never counts) found among the touched items, with two
    // measured filters. AMBIENT (first hand-labeled replay, 2026-07-24): a
    // word appearing in MORE THAN HALF the touched items ('thor' is in every
    // path of this project, '--release' in every cargo line) is the session's
    // background vocabulary, not evidence. THRESHOLD (48-event live simulation,
    // same day): two generic words leak badly in long sessions - 'werk'+'work',
    // 'release'+'versie', 'model'+'proxy' all fired falsely, while every
    // hand-confirmed echo carried >= 3 words or an identifier-shaped one
    // (deploy-watcher.sh, staging-box). So evidence = three distinct words, or a
    // single identifier-like word (>= 8 chars containing . / _ -), which is a
    // near-anchor and vouches on its own.
    if let Some(fw) = crate::footer::fires_when(body) {
        let mut hits: Vec<String> = Vec::new();
        for word in fw.split_whitespace() {
            let wl = word.to_lowercase();
            if wl.chars().count() < 4 || hits.contains(&wl) {
                continue;
            }
            // Stopwords are never evidence (measured 2026-07-25: a fact with
            // 'niet'/'meer' in its fires-when echoed on those alone - Dutch
            // function words clear the 4-char bar that filters English glue).
            if crate::recall::STOPWORDS.contains(&wl.as_str()) {
                continue;
            }
            let matches = touched.iter().filter(|t| t.contains(&wl)).count();
            if matches > 0 && matches * 2 <= touched.len() {
                hits.push(wl);
            }
        }
        let identifier_like =
            |w: &str| w.chars().count() >= 8 && w.contains(['.', '/', '_', '-']);
        if hits.len() >= 3 || hits.iter().any(|w| identifier_like(w)) {
            return Some(format!("fires-when '{}' touched this session", hits.join("' + '")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The underscore rule is the whole reason this signal is usable. An earlier
    /// pattern accepted any shouted word and swallowed Dutch emphasis, which put
    /// the fire rate at 42% of pairs and made it noise.
    #[test]
    fn cited_literals_take_code_names_and_leave_shouted_prose_alone() {
        let body = "GOTCHA: DATAVERLIES dreigt - MAX_HITS staat op 3 en event_store.rs leest \
                    TS_AUTHKEY, maar AANLEIDING was iets anders";
        let got: Vec<String> = cited_code_literals(body).into_iter().map(|(_, l)| l).collect();
        assert!(got.contains(&"MAX_HITS".to_string()), "a constant is a code name: {got:?}");
        assert!(got.contains(&"TS_AUTHKEY".to_string()), "{got:?}");
        assert!(got.contains(&"event_store".to_string()), "snake_case counts too: {got:?}");
        for shouted in ["GOTCHA", "DATAVERLIES", "AANLEIDING"] {
            assert!(
                !got.iter().any(|l| l == shouted),
                "{shouted} is emphasis, not code - it has no underscore: {got:?}"
            );
        }
    }

    /// The one surviving false positive in the measurement: a fact that
    /// documents the removal it would be warned about.
    #[test]
    fn a_fact_that_says_the_name_is_gone_is_not_warned_about() {
        let live = "server.js reads MAINSAIL_PUBLIC_ORIGIN at boot";
        let removed = "server.js no longer uses MAINSAIL_PUBLIC_ORIGIN, the value moved to env";
        let at = |b: &str| cited_code_literals(b).into_iter().next().unwrap().0;
        assert!(!negated_before(live, at(live)), "a plain citation is not a removal note");
        assert!(negated_before(removed, at(removed)), "'no longer' means the fact knows");
    }

    /// End to end: an anchored fact citing a name the file once had and no
    /// longer has comes back as a warning - and the same fact stops warning the
    /// moment the name is back in the file. The positive control for a signal
    /// whose whole point is to stay quiet: silence has to mean "nothing wrong",
    /// not "wired up wrong".
    #[test]
    fn a_vanished_citation_is_reported_and_a_present_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".thor"), "proj\n").unwrap();
        let file = root.join("src").join("server.js");

        let mut store = crate::event_store::EventStore::new(&db).unwrap();
        // The file DID carry the constant once - that is the transition evidence.
        store
            .append_event(
                "s", "l", "a", crate::event_store::EventKind::FactCreated,
                "proj:src/server.js#0", None, "const MAINSAIL_PUBLIC_ORIGIN = 'x';",
            )
            .unwrap();
        let hit = crate::recall::RecallHit {
            entity_id: "proj:mem-cites".into(),
            rev: "r1".into(),
            body: "the origin comes from MAINSAIL_PUBLIC_ORIGIN in src/server.js".into(),
            kind: crate::event_store::EventKind::FactCreated,
            is_diverged: false,
            rank: 0.0,
            project: Some("proj".into()),
            fact_type: None,
            matched_and: true,
        };
        let cwd = root.to_string_lossy().to_string();
        let path = file.to_string_lossy().to_string();

        // Code changed: the constant is gone from disk -> warn.
        std::fs::write(&file, "const origin = process.env.ORIGIN;\n").unwrap();
        let gone = vanished_citations(&store, std::slice::from_ref(&hit), &path, Some(&cwd));
        assert_eq!(
            gone,
            vec![("proj:mem-cites".to_string(), "MAINSAIL_PUBLIC_ORIGIN".to_string())],
            "the fact names something this file demonstrably used to have"
        );

        // Same fact, constant still there -> silence.
        std::fs::write(&file, "const MAINSAIL_PUBLIC_ORIGIN = 'x';\n").unwrap();
        assert!(
            vanished_citations(&store, std::slice::from_ref(&hit), &path, Some(&cwd)).is_empty(),
            "nothing vanished, so there is nothing to say"
        );
    }

    /// Transition evidence: absence from the file today proves nothing on its
    /// own. Measured 2026-07-25 - requiring that an OLDER chunk revision had the
    /// literal took the fire rate from 75% to 2.4% with no new storage, because
    /// the append-only log already keeps every revision.
    #[test]
    fn a_literal_counts_as_removed_only_if_this_file_once_had_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut store = crate::event_store::EventStore::new(&db).unwrap();
        store
            .append_event(
                "s", "l", "a", crate::event_store::EventKind::FactCreated,
                "P:src/server.js#0", None, "const MAINSAIL_PUBLIC_ORIGIN = 1;",
            )
            .unwrap();
        let events = store.get_all_events().unwrap();
        assert!(
            literal_was_removed(&events, "src/server.js", "MAINSAIL_PUBLIC_ORIGIN"),
            "this file demonstrably carried it once"
        );
        assert!(
            !literal_was_removed(&events, "src/server.js", "NEVER_WAS_HERE"),
            "a name this file never had cannot have been taken out of it"
        );
        assert!(
            !literal_was_removed(&events, "src/other.js", "MAINSAIL_PUBLIC_ORIGIN"),
            "history is per file - another file's chunks are not evidence"
        );
    }

    #[test]
    fn auto_echo_fires_on_anchor_overlap_once_and_only_with_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            let body = format!(
                "push-stop regel: niet pushen zonder go\n\n{}",
                crate::footer::compose_full(
                    "preference", &[], "global", &[], &["git push".into()], None, None,
                )
            );
            store
                .append_event(
                    "s", "l", "a", crate::event_store::EventKind::FactCreated,
                    "mem-anchored", None, &body,
                )
                .unwrap();
        }
        // The courier's judgment-debt record says this memory was served.
        crate::ledger::upsert(
            &db, "courier-seen", "sess-ae",
            &json!({"ts": 0, "count": 1, "seen": {}, "served": {"mem-anchored": "push-stop regel"}}),
        );
        let hook = json!({"session_id": "sess-ae"});
        let input = json!({"command": "git push origin main"});
        let count_echoes = |db: &Path| {
            let store = crate::event_store::EventStore::new(db).unwrap();
            let evs = store.get_events_by_entity("mem-anchored").unwrap();
            evs.iter()
                .filter(|e| matches!(e.kind, crate::event_store::EventKind::FactEchoed))
                .map(|e| e.body.clone())
                .collect::<Vec<_>>()
        };

        // Without the flag: nothing recorded, nothing echoed.
        record_touched(&db, &hook, &input);
        auto_echo(&db, "sess-ae");
        assert!(count_echoes(&db).is_empty(), "no flag = no echo");

        std::fs::write(dir.path().join("THOR-EXP-AUTO-ECHO.flag"), "x").unwrap();
        record_touched(&db, &hook, &input);
        auto_echo(&db, "sess-ae");
        auto_echo(&db, "sess-ae"); // second stop: idempotent
        let echoes = count_echoes(&db);
        assert_eq!(echoes.len(), 1, "exactly one auto-echo per (session, entity)");
        assert!(echoes[0].starts_with("auto-echo:"), "labeled auto: {}", echoes[0]);
        assert!(echoes[0].contains("git push"), "evidence names the anchor: {}", echoes[0]);
    }

    /// The 2026-07-24 tightening: 48 live echoes simulated, every known-false
    /// one carried exactly two generic words; every hand-confirmed one carried
    /// three-plus or an identifier. The matcher now demands that.
    #[test]
    fn echo_evidence_needs_three_words_or_an_identifier() {
        let body = |fw: &[&str]| {
            format!(
                "feit\n\n{}",
                crate::footer::compose_full(
                    "gotcha", &[], "global",
                    &fw.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    &[], None, None,
                )
            )
        };
        let touched =
            vec!["cargo test build release".to_string(), "deploy-watcher.sh gestart".to_string()];
        // Two generic words: the measured leak class ('werk'+'work') - silent.
        assert!(echo_evidence(&body(&["test", "build"]), &touched).is_none());
        // Stopwords never count, however many match (the 'niet'+'meer' leak).
        let stop_touched = vec!["dit werkt niet meer voor deze release build test".to_string()];
        assert!(echo_evidence(&body(&["niet", "meer", "deze", "voor"]), &stop_touched).is_none());
        // Three distinct words: evidence.
        assert!(echo_evidence(&body(&["test", "build", "release"]), &touched).is_some());
        // One identifier-like word vouches on its own.
        assert!(echo_evidence(&body(&["deploy-watcher.sh", "elders"]), &touched).is_some());
    }

    #[test]
    fn auto_echo_needs_strong_evidence_not_one_word() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            // fires-when: "deploy watcher" - only "deploy" will be touched.
            let body = format!(
                "de watcher pullt niet zelf\n\n{}",
                crate::footer::compose_full(
                    "gotcha", &[], "global",
                    &["deploy".into(), "watcher".into()], &[], None, None,
                )
            );
            store
                .append_event(
                    "s", "l", "a", crate::event_store::EventKind::FactCreated,
                    "mem-fw", None, &body,
                )
                .unwrap();
        }
        crate::ledger::upsert(
            &db, "courier-seen", "sess-weak",
            &json!({"ts": 0, "count": 1, "seen": {}, "served": {"mem-fw": "de watcher pullt"}}),
        );
        std::fs::write(dir.path().join("THOR-EXP-AUTO-ECHO.flag"), "x").unwrap();
        record_touched(
            &db,
            &json!({"session_id": "sess-weak"}),
            &json!({"command": "cargo build && start deploy"}),
        );
        auto_echo(&db, "sess-weak");
        let store = crate::event_store::EventStore::new(&db).unwrap();
        let evs = store.get_events_by_entity("mem-fw").unwrap();
        assert!(
            !evs.iter().any(|e| matches!(e.kind, crate::event_store::EventKind::FactEchoed)),
            "one matching fires-when word is not evidence"
        );
    }

    /// The measured eval-#6 repetition (2026-07-24): the same fact served as a
    /// full block on three consecutive, slightly-different commands - each got
    /// its own token-set key, so the per-key debounce never fired. An entity
    /// now serves once per (session, surface), whatever key reaches it.
    /// Expiry silences the anchored pass too: an expired report with an anchor
    /// must not fire at the moment of action (measured live 2026-07-26 -
    /// retro-anchored milestones whose date had already passed kept serving,
    /// because this fold never read the expires field recall filters on).
    #[test]
    fn anchored_pass_skips_an_expired_fact() {
        use crate::event_store::{EventKind, EventStore};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            s.append_event(
                "s", "l", "a", EventKind::FactCreated, "P:mem-expired", None,
                "old report\n\n[memory/decision | tags: x | anchors: deploy/watcher.sh | \
                 expires: 2020-01-01 | project: P]",
            )
            .unwrap();
            s.append_event(
                "s", "l", "a", EventKind::FactCreated, "P:mem-live", None,
                "the live rule\n\n[memory/gotcha | tags: x | anchors: deploy/watcher.sh | \
                 project: P]",
            )
            .unwrap();
            s.append_event(
                "s", "l", "a", EventKind::FactCreated, "P:mem-future", None,
                "valid until far away\n\n[memory/gotcha | tags: x | anchors: deploy/watcher.sh | \
                 expires: 2999-01-01 | project: P]",
            )
            .unwrap();
        }
        let store = EventStore::new(&db).unwrap();
        let scope = crate::recall::RecallScope::current(Some("P".to_string()));
        let hits = anchored_memories(&store, &scope, &|a| a == "deploy/watcher.sh");
        let mut ids: Vec<&str> = hits.iter().map(|h| h.entity_id.as_str()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["P:mem-future", "P:mem-live"],
            "expired is skipped, future-dated still serves"
        );
    }

    #[test]
    fn advisory_serves_an_entity_once_per_session_across_different_commands() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            let body = format!(
                "versie-stempel regel: bump alle vier de doelen\n\n{}",
                crate::footer::compose_full(
                    "decision", &[], "global", &[], &["set-version".into()], None, None,
                )
            );
            store
                .append_event(
                    "s", "l", "a", crate::event_store::EventKind::FactCreated,
                    "mem-stamp", None, &body,
                )
                .unwrap();
        }
        let hook = |cmd: &str| {
            json!({
                "tool_name": "Bash",
                "session_id": "sess-rep",
                "tool_input": { "command": cmd }
            })
        };
        let first = command_memory_advisory(&db, &hook("npm run set-version 0.9.117"));
        assert!(
            first.as_deref().is_some_and(|a| a.contains("mem-stamp")),
            "first command serves the anchored fact: {first:?}"
        );
        // Different command, different token-set key, SAME fact: silent now.
        let second = command_memory_advisory(&db, &hook("node scripts/set-version.js 0.9.118 --verify"));
        assert!(
            second.is_none(),
            "a slightly different command must not re-serve the same fact: {second:?}"
        );
    }

    /// The starvation this ends (found by an audit swarm, 2026-07-30): the cap
    /// used to be applied BEFORE the already-served filter, so facts that had
    /// nothing left to say held the slots and a fact that had never been shown
    /// was truncated away before the filter could make room for it. Two facts
    /// cover both files, a third covers only the second: after the first file
    /// is advised, touching the second must serve that third fact - not an
    /// empty advisory, and above all not a "no memory names this file" cache
    /// entry, which is the opposite of the truth.
    #[test]
    fn a_never_served_fact_is_not_starved_by_two_already_served_ones() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            // ids sort a < b < c, so the two broad facts win the anchor pass.
            for (id, anchors) in [
                ("mem-a", vec!["src/one.rs".to_string(), "src/two.rs".to_string()]),
                ("mem-b", vec!["src/one.rs".to_string(), "src/two.rs".to_string()]),
                ("mem-c", vec!["src/two.rs".to_string()]),
            ] {
                let body = format!(
                    "constraint {id}: read this before editing\n\n{}",
                    crate::footer::compose_full(
                        "gotcha", &[], "global", &[], &anchors, None, None,
                    )
                );
                store
                    .append_event(
                        "s", "l", "a", crate::event_store::EventKind::FactCreated, id, None, &body,
                    )
                    .unwrap();
            }
        }
        let hook = |path: &str| {
            json!({
                "tool_name": "Read",
                "session_id": "sess-starve",
                "tool_input": { "file_path": path }
            })
        };
        let first = file_memory_advisory(&db, &hook("src/one.rs"));
        let first = first.expect("the first file serves its two anchored facts");
        assert!(first.contains("mem-a") && first.contains("mem-b"), "{first}");

        let second = file_memory_advisory(&db, &hook("src/two.rs"))
            .expect("the second file still has a fact that was never served");
        assert!(
            second.contains("mem-c"),
            "the never-served fact must get the slot the already-served ones freed: {second}"
        );
    }

    /// The cap still bites, and it bites AFTER the filter: with more survivors
    /// than slots, exactly FILE_MEMORY_HITS are served and they are the
    /// never-served ones. Without this the reorder could have been written as
    /// "drop the cap entirely" and no test would have noticed.
    #[test]
    fn the_cap_still_applies_once_the_already_served_are_filtered_out() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            for (id, anchors) in [
                ("mem-a", vec!["src/one.rs".to_string(), "src/two.rs".to_string()]),
                ("mem-b", vec!["src/one.rs".to_string(), "src/two.rs".to_string()]),
                ("mem-c", vec!["src/two.rs".to_string()]),
                ("mem-d", vec!["src/two.rs".to_string()]),
                ("mem-e", vec!["src/two.rs".to_string()]),
            ] {
                let body = format!(
                    "constraint {id}: read this before editing\n\n{}",
                    crate::footer::compose_full("gotcha", &[], "global", &[], &anchors, None, None)
                );
                store
                    .append_event(
                        "s", "l", "a", crate::event_store::EventKind::FactCreated, id, None, &body,
                    )
                    .unwrap();
            }
        }
        let hook = |path: &str| {
            json!({
                "tool_name": "Read",
                "session_id": "sess-cap",
                "tool_input": { "file_path": path }
            })
        };
        file_memory_advisory(&db, &hook("src/one.rs")).expect("first file serves a and b");
        let second =
            file_memory_advisory(&db, &hook("src/two.rs")).expect("three never-served remain");
        let served = ["mem-c", "mem-d", "mem-e"]
            .iter()
            .filter(|id| second.contains(**id))
            .count();
        assert_eq!(served, FILE_MEMORY_HITS, "the cap holds after the filter: {second}");
        assert!(
            !second.contains("mem-a") && !second.contains("mem-b"),
            "the already-served pair stays out: {second}"
        );
    }

    /// A REVISED fact must be able to speak again (2026-07-30, found by an
    /// audit swarm). The dedup keyed on the entity alone, so a fact corrected
    /// after its first serving stayed silent for the rest of the session - the
    /// agent kept working from the version it had already seen, which is the
    /// exact stale-fact failure this channel exists to prevent. A CONTENT change
    /// re-arms it once; a metadata-only edit does not.
    ///
    /// The BOUNDARY, deliberate: the same file still gets at most one advisory
    /// per session (the outer per-(session, file) debounce, which this does not
    /// touch). What is fixed is the fact travelling to a DIFFERENT target it
    /// never served for - the case where the agent has the stale text and the
    /// corrected one has no way in.
    #[test]
    fn a_revised_fact_speaks_again_but_a_metadata_edit_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let anchors = vec!["src/one.rs".to_string(), "src/two.rs".to_string()];
        let body = |text: &str, tags: &[String]| {
            format!(
                "{text}\n\n{}",
                crate::footer::compose_full("gotcha", tags, "global", &[], &anchors, None, None)
            )
        };
        let head_of = |db: &std::path::Path| {
            let store = crate::event_store::EventStore::new(db).unwrap();
            let events = store.get_all_events().unwrap();
            crate::cas::compute_head_sets(&events)
                .get("mem-r")
                .unwrap()
                .heads
                .iter()
                .next()
                .unwrap()
                .clone()
        };
        {
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            store
                .append_event(
                    "s", "l", "a", crate::event_store::EventKind::FactCreated, "mem-r", None,
                    &body("the old wording of the constraint", &[]),
                )
                .unwrap();
        }
        let hook = |path: &str| {
            json!({
                "tool_name": "Read",
                "session_id": "sess-rev",
                "tool_input": { "file_path": path }
            })
        };
        assert!(file_memory_advisory(&db, &hook("src/one.rs")).is_some(), "first file serves it");
        assert!(
            file_memory_advisory(&db, &hook("src/one.rs")).is_none(),
            "nothing was written since, so the same file stays quiet"
        );

        // A metadata-only revision (a tag added) leaves the text alone, so it
        // has nothing new to say and must not re-fire.
        {
            let head = head_of(&db);
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            store
                .append_mutate_checked(
                    "s", "l", "a", crate::event_store::EventKind::FactRevised, "mem-r",
                    Some(&head),
                    &body("the old wording of the constraint", &["swept".to_string()]),
                )
                .unwrap();
        }
        assert!(
            file_memory_advisory(&db, &hook("src/two.rs")).is_none(),
            "a tag sweep is not news - it must not re-fire on the fact's other file"
        );

        // A real correction does.
        {
            let head = head_of(&db);
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            store
                .append_mutate_checked(
                    "s", "l", "a", crate::event_store::EventKind::FactRevised, "mem-r",
                    Some(&head),
                    &body("CORRECTED: the constraint is the other way round", &[]),
                )
                .unwrap();
        }
        let after = file_memory_advisory(&db, &hook("src/two.rs"));
        assert!(
            after.as_deref().is_some_and(|a| a.contains("CORRECTED")),
            "the corrected wording must reach the agent that already saw the old one: {after:?}"
        );

        // ...and only ONCE, on whichever target is touched first: the
        // cross-key rule from 2026-07-24 (one serving per session and surface,
        // however many keys reach the fact) is untouched by this.
        assert!(
            file_memory_advisory(&db, &hook("src/one.rs")).is_none(),
            "the correction speaks once per surface, not once per file"
        );
    }

    /// The case that actually bites: you correct a rule ABOUT the file you are
    /// editing, while editing it. Until 2026-07-30 the per-(session, file)
    /// debounce returned before any recall ran, so the correction could not
    /// reach that file again for the rest of the session - the agent kept the
    /// stale text. The debounce now carries a watermark of the store's own
    /// mtime: nothing written since means the same cheap early return as
    /// before, a write means look again.
    #[test]
    fn a_correction_reaches_the_very_file_it_governs() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let body = |t: &str| {
            format!(
                "{t}\n\n{}",
                crate::footer::compose_full(
                    "gotcha", &[], "global", &[], &["src/only.rs".to_string()], None, None,
                )
            )
        };
        {
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            store
                .append_event(
                    "s", "l", "a", crate::event_store::EventKind::FactCreated, "mem-o", None,
                    &body("the old wording"),
                )
                .unwrap();
        }
        let hook = json!({
            "tool_name": "Edit",
            "session_id": "sess-same",
            "tool_input": { "file_path": "src/only.rs" }
        });
        assert!(file_memory_advisory(&db, &hook).is_some(), "served once");
        assert!(file_memory_advisory(&db, &hook).is_none(), "quiet while nothing changes");

        {
            let head = {
                let store = crate::event_store::EventStore::new(&db).unwrap();
                let events = store.get_all_events().unwrap();
                crate::cas::compute_head_sets(&events)
                    .get("mem-o")
                    .unwrap()
                    .heads
                    .iter()
                    .next()
                    .unwrap()
                    .clone()
            };
            let mut store = crate::event_store::EventStore::new(&db).unwrap();
            store
                .append_mutate_checked(
                    "s", "l", "a", crate::event_store::EventKind::FactRevised, "mem-o",
                    Some(&head), &body("CORRECTED: the other way round"),
                )
                .unwrap();
        }
        let after = file_memory_advisory(&db, &hook);
        assert!(
            after.as_deref().is_some_and(|a| a.contains("CORRECTED")),
            "the correction must reach the file it governs: {after:?}"
        );
        assert!(
            file_memory_advisory(&db, &hook).is_none(),
            "and then quiet again - once per correction, not on every touch"
        );
    }

    /// "Already shown earlier" must never be cached as "nothing names this
    /// file" (2026-07-30, adversarial verifier). The negative cache is a
    /// fifteen-minute silence, so writing it here would swallow a fact stored
    /// seconds later on a file the store demonstrably knows.
    #[test]
    fn an_already_served_set_is_not_cached_as_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let write = |db: &std::path::Path, id: &str, anchors: Vec<String>| {
            let mut store = crate::event_store::EventStore::new(db).unwrap();
            let body = format!(
                "constraint {id}\n\n{}",
                crate::footer::compose_full("gotcha", &[], "global", &[], &anchors, None, None)
            );
            store
                .append_event(
                    "s", "l", "a", crate::event_store::EventKind::FactCreated, id, None, &body,
                )
                .unwrap();
        };
        write(&db, "mem-a", vec!["src/one.rs".into(), "src/two.rs".into()]);
        let hook = |path: &str| {
            json!({
                "tool_name": "Read",
                "session_id": "sess-neg",
                "tool_input": { "file_path": path }
            })
        };
        file_memory_advisory(&db, &hook("src/one.rs")).expect("serves on the first file");
        assert!(
            file_memory_advisory(&db, &hook("src/two.rs")).is_none(),
            "nothing new to say on the second file"
        );

        // A fact stored right after must still reach the very next touch.
        write(&db, "mem-z", vec!["src/two.rs".into()]);
        let after = file_memory_advisory(&db, &hook("src/two.rs"));
        assert!(
            after.as_deref().is_some_and(|a| a.contains("mem-z")),
            "a fresh fact must not be swallowed by a cached miss that was never a miss: {after:?}"
        );
    }

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

    /// The 2026-07-25 doc-chunk tightening:
    /// measured over every thor/src/*.rs + docs/*.md file, the doc-chunk lane's
    /// symbol-only admissions were 11 of 14 tangential (a chunk merely sharing a
    /// generic identifier fragment with the file, on an unrelated topic), while
    /// its stem-only admissions kept serving genuinely relevant paragraphs
    /// (strength.rs, cas.rs, guard.rs itself). So the doc-chunk lane drops the
    /// symbol leg and keeps the stem leg - the MEMORY lane (tested above) is
    /// unchanged, on purpose: this pins that split.
    /// The two residual defects the tightening measurement surfaced, both fixed
    /// in `names_file` rather than left as follow-ups: an underscored stem that
    /// prose spells as words, and a stem that is only a file's ROLE.
    #[test]
    fn names_file_reads_underscores_as_words_and_ignores_role_stems() {
        // event_store.rs: prose says "the event store", never "event_store".
        assert!(names_file("the event store keeps every revision", "event_store.rs", "event_store"));
        assert!(names_file("see the event-store design note", "event_store.rs", "event_store"));
        // A real subject stem still works on its own (measured good: cas, guard).
        assert!(names_file("resolve the diverged head with cas", "cas.rs", "cas"));
        // main.rs: "main" here is the git branch, not the file - no evidence.
        assert!(!names_file("everything landed on main this morning", "main.rs", "main"));
        assert!(!names_file("the lib is published as a crate", "lib.rs", "lib"));
        // The full name always wins, role stem or not.
        assert!(names_file("main.rs wires the cli", "main.rs", "main"));
    }

    /// The gate exists because reminders measurably do not work here (three
    /// ignored-rule ground truths this month), so it must behave like a gate:
    /// A rule about the SHAPE of a report must not fire on a two-line status
    /// note. Measured 2026-07-26: a 230-character closing sentence was blocked
    /// because it contained the word "pushed" - correct by the letter of the
    /// rule, wrong by its intent, and no word list can tell the two apart.
    /// `min_chars` can: below the floor the rule stays quiet, above it nothing
    /// changes. Absent (the default) it must behave exactly as before.
    #[test]
    fn a_length_floor_lets_a_short_status_line_through_and_still_catches_a_report() {
        let book = r#"[
          { "id": "needs-a-summary", "any_of": ["pushed"], "min_chars": 600,
            "reminder": "open with a plain-language summary" },
          { "id": "no-floor", "any_of": ["pushed"],
            "reminder": "fires at any length" }
        ]"#;
        let rules = parse_rules(book);
        assert_eq!(rules.len(), 2, "both rules parse: {rules:?}");
        assert_eq!(rules[0].min_chars, 600);
        assert_eq!(rules[1].min_chars, 0, "an absent min_chars is no floor at all");

        let short = "the build is green and everything is pushed.";
        let fired = evaluate(&rules, "response", short);
        assert_eq!(
            fired,
            vec!["fires at any length".to_string()],
            "the floored rule stays quiet on a short line; the unfloored one is untouched"
        );

        let long = format!("{} {}", "a real report with numbers and detail.".repeat(20), short);
        assert!(long.chars().count() >= 600, "fixture must clear the floor");
        let fired = evaluate(&rules, "response", &long);
        assert_eq!(fired.len(), 2, "above the floor BOTH rules fire again: {fired:?}");
    }

    /// hold once, accept ONE sentence, then stay quiet for a week - and never
    /// fire on a store with nothing to report.
    #[test]
    fn hygiene_gate_holds_once_a_week_and_is_satisfied_by_saying_it() {
        use crate::event_store::{EventKind, EventStore};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // A typed fact with no fires-when: one real worklist item.
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "P:mem-1", None,
                    "GOTCHA: de watcher pakt alleen het nieuwste pakket\n\n\
                     [memory/gotcha | tags: deploy | project: P]",
                )
                .unwrap();
        }
        // First stop of the week, reply says nothing about maintenance: held.
        let held = hygiene_gate(&db, "klaar, de build is groen en gepusht").expect("gate holds");
        assert!(held.contains("[THOR hygiene]"), "{held}");
        assert!(held.contains("fires-when"), "the reason names what is on the list: {held}");

        // Still within the same week, and still unreported: it keeps holding -
        // a gate that gives up after one refusal is a reminder again.
        assert!(hygiene_gate(&db, "en nu verder").is_some(), "unreported means still held");

        // A DENIAL contains the word too - and used to satisfy the gate, which is
        // how the live test found this hole ("niets over onderhoud").
        assert!(
            hygiene_gate(&db, "klaar, en niets over onderhoud te melden").is_some(),
            "the word alone must not count as having reported it"
        );
        // The reply names the cleanup AND the count: accepted, quiet for a week.
        assert!(
            hygiene_gate(&db, "de opruimlijst heeft nog 1 item, zal ik dat doen?").is_none(),
            "saying it with the count satisfies the gate"
        );
        assert!(hygiene_gate(&db, "klaar, niets over onderhoud").is_none(), "quiet for a week");

        // A store with an empty worklist never costs a turn.
        let clean = dir.path().join("clean.db");
        {
            let mut s = EventStore::new(&clean).unwrap();
            s.append_event(
                "s", "l", "a", EventKind::FactCreated, "P:mem-ok", None,
                "GOTCHA: iets\n\n[memory/gotcha | tags: x | fires-when: iets anders | project: P]",
            )
            .unwrap();
        }
        assert!(hygiene_gate(&clean, "klaar").is_none(), "nothing to report = no gate");

        // And the kill switch works like every other THOR surface.
        std::fs::write(db.with_file_name("THOR-NO-HYGIENE-GATE.flag"), "").unwrap();
        crate::ledger::upsert(&db, "hygiene", "last-reported", &serde_json::json!({ "ts": 0 }));
        assert!(hygiene_gate(&db, "klaar").is_none(), "flag silences the gate");
    }

    #[test]
    fn test_file_advisory_doc_chunks_drop_symbol_only_but_keep_stem_only() {
        use crate::event_store::{EventKind, EventStore};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // The touched file's own chunk defines a distinctive symbol...
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:src/widgets.rs#0", None,
                    "pub fn render_widget_tree() {\n    layout();\n}",
                )
                .unwrap();
            // ...a doc chunk that shares ONLY that symbol name, on an unrelated
            // topic: this is exactly the measured false-positive shape (a
            // coincidental identifier match, not a paragraph about widgets.rs).
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:docs/DESIGN-NOTES.md#0", None,
                    "Some other project's render_widget_tree call happens elsewhere in an unrelated pipeline.",
                )
                .unwrap();
            // ...a doc chunk that names only the STEM (never "widgets.rs", never
            // the symbol): the measured-good leg, left exactly as it was.
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "Proj:docs/CHANGELOG.md#0", None,
                    "The widgets subsystem was reworked to lay out panels lazily.",
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
            "session_id": "s1",
            "cwd": proj.to_string_lossy(),
            "tool_name": "Edit",
            "tool_input": { "file_path": proj.join("src/widgets.rs").to_string_lossy() }
        });

        let adv = file_memory_advisory(&db, &hook).expect("the stem-matched chunk alone must still advise");
        assert!(adv.contains("Proj:docs/CHANGELOG.md#0"), "a chunk naming only the stem still surfaces: {adv}");
        assert!(
            !adv.contains("Proj:docs/DESIGN-NOTES.md#0"),
            "a chunk matching only a defined symbol no longer surfaces: {adv}"
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
