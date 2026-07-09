use crate::event_store::EventStore;
use crate::recall::{recall_scoped, RecallHit, RecallScope};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// How many distinct hits to inject (matches the mimir hook's MaxHits).
const MAX_HITS: usize = 3;
/// How many hits to recall as the working pool. Deeper than MAX_HITS so the
/// session ledger can rotate suppressed hits out and the echo prior can promote
/// a proven-useful fact from below the fold. Recall cost is limit-independent.
const POOL_HITS: usize = 8;
/// A rev injected within this many recent prompts is suppressed (not repeated).
const SUPPRESS_WINDOW: u64 = 5;
/// Echo-promotion threshold: a below-the-fold fact the agent marked useful may
/// take slot 3 only when its bm25 strength is within this factor of slot 3's.
const ECHO_RANK_SLACK: f64 = 1.5;
/// Skip prompts shorter than this (pure acks like "ok").
const MIN_CHARS: usize = 4;
/// Cap the query length fed to recall.
const MAX_PROMPT_CHARS: usize = 500;

/// Words that, when they make up the WHOLE prompt, mean "no recall worth doing"
/// (acks / git verbs / greetings). Ported 1:1 from hook_recall.ps1 so THOR's
/// gating matches the live mimir hook it runs beside.
const TRIVIAL_WORDS: &[&str] = &[
    "ok", "oke", "okay", "k", "kk", "thanks", "thx", "ty", "bedankt", "dank", "dankje", "ja",
    "jawel", "jep", "yes", "yep", "yup", "nee", "neen", "no", "nope", "nop", "commit", "push",
    "pull", "merge", "stage", "staged", "rebase", "doe", "maar", "dit", "dat", "het", "graag",
    "please", "svp", "aub", "mooi", "top", "goed", "prima", "perfect", "klopt", "super", "fijn",
    "nice", "great", "good",
];

/// True when EVERY word of the prompt is trivial (so a terse real question like
/// "PID gains?" still recalls - only pure acks/commands are dropped).
fn is_all_trivial(prompt: &str) -> bool {
    for word in prompt
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if !TRIVIAL_WORDS.contains(&word) {
            return false; // a non-trivial word: worth recalling
        }
    }
    // every word was trivial, or there were no words at all
    true
}

/// Flag-file check (THOR-SILENT.flag, THOR-PRIMARY.flag): see ledger::flag_present.
fn flag_present(db: &Path, name: &str) -> bool {
    crate::ledger::flag_present(db, name)
}

/// Stateless per-hook courier. Reads the UserPromptSubmit hook JSON on stdin,
/// recalls THOR memory for the prompt, and prints an injection block to stdout.
///
/// HARD fail-open: every failure path (no stdin, bad JSON, store unreachable,
/// recall error) prints nothing and returns, so the courier can NEVER block or
/// slow a prompt. The caller always exits 0.
pub fn run_courier(db: &Path) {
    if let Some(block) = build_injection(db) {
        // single write; the hook forwards stdout verbatim into the model turn
        println!("{}", block);
    }
}

/// The pure core: returns the injection block to print, or None to stay silent.
/// Split out from run_courier so it is unit-testable without touching stdout.
fn build_injection(db: &Path) -> Option<String> {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return None;
    }
    injection_for_hook_json(db, &raw)
}

/// Given the raw hook JSON and a db path, produce the injection block (or None).
/// Applies the same gates as the mimir hook: min length, whole-prompt-trivial,
/// prompt truncation, dedup, and a hard cap.
/// Public so the drift-eval harness (examples/drift_eval.rs) drives the REAL path.
pub fn injection_for_hook_json(db: &Path, raw: &str) -> Option<String> {
    // Flip valve: THOR-SILENT.flag silences THOR entirely (its own kill-switch).
    // Checked first, so a silenced courier does nothing else. Flipping is a file,
    // never a code change.
    if flag_present(db, "THOR-SILENT.flag") {
        return None;
    }
    // Tolerate a leading UTF-8 BOM: some environments prepend one, and a BOM
    // would otherwise make serde reject the JSON so the courier silently
    // recalls nothing. (A UTF-16 stdin still fails open, which is correct.)
    let raw = raw.trim_start_matches('\u{feff}');
    if raw.trim().is_empty() {
        return None;
    }
    let data: serde_json::Value = serde_json::from_str(raw).ok()?;
    let prompt = data.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let trimmed = prompt.trim();

    if trimmed.chars().count() < MIN_CHARS {
        return None;
    }
    if is_all_trivial(trimmed) {
        return None;
    }
    let query: String = trimmed.chars().take(MAX_PROMPT_CHARS).collect();
    // An all-stopword prompt ("wat is dat dan") carries no content to match:
    // recall's best-effort fallback would search the stopwords themselves, and
    // a body containing two of them could even earn matched_and and bypass the
    // coverage gate below. Nothing worth recalling - stay silent.
    if !crate::recall::has_content_terms(&query) {
        return None;
    }

    // Store unreachable -> silent (the "hub-down -> exit 0" contract). Opening
    // creates an empty store if none exists, which simply yields no hits.
    let store = EventStore::new(db).ok()?;
    // Project isolation: recall inside project A must not surface project B's code
    // OR its memories. Derive the project from the hook cwd (a `.thor` marker or git
    // walk-up, no subprocess); the CORE recall then scopes to that project + the
    // always-in-scope global tier. A projectless cwd (scratch dir) -> global-only,
    // so auto-injection never re-imports another project's clutter.
    let cwd = data.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
    let session_id = data.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
    let project = cwd.as_deref().and_then(|c| crate::repo::project_key(Path::new(c)));
    let scope = RecallScope::current(project.clone());
    let pool = recall_for(db, &store, &query, &scope, POOL_HITS);

    // Silence threshold: an OR-fallback pool (only some query words matched) is
    // gated on real term coverage, so "best of an all-weak pool" is silence, not
    // three confident-looking noise lines. Strict-AND/semantic-evidence hits
    // (matched_and) pass as-is.
    let pool: Vec<RecallHit> = pool
        .into_iter()
        .filter(|h| h.matched_and || crate::recall::covers_query(&h.body, &query))
        .collect();

    // Per-session injection ledger: never re-inject a rev shown within the last
    // SUPPRESS_WINDOW prompts; deeper pool hits rotate into the freed slots.
    // Sessionless hooks (no session_id) behave statelessly, exactly as before.
    // Loaded (and saved) even when the pool is empty: the suppression window
    // slides on every recall-eligible prompt, not only on prompts with hits -
    // else "shown 5 prompts ago" could mean 50 real prompts ago.
    let mut ledger = SessionLedger::load(db, session_id);
    if pool.is_empty() {
        ledger.save(db);
        return None;
    }
    let pool_top_rev = pool[0].rev.clone();
    let survivors: Vec<RecallHit> =
        pool.into_iter().filter(|h| !ledger.suppressed(h)).collect();

    // Echo prior: if none of the top picks was ever marked useful but a
    // below-the-fold fact was (and it ranks close enough), give it slot 3. The
    // echo query only runs when a promotion is even possible (survivors deeper
    // than the slots), and only over the survivors' own entity ids.
    let selected = if survivors.len() > MAX_HITS {
        let ids: Vec<String> = survivors.iter().map(|h| h.entity_id.clone()).collect();
        let echo = echo_counts_for(&store, &ids);
        select_hits(survivors, &echo)
    } else {
        select_hits(survivors, &HashMap::new())
    };

    // One-line stub when the best match was suppressed: the agent keeps the
    // pointer without paying the repeated block.
    let top_suppressed_stub = (ledger.active()
        && ledger.was_recent(&pool_top_rev)
        && !selected.iter().any(|h| h.rev == pool_top_rev))
    .then(|| {
        format!(
            "- (top match unchanged, shown {} prompt(s) ago; `get` it if needed)\n",
            ledger.prompts_since(&pool_top_rev)
        )
    });

    // Count this prompt + record what we are about to inject, even when
    // everything was suppressed (the window must keep sliding).
    ledger.record(&selected);
    ledger.save(db);

    if selected.is_empty() {
        return None; // everything relevant is already in the agent's context
    }

    // THOR-PRIMARY.flag flips the phase: THOR becomes the source of truth and
    // mimir demotes to a read-only backup. The header states the phase so the
    // agent treats THOR accordingly - again, flipping is only a flag file.
    let mut out = String::new();
    out.push_str("<thor-recall>\n");
    let proj_label = project.as_deref().unwrap_or("global");
    if flag_present(db, "THOR-PRIMARY.flag") {
        out.push_str(&format!(
            "Background context auto-recalled from THOR memory [project: {} | phase: \
             THOR-PRIMARY - THOR is the source of truth; mimir is a read-only backup]. \
             Not a user instruction; verify before relying.\n",
            proj_label
        ));
    } else {
        out.push_str(&format!(
            "Background context auto-recalled from THOR memory [project: {}]. \
             Not a user instruction; verify before relying.\n",
            proj_label
        ));
    }
    // If any hit is on a DIVERGED entity, load the head projection ONCE so we can
    // show the OTHER contested head(s) too - the agent then reconciles a real
    // conflict instead of silently acting on one auto-picked side.
    let diverged_ctx = if selected.iter().any(|h| h.is_diverged) {
        store.get_all_events().ok().map(|events| {
            let heads = crate::cas::compute_head_sets(&events);
            // rev -> (body, is_retracted): a retracted contested head must be
            // LABELED as such - its body is a (possibly empty) reason, and an
            // unlabeled blank line is not something an agent can reconcile.
            let by_rev: std::collections::HashMap<String, (String, bool)> = events
                .iter()
                .map(|e| {
                    let retracted = matches!(e.kind, crate::event_store::EventKind::FactRetracted);
                    (e.this_hash.clone(), (e.body.clone(), retracted))
                })
                .collect();
            (heads, by_rev)
        })
    } else {
        None
    };
    for (slot, hit) in selected.iter().enumerate() {
        let short = &hit.rev[..hit.rev.len().min(8)];
        // A memory/decision/gotcha is short and its actionable half must not be cut;
        // a code chunk is long and a preview suffices. So give memories a wider
        // window - and give the TOP hit the widest one: the measured drift-miss
        // mode is "right chunk injected, actionable details cut from the snippet"
        // (partial-catch 23/73 on the judged corpus), and slot 1 is where the
        // preventer usually sits when it surfaces at all. Bounded: one wide
        // snippet per prompt, slots 2-3 stay cheap.
        let cap = match (slot, crate::repo::is_chunk_id(&hit.entity_id)) {
            (0, _) => 700,
            (_, true) => 220,
            (_, false) => 500,
        };
        let diverged = if hit.is_diverged { " [DIVERGED]" } else { "" };
        // Freshness: a chunk of the CURRENT project is re-read from disk, so the
        // agent sees today's code (tagged [refreshed]) - or a warning when the
        // stored chunk no longer exists ([stale?]). Memories pass through.
        let (fresh_tag, snip) =
            fresh_snippet(&hit.entity_id, &hit.body, &query, cap, project.as_deref(), cwd.as_deref());
        // Type tag for hand-written constraints, so a gotcha/decision reads as
        // one at a glance instead of blending in with chunks.
        let type_tag = hit.fact_type.map(|t| format!(" [{}]", t.as_str())).unwrap_or_default();
        // Scope tag so the agent knows which project a hit belongs to (esp. memories,
        // whose ids are opaque): [global] for the global tier, else [proj:<key>].
        let scope_tag = if crate::repo::is_global(hit.project.as_deref()) {
            "[global]".to_string()
        } else {
            format!("[proj:{}]", hit.project.as_deref().unwrap_or("?"))
        };
        out.push_str(&format!(
            "- {}{} {} ({}{}{}): {}\n",
            scope_tag, type_tag, hit.entity_id, short, diverged, fresh_tag, snip
        ));
        // Show the other contested head(s) so the agent reconciles, not guesses.
        if hit.is_diverged {
            if let Some((heads, by_rev)) = &diverged_ctx {
                if let Some(hs) = heads.get(&hit.entity_id) {
                    for rev in &hs.heads {
                        if rev == &hit.rev {
                            continue;
                        }
                        if let Some((body, retracted)) = by_rev.get(rev) {
                            let s = crate::recall::snippet(body, cap, &query);
                            let label = if *retracted { ", retracted" } else { "" };
                            out.push_str(&format!(
                                "    | contested head ({}{}): {}\n",
                                &rev[..rev.len().min(8)],
                                label,
                                s
                            ));
                        }
                    }
                }
            }
        }
    }
    if let Some(stub) = top_suppressed_stub {
        out.push_str(&stub);
    }
    out.push_str("</thor-recall>");
    Some(out)
}

/// FactEchoed count per entity, restricted to the given entity ids (served by
/// idx_event_entity instead of a full-table scan - this runs on the per-prompt
/// hot path). The "this actually helped me" signal written by the mark tool.
/// Fail-soft: any query error means an empty map (no prior).
fn echo_counts_for(store: &EventStore, ids: &[String]) -> HashMap<String, i64> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let conn = store.conn();
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT entity_id, COUNT(*) FROM event WHERE kind = ? AND entity_id IN ({}) GROUP BY entity_id",
        placeholders
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let params = std::iter::once(crate::event_store::EventKind::FactEchoed.as_str().to_string())
        .chain(ids.iter().cloned());
    let rows = match stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    }) {
        Ok(r) => r,
        Err(_) => return HashMap::new(),
    };
    rows.flatten().collect()
}

/// Rank slack for the typed-constraint slot: a deeper gotcha/decision/preference
/// may take slot 3 only when its bm25 strength is within this factor of slot 3's.
/// Same conservatism as the echo prior: a typed fact never displaces a clearly
/// stronger match, and slots 1-2 are never touched.
const TYPED_RANK_SLACK: f64 = 1.5;

/// Pick MAX_HITS from the (already gated + suppression-filtered) pool, in rank
/// order - then at most ONE slot-3 promotion, in priority order:
/// 1. echo prior: if NO pick was ever marked useful but a deeper hit was (and
///    ranks within ECHO_RANK_SLACK of slot 3), it takes slot 3 - "this actually
///    helped before" is the strongest prior we have;
/// 2. typed constraint: else, if NO pick is a typed gotcha/decision/preference
///    but a deeper one is (within TYPED_RANK_SLACK), it takes slot 3 - the
///    drift preventer is usually a typed fact ranked just below a wall of
///    same-topic chunks.
/// Conservative by construction; a promotion never fires against a dense-only
/// slot 3 (rank 0.0 = no lexical evidence to compare against).
fn select_hits(pool: Vec<RecallHit>, echo: &HashMap<String, i64>) -> Vec<RecallHit> {
    let echoed = |h: &RecallHit| echo.get(&h.entity_id).copied().unwrap_or(0) > 0;
    let typed = |h: &RecallHit| h.fact_type.is_some();
    let mut selected: Vec<RecallHit> = Vec::with_capacity(MAX_HITS);
    let mut rest: Vec<RecallHit> = Vec::new();
    for h in pool {
        if selected.len() < MAX_HITS {
            selected.push(h);
        } else {
            rest.push(h);
        }
    }
    if selected.len() == MAX_HITS {
        let slot3_rank = selected[MAX_HITS - 1].rank;
        // bm25 ranks are negative (more negative = stronger); a rank of 0.0 is a
        // dense-only hit with no lexical evidence - never promote against that.
        if slot3_rank < 0.0 {
            let within =
                |h: &RecallHit, slack: f64| h.rank < 0.0 && h.rank <= slot3_rank / slack;
            if !selected.iter().any(&echoed) {
                if let Some(pos) =
                    rest.iter().position(|h| echoed(h) && within(h, ECHO_RANK_SLACK))
                {
                    selected[MAX_HITS - 1] = rest.swap_remove(pos);
                    return selected;
                }
            }
            if !selected.iter().any(&typed) {
                if let Some(pos) =
                    rest.iter().position(|h| typed(h) && within(h, TYPED_RANK_SLACK))
                {
                    selected[MAX_HITS - 1] = rest.swap_remove(pos);
                }
            }
        }
    }
    selected
}

// ---- Per-session injection ledger --------------------------------------------

/// Sliding-window suppression state for one session, persisted as one row in
/// the fail-open ledger sidecar (crate::ledger, ns "courier-seen"). Inactive
/// (stateless, the pre-ledger behavior) when the hook carries no session_id or
/// the ledger cannot be read - the courier contract (never block, never error)
/// is preserved.
struct SessionLedger {
    session_id: String,
    /// This prompt's ordinal within the session (1-based; already incremented).
    count: u64,
    /// rev|diverged -> the prompt ordinal at which it was last injected.
    seen: HashMap<String, u64>,
}

fn ledger_key(rev: &str, diverged: bool) -> String {
    // Keyed on rev + diverged so a divergence FLIP re-surfaces the same rev with
    // its new [DIVERGED] marker instead of being suppressed as "already shown".
    format!("{}|{}", rev, diverged)
}

impl SessionLedger {
    fn load(db: &Path, session_id: &str) -> Self {
        let mut this = SessionLedger { session_id: session_id.to_string(), count: 0, seen: HashMap::new() };
        if session_id.is_empty() {
            return this; // inactive
        }
        if let Some(entry) = crate::ledger::get(db, "courier-seen", session_id) {
            this.count = entry.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            if let Some(seen) = entry.get("seen").and_then(|v| v.as_object()) {
                this.seen = seen
                    .iter()
                    .filter_map(|(k, v)| v.as_u64().map(|at| (k.clone(), at)))
                    .collect();
            }
        }
        this.count += 1; // this prompt
        this
    }

    fn active(&self) -> bool {
        !self.session_id.is_empty()
    }

    fn suppressed(&self, hit: &RecallHit) -> bool {
        if !self.active() {
            return false;
        }
        match self.seen.get(&ledger_key(&hit.rev, hit.is_diverged)) {
            Some(at) => self.count.saturating_sub(*at) <= SUPPRESS_WINDOW,
            None => false,
        }
    }

    /// Was this rev (either diverged state) injected within the window?
    fn was_recent(&self, rev: &str) -> bool {
        [true, false].iter().any(|d| {
            self.seen
                .get(&ledger_key(rev, *d))
                .map(|at| self.count.saturating_sub(*at) <= SUPPRESS_WINDOW)
                .unwrap_or(false)
        })
    }

    fn prompts_since(&self, rev: &str) -> u64 {
        [true, false]
            .iter()
            .filter_map(|d| self.seen.get(&ledger_key(rev, *d)))
            .map(|at| self.count.saturating_sub(*at))
            .min()
            .unwrap_or(0)
    }

    fn record(&mut self, injected: &[RecallHit]) {
        if !self.active() {
            return;
        }
        for h in injected {
            self.seen.insert(ledger_key(&h.rev, h.is_diverged), self.count);
        }
        // entries older than the window are dead weight - drop them here so the
        // sidecar stays bounded even in a very long session.
        let count = self.count;
        self.seen.retain(|_, at| count.saturating_sub(*at) <= SUPPRESS_WINDOW);
    }

    fn save(&self, db: &Path) {
        if !self.active() {
            return;
        }
        let now = crate::review::now_secs();
        let seen: serde_json::Map<String, serde_json::Value> =
            self.seen.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect();
        crate::ledger::upsert(
            db,
            "courier-seen",
            &self.session_id,
            &serde_json::json!({ "ts": now, "count": self.count, "seen": seen }),
        );
    }
}

/// Clear one session's courier ledger (SessionStart on `source:"compact"`): the
/// context was just wiped, so re-injection is exactly the point again.
pub fn clear_session_ledger(db: &Path, session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    crate::ledger::remove(db, "courier-seen", session_id);
}

// ---- Freshness ----------------------------------------------------------------

/// Freshness tag + display snippet for one hit, shared by every surface that
/// shows recall hits (courier injection, MCP recall, CLI recall): the tag is
/// "" / " [refreshed]" / " [stale?]" and the snippet is cut from the LIVE text
/// when the file changed since ingest.
pub(crate) fn fresh_snippet(
    entity_id: &str,
    body: &str,
    query: &str,
    cap: usize,
    project: Option<&str>,
    cwd: Option<&str>,
) -> (String, String) {
    match freshness(entity_id, body, project, cwd) {
        Freshness::Current => (String::new(), crate::recall::snippet(body, cap, query)),
        Freshness::Refreshed(live) => {
            (" [refreshed]".to_string(), crate::recall::snippet(&live, cap, query))
        }
        Freshness::Stale => (" [stale?]".to_string(), crate::recall::snippet(body, cap, query)),
    }
}

pub(crate) enum Freshness {
    /// Stored chunk still matches the file on disk (or the hit is not a chunk of
    /// the current project, or anything errored - fail-open to the stored body).
    Current,
    /// The file changed since ingest: inject THIS live chunk text instead.
    Refreshed(String),
    /// The file is gone or the chunk index no longer exists: warn the agent.
    Stale,
}

/// Ingest is a snapshot; the agent edits all session long. Before injecting a
/// chunk of the CURRENT project, re-read its file (same truncation + chunking as
/// ingest) and compare - so THOR never presents outdated code as fresh context.
/// Bounded: at most a handful of single-file reads per call site (courier slots,
/// MCP/CLI recall hits, one get). Hard fail-open. Shared with the deliberate
/// read surfaces (MCP recall/get, CLI recall/get) so a stale chunk is flagged
/// everywhere, not only in auto-injection.
pub(crate) fn freshness(entity_id: &str, stored_body: &str, project: Option<&str>, cwd: Option<&str>) -> Freshness {
    if !crate::repo::is_chunk_id(entity_id) {
        return Freshness::Current;
    }
    let (project, cwd) = match (project, cwd) {
        (Some(p), Some(c)) => (p, c),
        _ => return Freshness::Current,
    };
    // Only chunks OWNED by the current project resolve against this cwd's root.
    if crate::repo::owner_project(entity_id) != Some(project) {
        return Freshness::Current;
    }
    let rest = match entity_id.split_once(':') {
        Some((_, rest)) => rest,
        None => return Freshness::Current,
    };
    let (rel, n) = match rest.rsplit_once('#') {
        Some((rel, n)) => match n.parse::<usize>() {
            Ok(n) => (rel, n),
            Err(_) => return Freshness::Current,
        },
        None => return Freshness::Current,
    };
    // Path safety: a chunk rel is always a forward-slash relative path; refuse
    // anything that could escape the project root. The ':' rule is a Windows-ism
    // (drive-letter absolutes like "C:\x"); on Unix ':' is a legal filename char
    // that ingest itself will produce, so it must not disable freshness there.
    if rel.is_empty()
        || rel.starts_with('/')
        || rel.contains("..")
        || (cfg!(windows) && rel.contains(':'))
    {
        return Freshness::Current;
    }
    let root = match crate::repo::project_root(Path::new(cwd)) {
        Some(r) => r,
        None => return Freshness::Current,
    };
    let path = root.join(rel);
    // Size guard BEFORE the read: this runs synchronously inside the per-prompt
    // hook, and read_to_string would load the whole file even though ingest only
    // ever chunked the first MAX_FILE_CHARS. A file too big for a cheap re-read
    // falls back to the stored snapshot (fail-open), never to a multi-hundred-MB
    // allocation on the prompt path. 4 bytes/char covers any UTF-8 text within
    // the ingest window.
    const FRESHNESS_MAX_BYTES: u64 = (crate::repo::MAX_FILE_CHARS as u64) * 4;
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > FRESHNESS_MAX_BYTES => return Freshness::Current,
        Ok(_) => {}
        Err(_) => return Freshness::Stale, // tracked at ingest, gone now
    }
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Freshness::Stale, // tracked at ingest, unreadable now
    };
    crate::repo::truncate_to_max_file_chars(&mut text);
    let chunks = crate::repo::chunk_text(&text, crate::repo::MAX_CHUNK_CHARS);
    if n >= chunks.len() {
        return Freshness::Stale;
    }
    let live = crate::repo::chunk_body(&chunks[n], project, rel, n, chunks.len());
    if live == stored_body {
        Freshness::Current
    } else {
        Freshness::Refreshed(chunks[n].clone())
    }
}

/// Recall for the courier AND the MCP recall tool (fused parity: a deliberate
/// agent query deserves the same semantic path auto-injection gets): the
/// semantic score-fusion path when the feature is built AND the local model +
/// sidecar are present AND a warm query vector is available; otherwise pure
/// bm25. EVERY semantic failure degrades to bm25 (and warms the daemon for next
/// time), so a caller never pays the ~1.25s cold model load, never blocks, and
/// never returns worse than bm25.
pub(crate) fn recall_for(
    db: &Path,
    store: &EventStore,
    query: &str,
    scope: &RecallScope,
    limit: usize,
) -> Vec<RecallHit> {
    #[cfg(feature = "semantic")]
    {
        if let Some(hits) = try_semantic_recall(db, store, query, scope, limit) {
            return hits;
        }
    }
    let _ = db; // only the semantic path needs the db path (for the daemon/sidecar)
    recall_scoped(store, query, limit, scope).unwrap_or_default()
}

/// Attempt score-fusion recall. Returns None (caller falls back to bm25) whenever
/// the model or sidecar is absent, the sidecar is from a different model, the
/// warm daemon is unreachable (then it is spawned for the next prompt), or the
/// fused result is empty/errored.
#[cfg(feature = "semantic")]
fn try_semantic_recall(
    db: &Path,
    store: &EventStore,
    query: &str,
    scope: &RecallScope,
    limit: usize,
) -> Option<Vec<RecallHit>> {
    use crate::vectors::{default_vectors_path, VectorStore};

    if !crate::embed::model_present(&crate::embed::default_model_dir()) {
        return None; // no local model -> nothing to warm, stay on bm25
    }
    let vpath = default_vectors_path(db);
    if !vpath.exists() {
        return None; // no sidecar built yet
    }
    // Warm query vector from the resident daemon. If it is not up, warm it for the
    // NEXT prompt and use bm25 for this one (never cold-load in the hook path).
    let qvec = match crate::embed_daemon::client_embed(db, query) {
        Some(v) => v,
        None => {
            crate::embed_daemon::ensure_daemon(db);
            return None;
        }
    };
    let vecs = VectorStore::open(&vpath).ok()?;
    if vecs.model_id().as_deref() != Some(crate::embed::MODEL_ID) {
        return None; // sidecar built by a different model -> stale until rebuilt
    }
    match crate::recall::recall_fused_scoped(store, query, &qvec, &vecs, limit, crate::recall::FUSION_LAMBDA, scope) {
        Ok(hits) if !hits.is_empty() => Some(hits),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventKind;

    #[test]
    fn test_trivial_gate() {
        assert!(is_all_trivial("ok"));
        assert!(is_all_trivial("ok bedankt"));
        assert!(is_all_trivial("commit push"));
        assert!(is_all_trivial("   "));
        assert!(!is_all_trivial("how do I fix the deploy watcher"));
        assert!(!is_all_trivial("PID gains"));
    }

    fn seed(db: &Path) {
        let mut store = EventStore::new(db).unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "the deploy watcher gotcha lives here")
            .unwrap();
    }

    #[test]
    fn test_injection_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        let raw = r#"{"prompt":"how does the deploy watcher work","cwd":"x","session_id":"s1"}"#;
        let out = injection_for_hook_json(&db, raw).expect("should inject");
        assert!(out.contains("<thor-recall>"));
        assert!(out.contains("e1"));
        assert!(out.contains("deploy watcher"));
    }

    #[test]
    fn test_project_isolation_no_bleed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // two projects' chunks + one global memory, all matching "widget"
            for (eid, body) in [
                ("ProjA:a.rs#0", "the widget lives in project A"),        // ProjA code
                ("ProjB:b.rs#0", "the widget lives in project B"),        // ProjB code
                ("ProjB:mem-y", "the widget decision for project B"),     // ProjB MEMORY
                ("01KGLOBALMEMORY0000000000", "widget preference: always use blue"), // global
            ] {
                store.append_event("s", "l", "a", EventKind::FactCreated, eid, None, body).unwrap();
            }
        }
        // a cwd whose repo root basename is "ProjA"
        let proj_a = dir.path().join("ProjA");
        std::fs::create_dir_all(proj_a.join(".git")).unwrap();
        let raw = format!(
            r#"{{"prompt":"where is the widget","cwd":{}}}"#,
            serde_json::to_string(&proj_a.to_string_lossy()).unwrap()
        );
        let out = injection_for_hook_json(&db, &raw).expect("should inject");
        assert!(out.contains("ProjA:a.rs#0"), "same-project chunk kept");
        assert!(out.contains("01KGLOBALMEMORY"), "global memory kept");
        assert!(out.contains("[proj:ProjA]") && out.contains("[global]"), "hits are scope-labelled");
        assert!(out.contains("[project: ProjA]"), "header states the current project");
        assert!(!out.contains("ProjB:b.rs#0"), "another project's CODE must NOT bleed in");
        assert!(!out.contains("ProjB:mem-y"), "another project's MEMORY must NOT bleed in");
    }

    #[test]
    fn test_injection_tolerates_utf8_bom() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        let raw = format!("\u{feff}{}", r#"{"prompt":"how does the deploy watcher work"}"#);
        assert!(
            injection_for_hook_json(&db, &raw).is_some(),
            "a BOM-prefixed hook JSON must still recall, not silently degrade"
        );
    }

    #[test]
    fn test_injection_gates_and_failopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        // trivial prompt -> silent
        assert!(injection_for_hook_json(&db, r#"{"prompt":"ok"}"#).is_none());
        // all-stopword prompt -> silent (its stopwords must never become the query)
        assert!(injection_for_hook_json(&db, r#"{"prompt":"wat is dat dan"}"#).is_none());
        // too short -> silent
        assert!(injection_for_hook_json(&db, r#"{"prompt":"hi"}"#).is_none());
        // no match -> silent
        assert!(injection_for_hook_json(&db, r#"{"prompt":"unrelated xyzzy token"}"#).is_none());
        // malformed JSON -> silent (fail-open)
        assert!(injection_for_hook_json(&db, "not json at all").is_none());
        // empty stdin -> silent
        assert!(injection_for_hook_json(&db, "   ").is_none());
        // missing prompt field -> silent
        assert!(injection_for_hook_json(&db, r#"{"cwd":"x"}"#).is_none());
    }

    #[test]
    fn test_session_ledger_suppresses_repeats_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            for i in 0..5 {
                store
                    .append_event(
                        "s", "l", "a", EventKind::FactCreated, &format!("e{i}"), None,
                        &format!("deploy watcher fact number {i}"),
                    )
                    .unwrap();
            }
        }
        let raw = r#"{"prompt":"deploy watcher","cwd":"x","session_id":"sess-1"}"#;
        let first = injection_for_hook_json(&db, raw).expect("first prompt injects");
        // top-3 of the pool injected
        let injected_first: Vec<bool> = (0..5).map(|i| first.contains(&format!("e{i}"))).collect();
        assert_eq!(injected_first.iter().filter(|b| **b).count(), 3, "3 hits injected: {first}");

        // same prompt again, same session: the shown revs are suppressed and the
        // DEEPER pool hits rotate in (with a stub for the suppressed top match).
        let second = injection_for_hook_json(&db, raw).expect("rotation injects the deeper hits");
        for (i, was_in_first) in injected_first.iter().enumerate() {
            let id = format!("e{i}");
            assert_eq!(
                second.contains(&format!("- [global] {id} ")),
                !was_in_first,
                "prompt 2 must inject exactly the NOT-yet-shown hits; offender: {id}\n{second}"
            );
        }
        assert!(second.contains("top match unchanged"), "stub points at the suppressed top: {second}");

        // third time: everything within the window -> full silence
        assert!(
            injection_for_hook_json(&db, raw).is_none(),
            "everything recently shown -> silent, not a repeated block"
        );

        // a DIFFERENT session is unaffected
        let other = r#"{"prompt":"deploy watcher","cwd":"x","session_id":"sess-2"}"#;
        assert!(injection_for_hook_json(&db, other).is_some(), "other sessions keep their own ledger");

        // clearing the ledger (what SessionStart does on compact) re-arms injection
        clear_session_ledger(&db, "sess-1");
        assert!(
            injection_for_hook_json(&db, raw).is_some(),
            "post-compaction the same facts must inject again"
        );
    }

    #[test]
    fn test_suppression_window_slides_on_hitless_prompts() {
        // Regression: the prompt counter must advance on EVERY recall-eligible
        // prompt, not only on prompts with hits - else facts shown "5 prompts
        // ago" stay suppressed after 50 unrelated prompts.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            store
                .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "deploy watcher fact")
                .unwrap();
        }
        let hit = r#"{"prompt":"deploy watcher","cwd":"x","session_id":"slide-1"}"#;
        let miss = r#"{"prompt":"completely unrelated xyzzy topic","cwd":"x","session_id":"slide-1"}"#;
        assert!(injection_for_hook_json(&db, hit).is_some(), "prompt 1 injects");
        assert!(injection_for_hook_json(&db, hit).is_none(), "immediate repeat suppressed");
        for _ in 0..SUPPRESS_WINDOW {
            assert!(injection_for_hook_json(&db, miss).is_none(), "unrelated prompts are silent");
        }
        assert!(
            injection_for_hook_json(&db, hit).is_some(),
            "after {} hitless prompts the window has slid and the fact re-injects",
            SUPPRESS_WINDOW
        );
    }

    #[test]
    fn test_silence_gate_blocks_single_word_coincidence() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            store
                .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "the sync module ships batches")
                .unwrap();
        }
        // multi-word prompt, only "sync" matches -> OR-fallback coincidence -> silent
        let raw = r#"{"prompt":"ga verder met de sync refactor","cwd":"x"}"#;
        assert!(
            injection_for_hook_json(&db, raw).is_none(),
            "a one-word overlap on a multi-word prompt is noise, not an answer"
        );
        // a prompt genuinely covered by the body still injects
        let raw2 = r#"{"prompt":"sync ships batches how","cwd":"x"}"#;
        assert!(injection_for_hook_json(&db, raw2).is_some(), "real coverage still injects");
    }

    #[test]
    fn test_select_hits_promotes_echoed_fact_into_slot3() {
        let mk = |id: &str, rank: f64| RecallHit {
            entity_id: id.to_string(),
            rev: format!("rev-{id}"),
            body: "b".to_string(),
            kind: EventKind::FactCreated,
            is_diverged: false,
            rank,
            project: None,
            fact_type: None,
            matched_and: true,
        };
        let mut echo = std::collections::HashMap::new();
        echo.insert("e4".to_string(), 2i64);

        // e4 (echoed) is within 1.5x of slot 3 -> promoted into slot 3
        let pool = vec![mk("e1", -9.0), mk("e2", -8.0), mk("e3", -6.0), mk("e4", -5.0)];
        let sel = select_hits(pool, &echo);
        assert_eq!(sel[2].entity_id, "e4", "close-ranked echoed fact takes slot 3");
        assert_eq!(sel[0].entity_id, "e1");
        assert_eq!(sel[1].entity_id, "e2");

        // too far below slot 3 (-6/1.5 = -4 needed; -1 is far weaker) -> no swap
        let pool = vec![mk("e1", -9.0), mk("e2", -8.0), mk("e3", -6.0), mk("e4", -1.0)];
        let sel = select_hits(pool, &echo);
        assert_eq!(sel[2].entity_id, "e3", "a much weaker echoed fact never displaces a strong match");

        // an echoed fact already in the top 3 -> no swap needed
        let mut echo2 = std::collections::HashMap::new();
        echo2.insert("e2".to_string(), 1i64);
        let pool = vec![mk("e1", -9.0), mk("e2", -8.0), mk("e3", -6.0), mk("e4", -5.9)];
        let sel = select_hits(pool, &echo2);
        assert_eq!(sel[2].entity_id, "e3", "top already carries an echoed fact");
    }

    #[test]
    fn test_select_hits_typed_constraint_slot3() {
        let mk = |id: &str, rank: f64, ty: Option<crate::repo::FactType>| RecallHit {
            entity_id: id.to_string(),
            rev: format!("rev-{id}"),
            body: "b".to_string(),
            kind: EventKind::FactCreated,
            is_diverged: false,
            rank,
            project: None,
            fact_type: ty,
            matched_and: true,
        };
        use crate::repo::FactType::Gotcha;
        let no_echo = std::collections::HashMap::new();

        // no typed hit in the top 3, a close-ranked gotcha below -> slot 3
        let pool = vec![
            mk("c1", -9.0, None),
            mk("c2", -8.0, None),
            mk("c3", -6.0, None),
            mk("g", -5.0, Some(Gotcha)),
        ];
        let sel = select_hits(pool, &no_echo);
        assert_eq!(sel[2].entity_id, "g", "close-ranked typed constraint takes slot 3");

        // a typed hit already selected -> untouched
        let pool = vec![
            mk("c1", -9.0, Some(Gotcha)),
            mk("c2", -8.0, None),
            mk("c3", -6.0, None),
            mk("g", -5.9, Some(Gotcha)),
        ];
        let sel = select_hits(pool, &no_echo);
        assert_eq!(sel[2].entity_id, "c3", "top already carries a typed fact");

        // echo promotion outranks typed promotion (one swap max)
        let mut echo = std::collections::HashMap::new();
        echo.insert("e".to_string(), 1i64);
        let pool = vec![
            mk("c1", -9.0, None),
            mk("c2", -8.0, None),
            mk("c3", -6.0, None),
            mk("g", -5.5, Some(Gotcha)),
            mk("e", -5.0, None),
        ];
        let sel = select_hits(pool, &echo);
        assert_eq!(sel[2].entity_id, "e", "the echo prior wins the single slot-3 swap");

        // a far-weaker typed hit never displaces a strong match
        let pool = vec![
            mk("c1", -9.0, None),
            mk("c2", -8.0, None),
            mk("c3", -6.0, None),
            mk("g", -1.0, Some(Gotcha)),
        ];
        let sel = select_hits(pool, &no_echo);
        assert_eq!(sel[2].entity_id, "c3", "slack still guards the typed slot");
    }

    #[test]
    fn test_freshness_refreshes_changed_chunk_and_flags_deleted_file() {
        // a NON-git project (walk_files) with a .thor marker
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("Proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(".thor"), "Proj\n").unwrap();
        std::fs::write(proj.join("notes.md"), "how the widget frobnicator does work: magic pipeline v1\n").unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            crate::ingest::ingest_repos(&mut store, &[proj.clone()], "test", None).unwrap();
        }
        let raw = format!(
            r#"{{"prompt":"widget frobnicator work","cwd":{}}}"#,
            serde_json::to_string(&proj.to_string_lossy()).unwrap()
        );
        // unchanged file: no freshness tag
        let out = injection_for_hook_json(&db, &raw).expect("chunk injects");
        assert!(out.contains("Proj:notes.md#0"), "the chunk is the hit: {out}");
        assert!(!out.contains("[refreshed]") && !out.contains("[stale?]"), "unchanged -> no tag: {out}");

        // file edited after ingest: the LIVE text is injected, tagged [refreshed]
        std::fs::write(proj.join("notes.md"), "how the widget frobnicator does work: magic pipeline v2 LIVE\n").unwrap();
        let out = injection_for_hook_json(&db, &raw).expect("still injects");
        assert!(out.contains("[refreshed]"), "changed file must be tagged: {out}");
        assert!(out.contains("v2 LIVE"), "the agent sees today's content, not the snapshot: {out}");
        assert!(!out.contains("v1"), "the stale snapshot text is not shown: {out}");

        // file deleted: warn instead of presenting dead code as fresh
        std::fs::remove_file(proj.join("notes.md")).unwrap();
        let out = injection_for_hook_json(&db, &raw).expect("still injects (stored body)");
        assert!(out.contains("[stale?]"), "a vanished file must be flagged: {out}");
    }

    #[test]
    fn test_thor_silent_flag_silences_the_courier() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        let raw = r#"{"prompt":"how does the deploy watcher work"}"#;
        assert!(injection_for_hook_json(&db, raw).is_some(), "normally the courier injects");
        std::fs::write(dir.path().join("THOR-SILENT.flag"), "").unwrap();
        assert!(
            injection_for_hook_json(&db, raw).is_none(),
            "THOR-SILENT.flag next to the db must silence the courier"
        );
    }

    #[test]
    fn test_thor_primary_flag_marks_the_phase() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        let raw = r#"{"prompt":"how does the deploy watcher work"}"#;
        let shadow = injection_for_hook_json(&db, raw).expect("shadow injects");
        assert!(!shadow.contains("THOR-PRIMARY"), "no flag -> no phase marker");
        std::fs::write(dir.path().join("THOR-PRIMARY.flag"), "").unwrap();
        let primary = injection_for_hook_json(&db, raw).expect("primary injects");
        assert!(
            primary.contains("THOR-PRIMARY"),
            "THOR-PRIMARY.flag must mark the phase in the header"
        );
    }
}
