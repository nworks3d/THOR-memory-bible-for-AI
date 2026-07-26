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
/// Per-prompt injection budget in chars (~4 chars/token: ~8000 chars is the
/// chosen ~2000-token ceiling). A hard ceiling, never a target - typical
/// prompts stay in the hundreds; report the measured average alongside it.
const PROMPT_BUDGET_CHARS: usize = 8000;

/// Words that, when they make up the WHOLE prompt, mean "no recall worth doing"
/// (acks / git verbs / greetings). Lives in `vocab` with its sibling lists.
use crate::vocab::TRIVIAL_WORDS;

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

/// Cross-channel deduplication: ON by default since 2026-07-25.
///
/// A PINNED fact is already delivered in FULL by another channel: the
/// `<thor-brief>` re-injects the whole pin list at every session start and right
/// after each compaction. Serving it again per prompt spends one of three slots
/// on text that is verbatim in the agent's context already - measured on 100
/// real prompts as 13.9% of all served slots, and the bulk of the remaining
/// courier noise (hovering work rules).
///
/// `demote` is the shipped default: pinned hits move to the BACK of the pool, so
/// they still serve when nothing else matched (the hist-demote semantics, which
/// cost zero drift catch). It cut the duplication by 93% with the drift metric
/// byte-identical in every arm; `drop` cut the last 7% by giving up that
/// fallback and bought nothing measurable, so it stays a measurement arm only.
///
/// Off switch is a file op, never a code change: `THOR-NO-PIN-DEDUP.flag` next
/// to the store restores the old behaviour. `THOR_EXP_PINDEDUP=drop|off`
/// overrides the mode for measurement runs.
///
/// Id matching is prefix-tolerant: a pin may be recorded with or without the
/// `<project>:` prefix, and a scope change must not silently un-dedup a rule.
///
/// Since 2026-07-26 the same demotion covers facts tagged `guarded`: a pin is
/// only one way for another layer to carry a fact - a guard anchor and a
/// response-rulebook rule deliver at the moment of action too, which is stricter
/// than hovering in every prompt. Same off switch, same "demoted, never dropped".
fn pin_dedup(survivors: Vec<RecallHit>, db: &Path) -> Vec<RecallHit> {
    if flag_present(db, "THOR-NO-PIN-DEDUP.flag") {
        return survivors;
    }
    let mode = match std::env::var("THOR_EXP_PINDEDUP").as_deref() {
        Ok("off") => return survivors,
        Ok("drop") => "drop",
        _ => "demote",
    };
    let pins = crate::ledger::read_pins(db);
    let local = |id: &str| id.rsplit(':').next().unwrap_or(id).to_string();
    let pinned_local: Vec<String> = pins.iter().map(|p| local(p)).collect();
    // A fact is "already carried" when ANOTHER layer delivers it at the moment it
    // matters: the brief re-injects every PIN in full, and a fact tagged `guarded`
    // has its own gate - an anchor the moment-of-action guard fires on, or a rule
    // in the response rulebook that blocks the reply. Measured 2026-07-26 on 100
    // real prompts: three of the four biggest remaining noise sources were exactly
    // such facts, hovering per-prompt while a deterministic layer already had them
    // covered. Demoted, never dropped: a sole match still serves, and deliberate
    // recall is untouched.
    let is_carried = |h: &RecallHit| {
        pinned_local.iter().any(|p| *p == local(&h.entity_id))
            || crate::footer::has_tag(&h.body, GUARDED_TAG)
    };
    if mode == "drop" {
        return survivors.into_iter().filter(|h| !is_carried(h)).collect();
    }
    let (carried, rest): (Vec<RecallHit>, Vec<RecallHit>) =
        survivors.into_iter().partition(is_carried);
    rest.into_iter().chain(carried).collect()
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
    // Warm-first: a running `thor daemon` answers with the IDENTICAL decision
    // (shared core) while skipping process/store/model startup. Any daemon
    // failure falls back to the in-process cold path - exactly the previous
    // behavior plus a bounded probe.
    match crate::daemon_client::try_inject(db, &raw) {
        Some(crate::daemon_client::DaemonReply::Inject(block)) => return Some(block),
        Some(crate::daemon_client::DaemonReply::Silent) => return None,
        None => {}
    }
    injection_for_hook_json(db, &raw)
}

/// Everything THOR does at the compaction boundary (`thor session-start` with
/// source == "compact"): render the recovery message FROM the served map, THEN
/// clear the courier and guard session state so everything relevant may inject
/// again into the now-empty window. Order matters: the clear wipes the served
/// map the message is built from. Returns None when silenced
/// (THOR-SILENT.flag) - the state clear still happens, it is hygiene tied to
/// the compaction itself, not a nudge.
///
/// WHY here and not a PreCompact hook: Claude Code does not deliver PreCompact
/// stdout to the model - not into context, not into the transcript, and
/// additionalContext is unsupported for that event (verified 2026-07-23
/// against the hooks docs and a live session transcript). The PreCompact
/// registration THOR shipped 2026-07-10 printed into the void on every real
/// compaction; SessionStart stdout is context by contract, and source ==
/// "compact" fires exactly at the boundary. Idea credit for the nudge itself:
/// Letta's memory-pressure warning.
pub fn compact_boundary(db: &Path, session_id: &str) -> Option<String> {
    let msg = if flag_present(db, "THOR-SILENT.flag") {
        None
    } else {
        Some(compact_recovery_message(db, session_id))
    };
    clear_session_ledger(db, session_id);
    crate::guard::clear_session_guard_seen(db, session_id);
    msg
}

/// The compaction-boundary advisory text: the capture nudge, plus - when the
/// courier's session row carries served memories - the judgment-debt list. The
/// list is the measured half (A/B 2026-07-22, 12 pairs): presenting the served
/// ids at a rest point took cold-hit settlement from 0% (ambient nudge alone,
/// all control runs) to 100% with zero wrong labels. Ambient asking does not
/// work; a one-time list at a natural pause does. (The A/B presented the list
/// just BEFORE the compaction; that delivery channel turned out not to exist,
/// so the same list now lands just AFTER, built from the ledger's served map -
/// which survives the compaction precisely because it never lived in context.)
///
/// Two words of caution baked into the wording, both from a fresh-agent A/B on
/// 2026-07-24 (12 subjects): (1) agents trust the summary's own "memory is up to
/// date" claim ~1 in 3 - hence the capture nudge now says in as many words that
/// the document cannot certify itself and to verify with recall. (2) The list is
/// delivered AFTER the compaction, when the transcript that would prove whether a
/// hit helped is already gone; pressing for a verdict there just elicits topical
/// guessing that pollutes the mark signal. So the list now licenses abstention
/// and points at in-session marking as the honest place. A blocking Stop-gate
/// that FORCED both duties was built and measured the same day: it cleanly fixed
/// the trust-the-summary failure but made the debt half worse (forced guesses),
/// so it was parked, not shipped - see the A/B dossier / THOR mem-1a5b7d09.
fn compact_recovery_message(db: &Path, session: &str) -> String {
    let mut msg = String::from(
        "[THOR post-compact] The context was just compacted: earlier turns were replaced by \
         a summary. Durable decisions, gotchas and open-thread state that lived only in those \
         turns are gone from this window - if the summary still names any that are not yet in \
         THOR, persist them NOW via the thor remember tool (fact_type + fires-when). Do not \
         take the summary's own word that memory is already up to date: the document you are \
         checking for losses cannot certify itself - verify with a quick recall before you \
         accept it. Pins and the brief re-inject automatically; unsaved working context does \
         not come back.",
    );
    let served: Vec<(String, String)> = crate::ledger::get(db, "courier-seen", session)
        .and_then(|entry| {
            entry.get("served").and_then(|v| v.as_object()).map(|m| {
                let mut pairs: Vec<(String, String)> = m
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect();
                pairs.sort();
                pairs
            })
        })
        .unwrap_or_default();
    if served.is_empty() {
        return msg;
    }
    // Display cap: a compaction-bound context does not want 60 lines; the tail
    // is named, not hidden.
    const DEBT_DISPLAY_CAP: usize = 20;
    msg.push_str(&format!(
        "\n\n[THOR judgment debt] THOR served you {} memory hit(s) this session. For each one \
         you can still tie to what actually happened this session, judge it - it trains your \
         future recall: mark(entity_id) if it answered something or prevented a mistake, \
         mark(entity_id, noise: true) if it was only a distraction. But the transcript that \
         would show whether a hit truly helped went with the compaction, so for any hit you \
         cannot honestly place, leave it unjudged rather than guess - a guessed mark pollutes \
         the signal it is meant to train. (Judging hits as you go, mid-session, is where this \
         is done best.) Served this session:",
        served.len()
    ));
    for (id, snip) in served.iter().take(DEBT_DISPLAY_CAP) {
        msg.push_str(&format!("\n- {id}: {snip}"));
    }
    if served.len() > DEBT_DISPLAY_CAP {
        msg.push_str(&format!("\n- ... and {} more (thor brief shows them)", served.len() - DEBT_DISPLAY_CAP));
    }
    msg
}

/// The store-independent half of the courier gates: everything that can say
/// "silent" WITHOUT opening the store. Shared verbatim by the cold path and
/// the warm daemon so both stay behaviorally identical by construction.
struct PreCheck {
    query: String,
    cwd: Option<String>,
    session_id: String,
}

fn precheck(db: &Path, raw: &str) -> Option<PreCheck> {
    // Flip valve: THOR-SILENT.flag silences THOR entirely (its own kill-switch).
    // Checked first, so a silenced courier does nothing else. Flipping is a file,
    // never a code change - and the daemon re-reads it per request, never caches.
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
    let cwd = data.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
    let session_id =
        data.get("session_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some(PreCheck { query, cwd, session_id })
}

/// Given the raw hook JSON and a db path, produce the injection block (or None).
/// Applies the same gates as the mimir hook: min length, whole-prompt-trivial,
/// prompt truncation, dedup, and a hard cap.
pub fn injection_for_hook_json(db: &Path, raw: &str) -> Option<String> {
    let pre = precheck(db, raw)?;
    // Store unreachable -> silent (the "hub-down -> exit 0" contract). Opening
    // creates an empty store if none exists, which simply yields no hits.
    let store = EventStore::new(db).ok()?;
    injection_with_store(&store, db, &pre, None)
}

/// Warm entry point for the daemon's /inject handler: the caller supplies an
/// already-open store. Same precheck, same core - warm and cold are identical
/// by construction (flags and the session ledger are files/sidecars re-read
/// fresh on every call, never cached in the daemon).
pub fn injection_for_hook_json_warm(store: &EventStore, db: &Path, raw: &str) -> Option<String> {
    let pre = precheck(db, raw)?;
    injection_with_store(store, db, &pre, None)
}

/// As [`injection_for_hook_json_warm`], but the daemon passes its resident
/// `WarmRecall` so the per-prompt fold is skipped. `warm = None` is identical to
/// the cold hook path.
#[cfg(feature = "semantic")]
pub fn injection_for_hook_json_resident(
    store: &EventStore,
    db: &Path,
    raw: &str,
    warm: &mut crate::recall::WarmRecall,
) -> Option<String> {
    let pre = precheck(db, raw)?;
    injection_with_store(store, db, &pre, Some(warm))
}

fn injection_with_store(
    store: &EventStore,
    db: &Path,
    pre: &PreCheck,
    #[cfg(feature = "semantic")] warm: Option<&mut crate::recall::WarmRecall>,
    #[cfg(not(feature = "semantic"))] warm: Option<&mut ()>,
) -> Option<String> {
    let query = pre.query.clone();
    // Project isolation: recall inside project A must not surface project B's code
    // OR its memories. Derive the project from the hook cwd (a `.thor` marker or git
    // walk-up, no subprocess); the CORE recall then scopes to that project + the
    // always-in-scope global tier. A projectless cwd (scratch dir) -> global-only,
    // so auto-injection never re-imports another project's clutter.
    let cwd = pre.cwd.clone();
    let session_id = pre.session_id.as_str();
    let project = cwd.as_deref().and_then(|c| crate::repo::project_key(Path::new(c)));
    let scope = RecallScope::current(project.clone());
    // No path boosting on the courier pool: drift preventers are memories, and
    // the file-stem lift measurably displaced them here (live replay A-B).
    let pool = recall_for_warm(db, &store, &query, &scope, POOL_HITS, false, warm);

    // Scope-pointer arm (env `THOR_EXP_SCOPEHINT=1`), taken from the pool BEFORE
    // the coverage gate - which is exactly where a pointer dies: it carries no
    // domain content of its own, so it can never "cover" the query. Measured on
    // 20 blind general-chat prompts: only 30% got told which scope holds the
    // answer, i.e. scoping domain knowledge out of the global tier made it
    // unreachable from a normal chat unless the wording happened to match.
    // `registry` is the shipped default: ONE rare-token lookup on the pointer
    // tag returns the whole registry, independent of how the prompt is worded.
    // The `pool` arm (read the pointers out of the ranked pool) is kept as the
    // measurement arm that proved ranking cannot do this job: 30% -> 35%
    // reachable, because a pointer with no lexical overlap is never in the
    // top-8. Off switch is a file op: THOR-NO-SCOPE-HINT.flag.
    let hint_mode = match std::env::var("THOR_EXP_SCOPEHINT").as_deref() {
        _ if flag_present(db, "THOR-NO-SCOPE-HINT.flag") => "off",
        Ok("off") => "off",
        Ok("pool") => "pool",
        _ => "registry",
    };
    let hint_candidates: Vec<RecallHit> = match hint_mode {
        "pool" => scope_hints(pool.iter().cloned(), &query, project.as_deref()),
        "registry" => {
            let regs = crate::recall::recall_scoped(&store, SCOPE_TAG, REGISTRY_LIMIT, &scope)
                .unwrap_or_default();
            scope_hints(regs.into_iter(), &query, project.as_deref())
        }
        _ => Vec::new(),
    };

    // Silence threshold: an OR-fallback pool (only some query words matched) is
    // gated on real term coverage, so "best of an all-weak pool" is silence, not
    // three confident-looking noise lines. Strict-AND/semantic-evidence hits
    // (matched_and) pass as-is.
    let pool: Vec<RecallHit> = pool
        .into_iter()
        // A scope pointer is served by the hint channel or not at all: letting it
        // also win a ranked slot would spend a full-body slot on a signpost, and
        // it can pass the trigger gate on two matched keywords. With the hint
        // channel switched off it stays an ordinary fact, exactly as before.
        .filter(|h| hint_mode == "off" || !crate::footer::has_tag(&h.body, SCOPE_TAG))
        .filter(|h| {
            h.matched_and
                || crate::recall::covers_query(&h.body, &query)
                // Author-declared triggers live in the footer, which the
                // coverage gate deliberately no longer reads - specific
                // trigger evidence (two terms, or one identifier/path) is its
                // own authorization, same rule as the below-floor rescue.
                || crate::recall::trigger_authorizes(&h.body, &query)
        })
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
        // Nothing matched IN scope - which is precisely the general chat that
        // cannot reach a scoped fact. A pointer alone is then the whole answer.
        return hint_only_block(db, project.as_deref(), &hint_candidates);
    }
    let pool_top_rev = pool[0].rev.clone();
    let survivors: Vec<RecallHit> =
        pool.into_iter().filter(|h| !ledger.suppressed(h)).collect();

    // Experiment (THOR-EXP-HIST-DEMOTE.flag next to the store): a fact whose
    // opening declares ITSELF a historical record loses its injection slot to a
    // live fact of COMPARABLE strength. Demotion, not exclusion: demoted hits
    // keep their relative order at the BACK of the pool and still serve when
    // nothing live matched; the deliberate surfaces (MCP/CLI recall) are
    // untouched. Chunks are exempt - doc text quoting the word is not a
    // self-declaration. Flag absent (the default) = byte-identical behavior;
    // flipping is a file op.
    let survivors: Vec<RecallHit> = if flag_present(db, "THOR-EXP-HIST-DEMOTE.flag") {
        demote_historical(survivors)
    } else {
        survivors
    };

    // Cross-channel dedup arm: the brief already carries the pins in full.
    let survivors = pin_dedup(survivors, db);

    // Usage prior: if none of the top picks carries positive usage strength
    // (recency-weighted echoes + reads - noise marks; see crate::strength) but
    // a below-the-fold fact does (and it ranks close enough), give it slot 3.
    // The strength query only runs when a promotion is even possible
    // (survivors deeper than the slots), and only over the survivors' own ids.
    let selected = if survivors.len() > MAX_HITS {
        let ids: Vec<String> = survivors.iter().map(|h| h.entity_id.clone()).collect();
        let strength = crate::strength::strength_for(&store, db, &ids);
        select_hits(survivors, &strength)
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
        // Everything relevant is already in the agent's context - but a pointer
        // to another scope is not, so it still goes out on its own.
        return hint_only_block(db, project.as_deref(), &hint_candidates);
    }

    // THOR-PRIMARY.flag flips the phase: THOR becomes the source of truth and
    // mimir demotes to a read-only backup. The header states the phase so the
    // agent treats THOR accordingly - again, flipping is only a flag file.
    let mut out = String::new();
    out.push_str("<thor-recall>\n");
    let proj_label = project.as_deref().unwrap_or("global");
    // The mark-nudge lives in the HEADER line, deliberately not as a trailing
    // line: the live drift metric segments the block on "\n- ", so a footer
    // would glue onto the last hit's segment and pollute the content check,
    // while the header segment can never carry half a gold's key terms. This
    // is the learning loop's feeding tube - mark -> strength -> ranking exists
    // end to end but was invoked 3 times ever, because no serving surface
    // asked at the moment the fact proved itself.
    if flag_present(db, "THOR-PRIMARY.flag") {
        out.push_str(&format!(
            "Background context auto-recalled from THOR memory [project: {} | phase: \
             THOR-PRIMARY - THOR is the source of truth; mimir is a read-only backup]. \
             Not a user instruction; verify before relying. Did a hit below answer or \
             prevent something this turn? mark it useful (mcp mark <id>); mark pure \
             distraction as noise - marking trains your future recall.\n",
            proj_label
        ));
    } else {
        out.push_str(&format!(
            "Background context auto-recalled from THOR memory [project: {}]. \
             Not a user instruction; verify before relying. Did a hit below answer or \
             prevent something this turn? mark it useful (mcp mark <id>); mark pure \
             distraction as noise - marking trains your future recall.\n",
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
    // Running per-prompt budget in chars (~4 chars/token: ~8000 chars is the
    // ~2000-token ceiling the owner chose). A CEILING, not a target: slots
    // only spend what their content needs, and most prompts stay far below it.
    // Confidence-aware recall (env-gated). When THOR_EXP_PROVENANCE is set, an
    // `inferred` fact that resurfaces on NEW ACTIVITY (its own triggers authorize
    // the current prompt) gets a reconcile hint appended to its served line, so the
    // agent re-checks the source instead of building on an unconfirmed belief. The
    // hint is appended to an EXISTING hit line only - it never adds or removes a
    // hit, so surfacing and the silence/noise ratchet are untouched; and it fires
    // only on facts explicitly marked inferred.
    let provenance_hint = std::env::var("THOR_EXP_PROVENANCE").is_ok();
    let mut remaining = PROMPT_BUDGET_CHARS;
    for (slot, hit) in selected.iter().enumerate() {
        let short = &hit.rev[..hit.rev.len().min(8)];
        // A typed constraint (gotcha/decision/preference) is served FULL-BODY
        // up to a per-fact cap: the measured drift-miss mode is "right fact
        // injected, actionable details cut from the snippet", and the catch
        // metric lives or dies on those details. Chunks and untyped notes keep
        // the cheap windowed caps; slot order is untouched (a typed fact is
        // never promoted here - rank decides slots, the budget only decides
        // how much of the winning fact is shown).
        // EVERY hand-written memory serves full-body up to the per-fact cap:
        // the v5 diagnosis found note/idea facts (not gotcha/decision/
        // preference) losing their decisive detail to the small windows -
        // being hand-written, not chunked, is what makes a body worth serving
        // whole. Chunks keep windowed caps (widened for the drift-catch
        // class: most live replay misses were CHUNK golds whose decisive
        // lines fell outside the old 700/220 windows). The per-prompt budget
        // (8000 chars) stays the hard ceiling.
        let diverged = if hit.is_diverged { " [DIVERGED]" } else { "" };
        // ONE serving stack. The courier used to run its own snippet pipeline
        // (windowed caps: memories 1200, chunks 1200/500) next to the deliberate
        // path's serve_deliberate (memories full-body, chunks neighbor-stitched).
        // Measured on the same 73 drift scenarios, the deliberate form catches
        // more (72.6% vs 67.1% preventer-surfaced) from the SAME pool - the
        // courier's own comment above names the failure mode: right fact
        // injected, actionable details cut. So the courier now serves through
        // serve_deliberate too (which does its own freshness re-read), and the
        // per-prompt budget is the only limiter left: a serving that does not
        // fit what remains falls back to the query-centered window over the
        // same stitched text - never a blind truncation.
        // Each slot's ceiling is its FAIR SHARE of what remains, not a fixed
        // number: one long fact must never eat the whole budget, because
        // fragmentation is exactly the case where multiple facts need to
        // surface together. A short serving donates its unused share to the
        // slots after it; a long one is windowed at its share, never blindly
        // truncated. Derived from the budget, so there is no cap left to tune.
        let slots_left = selected.len() - slot;
        let fair_share = (remaining / slots_left.max(1)).max(220);
        let (fresh_tag, snip) = {
            // MEASUREMENT ARMS (THOR_EXP_SERVEFORM, temp): "compact" serves
            // every hit as a one-liner + expand pointer; "hybrid" keeps the top
            // slot full and compacts the rest. Unset (production) is the
            // existing full-serving path, byte-identical. Three session
            // reports asked for compactness against the 2026-07-13 measured
            // full-body win; this arm switch lets the A/B re-run on today's
            // store instead of settling opinion vs stale measurement.
            let compact_this = match std::env::var("THOR_EXP_SERVEFORM").as_deref() {
                Ok("compact") => true,
                Ok("hybrid") => slot > 0,
                _ => false,
            };
            if compact_this {
                let one: String = crate::footer::strip(&hit.body)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(200)
                    .collect();
                (String::new(), format!("{one}... (get {} for the full fact)", hit.entity_id))
            } else {
                let (tag, full) = serve_deliberate(
                    &store,
                    &hit.entity_id,
                    &hit.body,
                    &query,
                    project.as_deref(),
                    cwd.as_deref(),
                );
                if full.chars().count() <= fair_share {
                    (tag, full)
                } else {
                    (tag, crate::recall::snippet(&full, fair_share, &query))
                }
            }
        };
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
        remaining = remaining.saturating_sub(snip.chars().count());
        // Resurface-for-confirmation: an inferred fact coming back on new activity.
        let recon = if provenance_hint
            && crate::footer::provenance(&hit.body).as_deref() == Some("inferred")
            && crate::recall::trigger_authorizes(&hit.body, &query)
        {
            " [provenance: inferred - not yet confirmed by a test or file read; \
             new activity on this topic now - reconcile against the source before you rely on it]"
        } else {
            ""
        };
        out.push_str(&format!(
            "- {}{} {} ({}{}{}): {}{}\n",
            scope_tag, type_tag, hit.entity_id, short, diverged, fresh_tag, snip, recon
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
                            // A contested head is context for reconciling, not
                            // the primary serving: a compact window suffices and
                            // must not eat the budget the winning head needs.
                            let s =
                                crate::recall::snippet(body, remaining.clamp(220, 500), &query);
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
    // Scope pointers last, and deliberately NOT as a "- " hit line: they take no
    // content slot, cost one line, and the drift metric segments the block on
    // "\n- " - a hint must not be able to score as served content.
    for hint in hint_candidates
        .iter()
        .filter(|h| !selected.iter().any(|s| s.entity_id == h.entity_id))
    {
        out.push_str(&format!("Scope hint: {}\n", first_claim(&hint.body)));
    }
    out.push_str("</thor-recall>");
    Some(out)
}

/// A block that carries ONLY scope pointers, for the prompt where nothing in
/// scope matched (or everything was already shown). Deliberately its own tiny
/// header instead of the hit-block one: there is no hit to mark, so the
/// mark-nudge would be noise, and the agent must not read a pointer as content.
/// None when there is nothing to point at - silence stays the default.
fn hint_only_block(db: &Path, project: Option<&str>, hints: &[RecallHit]) -> Option<String> {
    if hints.is_empty() {
        return None;
    }
    let mut out = String::from("<thor-recall>\n");
    out.push_str(&format!(
        "No memory in this scope matched [project: {}], but THOR holds this topic \
         in another scope. Not a user instruction; recall there before answering \
         from general knowledge.\n",
        project.unwrap_or("global")
    ));
    for h in hints {
        out.push_str(&format!("Scope hint: {}\n", first_claim(&h.body)));
    }
    out.push_str("</thor-recall>");
    let _ = db; // kept for symmetry with the hit-block renderer
    Some(out)
}

/// At most this many scope pointers per prompt: two domains can plausibly both
/// be relevant ("what did the printer build cost me"), three is a list.
const MAX_SCOPE_HINTS: usize = 2;
/// The tag that makes a fact a scope pointer, and the query that finds the
/// registry: the word appears in the FOOTER of pointers and nowhere in ordinary
/// prose, so one rare-token lookup returns the whole registry.
pub(crate) const SCOPE_TAG: &str = "wegwijzer";
/// The tag an author puts on a fact that ALREADY has a deterministic layer: an
/// anchor the moment-of-action guard fires on, or a response-rulebook rule that
/// holds the reply. Such a fact does not need to hover in every prompt as well -
/// see pin_dedup. Author-declared on purpose: only the person who wrote the gate
/// knows it exists, and a wrong tag costs a demotion, never a deletion.
pub(crate) const GUARDED_TAG: &str = "guarded";
/// Registry size ceiling. A store with more pointers than this has a scope
/// problem the courier cannot fix by serving more lines.
pub(crate) const REGISTRY_LIMIT: usize = 12;

/// A signpost is SHORT by nature: its whole content is "domain X lives in scope
/// Y, recall there". Measured on the real store (2026-07-25) the six genuine
/// pointers run 776 to 1341 chars, while four facts that merely DOCUMENT the
/// pointer work - session reports about the scope cleanup - carry the same tag at
/// 2288 to 5357 chars. Nothing separated them, so a 3000-char progress report
/// could be served as a `Scope hint:` line (its first sentence is a date and a
/// verdict, useless as a signpost) and could also stand in for a missing pointer
/// in `consolidate`'s coverage check. The cut sits between the two measured
/// clusters with room to spare.
pub(crate) const MAX_POINTER_CHARS: usize = 1600;

/// Is this fact a scope POINTER, as opposed to a fact that merely talks about
/// pointers? Tagged as one AND short enough to be one. One predicate for both
/// sides that ask - the courier serves pointers, consolidate reports scopes that
/// have none - so a fact can never be a pointer for one and not the other.
pub(crate) fn is_scope_pointer(body: &str) -> bool {
    crate::footer::has_tag(body, SCOPE_TAG) && body.chars().count() <= MAX_POINTER_CHARS
}

/// Which pointers this prompt should be told about: tagged as a pointer, enough
/// declared triggers matched for this session shape, and not already naming the
/// session's own project (a pointer to where you already are is a wasted line).
/// Shared with the MCP recall tool, which needs the identical rule: a chat on
/// the hosted connector has no courier at all, and that is exactly where a
/// scoped fact is unreachable.
pub(crate) fn scope_hints(
    candidates: impl Iterator<Item = RecallHit>,
    query: &str,
    project: Option<&str>,
) -> Vec<RecallHit> {
    // A projectless session (a general chat) is the case that cannot reach a
    // scoped fact at all, so one matched trigger earns the line there; inside a
    // project two are required - measured, see trigger_matches_scope.
    let need = if project.is_some() { 2 } else { 1 };
    candidates
        .filter(|h| is_scope_pointer(&h.body))
        .filter(|h| crate::recall::trigger_matches_scope(&h.body, query, need))
        // "Already your own project" is a PROSE claim, not a substring: a pointer
        // that merely cites a fact id from this project ([[Key:mem-...]]) is about
        // another domain and must still be offered. One shared rule with
        // consolidate's mirror-image check - see repo::prose_mentions_project.
        .filter(|h| !project.is_some_and(|p| crate::repo::prose_mentions_project(&h.body, p)))
        .take(MAX_SCOPE_HINTS)
        .collect()
}

/// The pointer's own first claim - where the scope is and how to reach it -
/// capped to one served line. Footer stripped, cut on the first sentence end
/// that leaves something substantial, else hard-capped.
pub(crate) fn first_claim(body: &str) -> String {
    let content = crate::footer::strip(body).trim().replace('\n', " ");
    let cut = content
        .char_indices()
        .filter(|(i, c)| *c == '.' && *i >= 80)
        .map(|(i, _)| i + 1)
        .next()
        .unwrap_or(content.len());
    let mut s: String = content.chars().take(cut.min(SCOPE_HINT_CHARS)).collect();
    if s.len() < content.len() {
        s.push_str(" ...");
    }
    s
}

/// One served line, not a served fact: enough to name the scope and the recall
/// to run, never enough to answer the question from.
const SCOPE_HINT_CHARS: usize = 300;

/// Rank slack for the typed-constraint slot: a deeper gotcha/decision/preference
/// may take slot 3 only when its bm25 strength is within this factor of slot 3's.
/// Same conservatism as the echo prior: a typed fact never displaces a clearly
/// stronger match, and slots 1-2 are never touched.
const TYPED_RANK_SLACK: f64 = 1.5;

/// Apply the THOR-EXP-HIST-DEMOTE reordering: a self-declared historical
/// memory hit moves to the back of the pool when a LIVE hit matches at least
/// as strongly - when the query's best evidence is live, the marginal slot is
/// better spent on another live fact than on a bygone record. But a historical
/// fact that is itself the STRONGEST lexical match keeps its position: the
/// query is then about the archived thing, and the archive IS the answer
/// (measured 2026-07-24: always-demote and slack-1.5 demotion both lost
/// live-drift gold seq 3035, an archive fact that was the asked-for answer in
/// a pool where every rival was equally-bygone mimir material that just does
/// not say so). No tuned constant: the tie-break is "a live hit >= mine
/// exists". bm25 ranks are negative (more negative = stronger; 0.0 =
/// dense-only), so a dense-only historical hit yields to any lexical live hit.
/// Original order is preserved on both sides of the split.
fn demote_historical(survivors: Vec<RecallHit>) -> Vec<RecallHit> {
    let hist = |h: &RecallHit| {
        !crate::repo::is_chunk_id(&h.entity_id) && crate::recall::is_historical(&h.body)
    };
    // Strongest live lexical rank in the pool (0.0 when there is none).
    let best_live = survivors.iter().filter(|h| !hist(h)).map(|h| h.rank).fold(0.0_f64, f64::min);
    let (keep, demoted): (Vec<RecallHit>, Vec<RecallHit>) = survivors
        .into_iter()
        // Keep in place: live, or historical while every live hit is a
        // strictly weaker match.
        .partition(|h| !hist(h) || best_live > h.rank);
    keep.into_iter().chain(demoted).collect()
}

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
fn select_hits(pool: Vec<RecallHit>, strength: &HashMap<String, f64>) -> Vec<RecallHit> {
    // Positive usage strength = proven useful on balance; a noise-marked fact
    // (strength <= 0) never earns the promotion, however often it was echoed.
    let echoed = |h: &RecallHit| strength.get(&h.entity_id).copied().unwrap_or(0.0) > 0.0;
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
    /// entity_id -> one-line snippet for every MEMORY served this session.
    /// Unlike `seen` this is NOT window-pruned: it is the judgment-debt list
    /// the pre-compact surface presents (measured 2026-07-22: a debt list at
    /// the rest point took cold-hit settlement from 0% to 100%; the ambient
    /// nudge alone settled nothing). Bounded by SERVED_CAP.
    served: HashMap<String, String>,
}

/// Upper bound on the per-session served-memories record. A session that
/// genuinely serves more distinct memories than this keeps the FIRST arrivals
/// (insertion refuses past the cap) - a bounded, predictable debt list beats
/// an unbounded sidecar row.
const SERVED_CAP: usize = 60;

/// One line of the debt list: the body without its footer, whitespace
/// collapsed, cut at 70 chars - just enough for the agent to recognize the
/// fact it saw earlier without re-serving the whole body.
fn served_snippet(body: &str) -> String {
    let stripped = crate::footer::strip(body);
    let one_line = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut snip: String = one_line.chars().take(70).collect();
    if one_line.chars().count() > 70 {
        snip.push_str("...");
    }
    snip
}

fn ledger_key(rev: &str, diverged: bool) -> String {
    // Keyed on rev + diverged so a divergence FLIP re-surfaces the same rev with
    // its new [DIVERGED] marker instead of being suppressed as "already shown".
    format!("{}|{}", rev, diverged)
}

impl SessionLedger {
    fn load(db: &Path, session_id: &str) -> Self {
        let mut this = SessionLedger {
            session_id: session_id.to_string(),
            count: 0,
            seen: HashMap::new(),
            served: HashMap::new(),
        };
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
            if let Some(served) = entry.get("served").and_then(|v| v.as_object()) {
                this.served = served
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
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
            // The judgment-debt record: memories only (repo chunks are managed
            // by ingest, marking them is not the learning loop).
            if !crate::repo::is_chunk_id(&h.entity_id)
                && (self.served.len() < SERVED_CAP || self.served.contains_key(&h.entity_id))
            {
                self.served.entry(h.entity_id.clone()).or_insert_with(|| served_snippet(&h.body));
            }
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
        let served: serde_json::Map<String, serde_json::Value> =
            self.served.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect();
        crate::ledger::upsert(
            db,
            "courier-seen",
            &self.session_id,
            &serde_json::json!({ "ts": now, "count": self.count, "seen": seen, "served": served }),
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

/// Wide query-focused window for repo chunks on the deliberate path (MCP/CLI
/// recall): matches what the benchmark's fused channel measures, so the
/// harness and production serve the same thing.
pub(crate) const DELIBERATE_CHUNK_CAP: usize = 500;
/// Full-body ceiling on the deliberate path: a served text at or under this
/// passes through VERBATIM instead of being query-windowed. Measured (v6 A-B,
/// 93 code questions): windowed snippets cut the answer out of the already-
/// found chunk (best-hit gold coverage 0.504/0.529 struct/behavior) while the
/// full chunk body reaches 0.531/0.577 - at or above the rival's full-body
/// serving. Sits above MAX_CHUNK_CHARS (1800) so every single chunk fits.
pub(crate) const DELIBERATE_FULL_BODY_CAP: usize = 2000;
/// Window budget once neighbor chunks are stitched in: the dominant TEST1
/// failure mode is an answer SPLIT across a chunk boundary, so the joined text
/// gets room for a window that spans the seam.
pub(crate) const DELIBERATE_STITCHED_CAP: usize = 900;
/// Wider window for depth-2 stitched MARKDOWN chunks: doc answers spread over
/// several short sections, and the joined text is up to five chunks long.
pub(crate) const DELIBERATE_STITCHED_DOC_CAP: usize = 1200;

/// The current live single-head body of an entity: None when absent, diverged
/// (a contested neighbor is not safe stitching material) or retracted.
/// pub(crate): the guard's auto-echo settlement reads served facts through the
/// same single-head/retracted discipline instead of reimplementing it.
pub(crate) fn live_head_body(store: &EventStore, entity_id: &str) -> Option<String> {
    let events = store.get_events_by_entity(entity_id).ok()?;
    if events.is_empty() {
        return None;
    }
    let heads = crate::cas::compute_head_sets(&events);
    let hs = heads.get(entity_id)?;
    if hs.heads.len() != 1 {
        return None;
    }
    let rev = hs.heads.iter().next()?;
    let ev = events.iter().find(|e| &e.this_hash == rev)?;
    if matches!(ev.kind, crate::event_store::EventKind::FactRetracted) {
        return None;
    }
    Some(ev.body.clone())
}

/// Sibling chunk ids of `entity_id` up to `depth` on each side, nearest first:
/// ([n-1, n-2, ...], [n+1, n+2, ...]).
fn neighbor_ids(entity_id: &str, depth: usize) -> Option<(Vec<String>, Vec<String>)> {
    let (prefix, n) = entity_id.rsplit_once('#')?;
    let n: usize = n.parse().ok()?;
    let before = (1..=depth).filter_map(|d| n.checked_sub(d).map(|p| format!("{prefix}#{p}"))).collect();
    let after = (1..=depth).map(|d| format!("{prefix}#{}", n + d)).collect();
    Some((before, after))
}

/// The repo-relative path inside a chunk id (`<project>:<rel>#<n>` -> `rel`).
/// None for a non-chunk id. What the file IS decides how it is served, so both
/// the stitch depth and the seam glue read it from here.
fn chunk_rel(entity_id: &str) -> Option<&str> {
    entity_id.split_once(':').map(|(_, rest)| rest.rsplit_once('#').map_or(rest, |(rel, _)| rel))
}

/// Stitching depth per chunk kind: markdown sections fragment answers across
/// MORE boundaries than code (a doc answer often spans several short
/// sections), so doc chunks pull two neighbors each side where code pulls one.
fn stitch_depth(entity_id: &str) -> usize {
    chunk_rel(entity_id).map_or(1, |rel| if crate::repo::is_crumb_doc(rel) { 2 } else { 1 })
}

/// Deliberate-path serving (MCP recall, CLI recall, benchmark harness): a
/// memory fact is served FULL-BODY (footer stripped, whitespace collapsed) -
/// multi-project measured 96.7% on full bodies while capped snippets measured
/// ~70%, and the agent asked for this hit explicitly. A repo chunk is
/// NEIGHBOR-STITCHED: the adjacent chunks of the same file are joined around
/// the (freshness-checked) hit so an answer spanning a chunk boundary serves
/// as one window instead of a truncated half. This replaces the old flat
/// 220-char cap, the single largest gap between what the benchmark measured
/// and what production served.
pub fn serve_deliberate(
    store: &EventStore,
    entity_id: &str,
    body: &str,
    query: &str,
    project: Option<&str>,
    cwd: Option<&str>,
) -> (String, String) {
    if !crate::repo::is_chunk_id(entity_id) {
        let full = crate::footer::strip(body).split_whitespace().collect::<Vec<_>>().join(" ");
        return (String::new(), full);
    }
    // Center chunk honors the freshness re-read exactly like fresh_snippet.
    let (tag, center) = match freshness(entity_id, body, project, cwd) {
        Freshness::Current => (String::new(), body.to_string()),
        Freshness::Refreshed(live) => (" [refreshed]".to_string(), live),
        Freshness::Stale => (" [stale?]".to_string(), body.to_string()),
    };
    // Stitch stored neighbors (live heads only; fail-open to the bare chunk).
    // Neighbors come from the store, not the live file, so the benchmark and
    // the MCP surface serve identically; a refreshed center with stored
    // neighbors is the accepted, documented asymmetry. Depth is per kind:
    // markdown pulls two neighbors each side, code one.
    let mut joined = crate::footer::strip(&center).trim().to_string();
    let mut stitched = false;
    let depth = stitch_depth(entity_id);
    // Seam glue. Chunks are cut on LINE boundaries, so joining code with a space
    // welds the previous chunk's last line onto the next chunk's first line and
    // the seam lands mid-statement (`}` `export function foo() {` on one line) -
    // unreadable for the agent, and invisible as a boundary to any line-based
    // reader. Prose is unharmed by a space (markdown reflows), so only source
    // files get the newline back.
    let glue = match chunk_rel(entity_id).is_some_and(crate::repo::is_source_file) {
        true => "\n",
        false => " ",
    };
    if let Some((before, after)) = neighbor_ids(entity_id, depth) {
        // nearest-first order: prepend walks outward, append walks outward
        for id in before {
            match live_head_body(store, &id) {
                Some(p) => {
                    joined = format!("{}{}{}", crate::footer::strip(&p).trim(), glue, joined);
                    stitched = true;
                }
                None => break, // a gap ends the contiguous run
            }
        }
        for id in after {
            match live_head_body(store, &id) {
                Some(n) => {
                    joined = format!("{}{}{}", joined, glue, crate::footer::strip(&n).trim());
                    stitched = true;
                }
                None => break,
            }
        }
    }
    // Full-body first (the measured v6 lever): the whole stitched text when it
    // fits, else the complete CENTER chunk - the ranker chose it, and a window
    // that cuts the answer out of it was the dominant judged code loss. Only a
    // pathologically oversized single chunk still gets the query window.
    if joined.chars().count() <= DELIBERATE_FULL_BODY_CAP {
        return (tag, joined);
    }
    let center_full = crate::footer::strip(&center).trim().to_string();
    if center_full.chars().count() <= DELIBERATE_FULL_BODY_CAP {
        return (tag, center_full);
    }
    let cap = match (stitched, depth) {
        (false, _) => DELIBERATE_CHUNK_CAP,
        (true, 1) => DELIBERATE_STITCHED_CAP,
        // Doc chunks: more joined text deserves a wider window, still bounded.
        (true, _) => DELIBERATE_STITCHED_DOC_CAP,
    };
    (tag, crate::recall::snippet(&joined, cap, query))
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
    // MUST match ingest's chunking byte-for-byte (repo::chunk_file dispatches
    // source files to the symbol-boundary chunker) or every chunk of a source
    // file reads as changed forever.
    let chunks = crate::repo::chunk_file(rel, &text, crate::repo::MAX_CHUNK_CHARS);
    if n >= chunks.len() {
        return Freshness::Stale;
    }
    // Rebuild the body EXACTLY as ingest would - including the heading-trail
    // crumb - or every markdown chunk would compare as "changed" forever.
    let crumb = if crate::repo::is_crumb_doc(rel) {
        crate::repo::heading_trails(&chunks).into_iter().nth(n).unwrap_or_default()
    } else {
        String::new()
    };
    let live = crate::repo::chunk_body(&chunks[n], project, rel, n, chunks.len(), &crumb);
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
    boost_paths: bool,
) -> Vec<RecallHit> {
    recall_for_warm(db, store, query, scope, limit, boost_paths, None)
}

/// As [`recall_for`], but a long-lived caller (the daemon) may pass its resident
/// `WarmRecall` to skip re-opening the sidecar and re-folding the log. `warm =
/// None` is the cold path, byte-for-byte unchanged. The warm state refreshes
/// itself against `store` before use, so this can only be faster, never staler.
pub(crate) fn recall_for_warm(
    db: &Path,
    store: &EventStore,
    query: &str,
    scope: &RecallScope,
    limit: usize,
    boost_paths: bool,
    #[cfg(feature = "semantic")] warm: Option<&mut crate::recall::WarmRecall>,
    #[cfg(not(feature = "semantic"))] warm: Option<&mut ()>,
) -> Vec<RecallHit> {
    #[cfg(feature = "semantic")]
    {
        if let Some(hits) = try_semantic_recall(db, store, query, scope, limit, boost_paths, warm) {
            return hits;
        }
        // Semantic path declined (no model/sidecar, cold embed daemon, empty
        // result): fall through to bm25, exactly as before.
    }
    #[cfg(not(feature = "semantic"))]
    let _ = (db, boost_paths, warm); // only the semantic path needs these
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
    boost_paths: bool,
    warm: Option<&mut crate::recall::WarmRecall>,
) -> Option<Vec<RecallHit>> {
    use crate::vectors::{default_vectors_path, VectorStore};

    let model_dir = crate::embed::default_model_dir()?;
    if !crate::embed::model_present(&model_dir) {
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
    // The symbol sidecar rides the same deliberate-only gate as path boosting;
    // absent or unopenable degrades to no bonus, never an error.
    let symbols = if boost_paths {
        crate::symbols::SymbolStore::open_default(db).ok()
    } else {
        None
    };
    // WARM: reuse the resident sidecar + fold (refreshed against the store
    // first). COLD: open the sidecar and fold, exactly as before. Both run the
    // SAME model-id gate - a sidecar from another model is stale either way.
    let hits = match warm {
        Some(w) => {
            if w.model_id().as_deref() != Some(crate::embed::MODEL_ID) {
                return None;
            }
            let (vecs, cache) = w.for_query(store);
            crate::recall::recall_fused_scoped_cached(
                store, query, &qvec, vecs, limit, crate::recall::FUSION_LAMBDA, scope, boost_paths,
                symbols.as_ref(), Some(cache),
            )
        }
        None => {
            let vecs = VectorStore::open(&vpath).ok()?;
            if vecs.model_id().as_deref() != Some(crate::embed::MODEL_ID) {
                return None; // sidecar built by a different model -> stale until rebuilt
            }
            crate::recall::recall_fused_scoped(
                store, query, &qvec, &vecs, limit, crate::recall::FUSION_LAMBDA, scope,
                boost_paths, symbols.as_ref(),
            )
        }
    };
    match hits {
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
    fn hist_demote_flag_moves_a_self_declared_historical_fact_behind_live_hits() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("hist.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // A live fact STRONGER than the historical one exists, so the
            // historical hit must yield - but only past live hits, never out.
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "mem-live-strong", None,
                    "deploy watcher deploy watcher deploy watcher regel: pull eerst",
                )
                .unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "mem-hist", None,
                    "HISTORISCH (2026-01-01): deploy watcher deploy watcher oude aanpak",
                )
                .unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "mem-live-weak", None,
                    "de deploy watcher van nu",
                )
                .unwrap();
        }
        let raw = r#"{"prompt":"hoe werkt de deploy watcher","cwd":"x","session_id":""}"#;

        // Flag OFF = baseline: rank order, historical in the middle.
        let off = injection_for_hook_json(&db, raw).expect("injects");
        assert!(
            off.find("mem-hist").unwrap() < off.find("mem-live-weak").unwrap(),
            "baseline sanity: the historical fact outranks the weak live one"
        );

        // Flag ON: a live hit matches at least as strongly, so the historical
        // fact moves behind ALL live hits - demoted, not dropped.
        std::fs::write(dir.path().join("THOR-EXP-HIST-DEMOTE.flag"), "x").unwrap();
        let on = injection_for_hook_json(&db, raw).expect("injects");
        assert!(on.contains("mem-hist"), "demotion never drops the hit");
        assert!(
            on.find("mem-live-weak").unwrap() < on.find("mem-hist").unwrap(),
            "the weaker live fact serves before the self-declared historical one"
        );
    }

    #[test]
    fn hist_demote_keeps_a_historical_fact_that_is_the_strongest_match() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("histtop.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // The query is about the ARCHIVED mechanics themselves: the
            // self-declared historical fact is the strongest match by far.
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "mem-archive", None,
                    "HISTORISCH ARCHIEF: sync watermark reset sync watermark reset sync watermark push",
                )
                .unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "mem-other", None,
                    "de sync van vandaag draait via ship en recv, geen watermark nodig",
                )
                .unwrap();
        }
        let raw = r#"{"prompt":"waarom pusht de sync de oude memories niet - watermark reset?","cwd":"x","session_id":""}"#;
        std::fs::write(dir.path().join("THOR-EXP-HIST-DEMOTE.flag"), "x").unwrap();
        let on = injection_for_hook_json(&db, raw).expect("injects");
        assert!(
            on.find("mem-archive").unwrap() < on.find("mem-other").unwrap(),
            "a historical fact that is the strongest match keeps its slot"
        );
    }

    #[test]
    fn provenance_hint_resurfaces_an_inferred_fact_only_when_the_flag_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("prov.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // An INFERRED fact whose fires-when vocabulary the prompt re-activates.
            let inf = format!(
                "the metrics server listens on port 9090\n\n{}",
                crate::footer::compose_full(
                    "gotcha", &[], "global", &["metrics".into(), "port".into()], &[], None, Some("inferred"),
                )
            );
            store.append_event("s", "l", "a", EventKind::FactCreated, "mem-inf", None, &inf).unwrap();
        }
        let raw = r#"{"prompt":"what port does the metrics server use","cwd":"x","session_id":""}"#;
        const HINT: &str = "reconcile against the source";

        // Flag OFF = baseline: the fact surfaces, but no reconcile hint.
        std::env::remove_var("THOR_EXP_PROVENANCE");
        let off = injection_for_hook_json(&db, raw).expect("injects");
        assert!(off.contains("mem-inf"), "the fact surfaces");
        assert!(!off.contains(HINT), "no reconcile hint when the flag is off");

        // Flag ON: an inferred fact resurfacing on new activity gets the hint.
        std::env::set_var("THOR_EXP_PROVENANCE", "1");
        let on = injection_for_hook_json(&db, raw).expect("injects");
        assert!(on.contains(HINT), "inferred + re-activated fact gets the reconcile hint");
        std::env::remove_var("THOR_EXP_PROVENANCE");
    }

    #[test]
    fn test_warm_path_matches_cold_path_and_shares_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("wp.db");
        seed(&db);
        let raw = r#"{"prompt":"how does the deploy watcher work","cwd":"x","session_id":""}"#;
        // Stateless (no session): warm and cold must agree byte-for-byte.
        let store = EventStore::new(&db).unwrap();
        let warm = injection_for_hook_json_warm(&store, &db, raw);
        let cold = injection_for_hook_json(&db, raw);
        assert_eq!(warm, cold, "warm and cold must be identical by construction");
        assert!(warm.is_some());
        // Ledger shared: a WARM call with a session id suppresses the COLD
        // repeat, proving both paths read/write the same thor-ledger.db.
        let raw_s = r#"{"prompt":"how does the deploy watcher work","cwd":"x","session_id":"ws1"}"#;
        let first = injection_for_hook_json_warm(&store, &db, raw_s);
        assert!(first.is_some(), "first injection fires");
        let second = injection_for_hook_json(&db, raw_s);
        assert!(
            second.map_or(true, |b| !b.contains("e1 (")),
            "the cold repeat within the suppression window must not re-inject the same rev"
        );
        // Fallback wrapper: with no daemon flag, the warm probe is a no-op and
        // the cold result is served unchanged.
        assert!(crate::daemon_client::try_inject(&db, raw).is_none());
    }

    #[test]
    fn test_serve_deliberate_stitches_neighbors_and_serves_memories_full() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("st.db");
        let mut store = EventStore::new(&db).unwrap();
        let mk = |i: usize, text: &str| {
            format!("{}\n\n[repo file | P/src/a.rs | chunk {}/3]", text, i + 1)
        };
        store.append_event("s", "l", "a", EventKind::FactCreated, "P:src/a.rs#0",
            None, &mk(0, "fn alpha() { begin_processing(); }")).unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, "P:src/a.rs#1",
            None, &mk(1, "fn beta() { let matrix_cache = build(); }")).unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, "P:src/a.rs#2",
            None, &mk(2, "fn gamma() { matrix_ensure_rebuild(); }")).unwrap();
        // Query terms split across the #1/#2 boundary: the stitched window must
        // span the seam that a bare-chunk snippet could never cover.
        let (_, snip) = serve_deliberate(
            &store, "P:src/a.rs#1",
            &mk(1, "fn beta() { let matrix_cache = build(); }"),
            "matrix_cache ensure rebuild", None, None,
        );
        assert!(snip.contains("matrix_cache"), "center content served: {snip}");
        assert!(snip.contains("matrix_ensure_rebuild"), "next-chunk content stitched in: {snip}");
        assert!(!snip.contains("[repo file"), "footers never reach the served window: {snip}");
        // The seam must be a LINE break, not a space: chunks are cut on line
        // boundaries, so a space welds `}` onto the next chunk's `fn ...` and
        // the served code is broken mid-statement.
        for line in snip.lines() {
            assert!(
                !(line.contains('}') && line.contains("fn ") && line.matches("fn ").count() > 0
                    && line.trim_start().starts_with('}')),
                "chunk seam welded two lines together: {line:?}"
            );
        }
        assert!(
            snip.contains("build(); }\nfn gamma"),
            "code chunks must be stitched with a newline at the seam: {snip:?}"
        );

        // A memory fact is served full-body, footer stripped, uncapped.
        let long = format!("GOTCHA: {}\n\n[memory/gotcha | tags: x | project: P]", "detail ".repeat(120));
        let (tag, full) = serve_deliberate(&store, "P:mem-x", &long, "detail", None, None);
        assert!(tag.is_empty());
        assert!(full.len() > 700, "memory body not truncated: {}", full.len());
        assert!(!full.contains("[memory/"), "footer stripped from the served body");
    }

    #[test]
    fn test_serve_deliberate_full_body_first_windows_only_when_oversized() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("fb.db");
        let mut store = EventStore::new(&db).unwrap();
        // A lone chunk under the cap serves VERBATIM: head and tail both
        // present even when the query only matches the middle (a window
        // would have cut them off).
        let body = format!(
            "fn head_marker() {{}}\n{}\nfn needle_fn() {{ special_needle(); }}\n{}\nfn tail_marker() {{}}\n\n[repo file | P/src/big.rs | chunk 1/1]",
            "// filler\n".repeat(30),
            "// filler\n".repeat(30),
        );
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "P:src/big.rs#0", None, &body)
            .unwrap();
        let (_, served) = serve_deliberate(&store, "P:src/big.rs#0", &body, "special_needle", None, None);
        assert!(served.contains("head_marker") && served.contains("tail_marker"),
            "chunk under the full-body cap serves complete: {} chars", served.len());
        // An OVERSIZED stitched join falls back to the complete CENTER chunk,
        // never a window that could cut the ranked answer out.
        let huge = |name: &str| format!(
            "fn {name}() {{}}\n{}\n\n[repo file | P/src/wide.rs | chunk n/3]",
            "// pad\n".repeat(400),
        );
        for (i, name) in ["left", "center_answer", "right"].iter().enumerate() {
            store
                .append_event("s", "l", "a", EventKind::FactCreated,
                    &format!("P:src/wide.rs#{i}"), None, &huge(name))
                .unwrap();
        }
        let (_, served) = serve_deliberate(&store, "P:src/wide.rs#1", &huge("center_answer"), "center_answer", None, None);
        assert!(served.contains("center_answer"), "center content survives: {served}");
        assert!(served.chars().count() <= DELIBERATE_FULL_BODY_CAP,
            "oversized join stays bounded: {}", served.chars().count());
    }

    #[test]
    fn test_doc_chunks_stitch_two_neighbors_each_side() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("md.db");
        let mut store = EventStore::new(&db).unwrap();
        for i in 0..5 {
            let body = format!(
                "section {} text over topic-{}\n\n[repo file | P/docs/guide.md | chunk {}/5 | crumb: Guide]",
                i, i, i + 1
            );
            store
                .append_event("s", "l", "a", EventKind::FactCreated, &format!("P:docs/guide.md#{i}"), None, &body)
                .unwrap();
        }
        assert_eq!(stitch_depth("P:docs/guide.md#2"), 2, "markdown stitches depth 2");
        assert_eq!(stitch_depth("P:src/a.rs#2"), 1, "code stitches depth 1");
        let (_, snip) = serve_deliberate(
            &store, "P:docs/guide.md#2",
            "section 2 text over topic-2\n\n[repo file | P/docs/guide.md | chunk 3/5 | crumb: Guide]",
            "topic-0 topic-4", None, None,
        );
        assert!(snip.contains("topic-0"), "outermost previous section reachable: {snip}");
        assert!(snip.contains("topic-4"), "outermost next section reachable: {snip}");
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
    fn test_fair_share_budget_one_long_fact_cannot_starve_the_rest() {
        // The unified serving stack serves full bodies, so a single long fact
        // could eat the whole prompt budget - and fragmentation is exactly the
        // case where MULTIPLE facts must surface together. Each slot is capped
        // at its fair share of what remains: the long fact gets windowed, the
        // short ones still serve whole.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let long =
                format!("deploy watcher rule number one {}", "padding words here ".repeat(600));
            for (eid, body) in [
                ("mem-long", long.as_str()),
                ("mem-two", "deploy watcher rule number two: never restart during a build"),
                ("mem-three", "deploy watcher rule number three: check the flag file first"),
            ] {
                store.append_event("s", "l", "a", EventKind::FactCreated, eid, None, body).unwrap();
            }
        }
        let raw = r#"{"prompt":"deploy watcher rule","cwd":"x","session_id":"s1"}"#;
        let out = injection_for_hook_json(&db, raw).expect("should inject");
        for id in ["mem-long", "mem-two", "mem-three"] {
            assert!(out.contains(id), "{id} must be present in the injection");
        }
        assert!(out.contains("never restart during a build"), "short fact 2 serves whole");
        assert!(out.contains("check the flag file first"), "short fact 3 serves whole");
        assert!(
            out.chars().count() < 10_000,
            "the ~11k-char fact was windowed to its share, not served whole ({} chars)",
            out.chars().count()
        );
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
    fn test_scope_hint_serves_a_pointer_without_taking_a_content_slot() {
        // A scope pointer holds no domain content, so it can never pass the
        // coverage gate - it is served as a trailing hint line instead, and must
        // not be able to score as a hit (the drift metric segments on "\n- ").
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "wegwijzer-geld", None,
                    "SCOPE POINTER: all finance facts live in project-scope Investments - \
                     recall there with project:\"Investments\".\n\n\
                     [memory/preference | tags: wegwijzer scope | fires-when: broker dividend ETF]",
                )
                .unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "e-content", None,
                    "the deploy watcher unpacks the tarball and rebuilds the container",
                )
                .unwrap();
        }
        // Projectless session (a general chat): ONE matched trigger is enough.
        let chat = r#"{"prompt":"what dividend do I get per share","cwd":"x"}"#;
        let out = injection_for_hook_json(&db, chat).expect("the pointer alone justifies a block");
        assert!(out.contains("Scope hint: SCOPE POINTER"), "hint served: {out}");
        assert!(
            !out.contains("- [global] wegwijzer-geld"),
            "a hint must never occupy a ranked hit line: {out}"
        );

        // Inside a project TWO terms are required: one loose word is noise there.
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(".thor"), "SomeProject\n").unwrap();
        let one = format!(
            r#"{{"prompt":"what dividend do I get per share","cwd":{}}}"#,
            serde_json::to_string(&proj.to_string_lossy()).unwrap()
        );
        assert!(
            injection_for_hook_json(&db, &one).is_none_or(|o| !o.contains("Scope hint")),
            "one trigger inside a project must stay silent"
        );
        let two = format!(
            r#"{{"prompt":"which broker pays that dividend","cwd":{}}}"#,
            serde_json::to_string(&proj.to_string_lossy()).unwrap()
        );
        let out2 = injection_for_hook_json(&db, &two).expect("two triggers earn the hint");
        assert!(out2.contains("Scope hint:"), "two triggers inside a project fire: {out2}");

        // A long fact carrying the same tag is a REPORT about pointers, not a
        // pointer: measured 2026-07-25, the genuine ones run under 1400 chars and
        // three session reports about the scope cleanup carried the tag at 2288+.
        // Serving one as a hint line yields a date and a verdict, not a signpost.
        {
            let long = dir.path().join("long.db");
            let mut s2 = EventStore::new(&long).unwrap();
            s2.append_event(
                "s", "l", "a", EventKind::FactCreated, "wegwijzer-report", None,
                &format!(
                    "SCOPE WORK REPORT: what we changed about dividend and broker facts today.{}\n\n\
                     [memory/decision | tags: wegwijzer scope | fires-when: broker dividend]",
                    " padding to push this body past the signpost length limit.".repeat(30)
                ),
            )
            .unwrap();
            drop(s2);
            assert!(
                injection_for_hook_json(&long, chat).is_none_or(|o| !o.contains("Scope hint")),
                "a long tagged report must never be served as a signpost"
            );
        }

        // The off switch is a file op, never a code change.
        std::fs::write(db.with_file_name("THOR-NO-SCOPE-HINT.flag"), "").unwrap();
        assert!(
            injection_for_hook_json(&db, chat).is_none_or(|o| !o.contains("Scope hint")),
            "THOR-NO-SCOPE-HINT.flag silences the hint"
        );
    }

    #[test]
    fn test_pin_dedup_arms_free_the_slot_but_keep_the_fallback() {
        // The brief already carries pinned facts in full, so the courier takes
        // them out of the slots (ON by default since 2026-07-25). `demote` - the
        // default - must still serve one when it is the ONLY match (the
        // fail-safe), `drop` must not, and the flag file must restore the old
        // behaviour.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            for i in 0..4 {
                store
                    .append_event(
                        "s", "l", "a", EventKind::FactCreated, &format!("e{i}"), None,
                        &format!("deploy watcher fact number {i}"),
                    )
                    .unwrap();
            }
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "pin-only", None,
                    "quokka telemetry ritual, nothing else mentions quokka",
                )
                .unwrap();
        }
        // e0 is both pinned and a strong match for the shared query.
        crate::ledger::mutate_pins(&db, |_| vec!["e0".to_string(), "pin-only".to_string()])
            .unwrap();
        let raw = r#"{"prompt":"deploy watcher","cwd":"x"}"#;
        let solo = r#"{"prompt":"quokka telemetry ritual","cwd":"x"}"#;

        std::env::remove_var("THOR_EXP_PINDEDUP");
        let demoted = injection_for_hook_json(&db, raw).expect("the default still injects");
        assert!(!demoted.contains("e0"), "a pin loses its slot to a live hit: {demoted}");
        let fallback = injection_for_hook_json(&db, solo).expect("the default keeps the fallback");
        assert!(fallback.contains("pin-only"), "sole match still serves by default: {fallback}");

        // Off switch: the pin is served again, exactly as before 2026-07-25.
        std::fs::write(db.with_file_name("THOR-NO-PIN-DEDUP.flag"), "").unwrap();
        let base = injection_for_hook_json(&db, raw).expect("flagged off still injects");
        assert!(base.contains("e0"), "THOR-NO-PIN-DEDUP.flag restores the pin: {base}");
        std::fs::remove_file(db.with_file_name("THOR-NO-PIN-DEDUP.flag")).unwrap();

        std::env::set_var("THOR_EXP_PINDEDUP", "drop");
        let dropped = injection_for_hook_json(&db, raw).expect("drop still injects");
        assert!(!dropped.contains("e0"), "drop removes the pin: {dropped}");
        assert!(
            injection_for_hook_json(&db, solo).is_none(),
            "drop has no fallback: a pin-only match means silence"
        );
        std::env::remove_var("THOR_EXP_PINDEDUP");
    }

    #[test]
    fn a_guarded_fact_steps_aside_for_a_hit_that_has_no_other_carrier() {
        // Measured 2026-07-26 on 100 real prompts: three of the four biggest
        // remaining noise sources were facts that a deterministic layer ALREADY
        // enforces (a guard anchor, a response-rulebook rule) and that the
        // per-prompt channel repeated anyway. The `guarded` tag says "another
        // layer carries me", and pin_dedup then treats the fact exactly like a
        // pin: moved to the back, never removed.
        //
        // Asserted on pin_dedup directly, not through the whole injection: an
        // end-to-end version of this passed with the feature switched OFF (the
        // ranker happened to order it right), so it proved nothing.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        EventStore::new(&db).unwrap();
        let hit = |id: &str, body: &str| RecallHit {
            entity_id: id.into(),
            rev: "r".into(),
            body: body.into(),
            kind: EventKind::FactCreated,
            is_diverged: false,
            rank: -1.0,
            project: None,
            fact_type: None,
            matched_and: true,
        };
        let guarded = hit(
            "e-guarded",
            "PUSH RULE: only push when the whole chain follows.\n\n\
             [memory/preference | tags: guarded push | anchors: git push]",
        );
        let live = hit("e-live", "the watcher unpacks the newest tarball");

        let order = |hits: Vec<RecallHit>| {
            pin_dedup(hits, &db).iter().map(|h| h.entity_id.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            order(vec![guarded.clone(), live.clone()]),
            vec!["e-live", "e-guarded"],
            "a guarded fact ranked first still serves behind the hit nothing else carries"
        );
        assert_eq!(
            order(vec![guarded.clone()]),
            vec!["e-guarded"],
            "demoted is not dropped: a sole match is still the answer"
        );

        // Off switch, same file op as the pin side.
        std::fs::write(db.with_file_name("THOR-NO-PIN-DEDUP.flag"), "").unwrap();
        assert_eq!(
            order(vec![guarded, live]),
            vec!["e-guarded", "e-live"],
            "THOR-NO-PIN-DEDUP.flag restores the pre-2026-07-26 order"
        );
        std::fs::remove_file(db.with_file_name("THOR-NO-PIN-DEDUP.flag")).unwrap();
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
        echo.insert("e4".to_string(), 2.0f64);

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
        echo2.insert("e2".to_string(), 1.0f64);
        let pool = vec![mk("e1", -9.0), mk("e2", -8.0), mk("e3", -6.0), mk("e4", -5.9)];
        let sel = select_hits(pool, &echo2);
        assert_eq!(sel[2].entity_id, "e3", "top already carries an echoed fact");

        // noise-drowned strength (<= 0) never earns the promotion, however
        // close it ranks - the mark --noise demotion in action
        let mut noisy = std::collections::HashMap::new();
        noisy.insert("e4".to_string(), -1.0f64);
        let pool = vec![mk("e1", -9.0), mk("e2", -8.0), mk("e3", -6.0), mk("e4", -5.9)];
        let sel = select_hits(pool, &noisy);
        assert_eq!(sel[2].entity_id, "e3", "negative strength = no promotion");
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
        echo.insert("e".to_string(), 1.0f64);
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

#[cfg(test)]
mod judgment_debt_tests {
    use super::*;
    use crate::event_store::EventKind;

    fn hit(id: &str, body: &str) -> RecallHit {
        RecallHit {
            entity_id: id.to_string(),
            rev: format!("rev-{id}"),
            body: body.to_string(),
            kind: EventKind::FactCreated,
            is_diverged: false,
            rank: -1.0,
            project: None,
            fact_type: None,
            matched_and: true,
        }
    }

    #[test]
    fn served_snippet_strips_the_footer_and_bounds_the_line() {
        let body = format!("the deploy waits for the lock file\n\n[memory/gotcha | tags: x | project: p]");
        let snip = served_snippet(&body);
        assert_eq!(snip, "the deploy waits for the lock file");
        let long = "word ".repeat(40);
        assert!(served_snippet(&long).chars().count() <= 73, "70 chars + ellipsis");
        assert!(served_snippet(&long).ends_with("..."));
    }

    #[test]
    fn record_collects_memories_but_never_chunks_and_survives_the_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut led = SessionLedger::load(&db, "sess-debt");
        led.record(&[
            hit("proj:mem-aaa", "a real memory fact"),
            hit("proj:src/main.rs#3", "fn main() {}"),
        ]);
        led.save(&db);
        let reloaded = SessionLedger::load(&db, "sess-debt");
        assert!(reloaded.served.contains_key("proj:mem-aaa"), "memory recorded");
        assert!(
            !reloaded.served.keys().any(|k| k.contains("#")),
            "repo chunks never enter the debt list: {:?}",
            reloaded.served
        );
    }

    #[test]
    fn served_record_is_not_window_pruned_and_respects_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut led = SessionLedger::load(&db, "sess-cap");
        // Far more prompts than SUPPRESS_WINDOW: `seen` prunes, `served` keeps.
        for i in 0..(SERVED_CAP + 10) {
            led.count += 1;
            led.record(&[hit(&format!("p:mem-{i:03}"), "body")]);
        }
        assert!(led.served.len() <= SERVED_CAP, "cap enforced: {}", led.served.len());
        assert!(led.served.contains_key("p:mem-000"), "first arrivals are kept past the window");
    }

    #[test]
    fn compact_recovery_message_lists_the_debt_only_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        // no session row -> capture nudge only
        let bare = compact_recovery_message(&db, "sess-x");
        assert!(bare.contains("[THOR post-compact]"));
        assert!(!bare.contains("judgment debt"), "no served memories, no debt block");
        // a session with served memories -> the list rides along
        let mut led = SessionLedger::load(&db, "sess-x");
        led.record(&[hit("proj:mem-abc", "the importer takes the same lock"), hit("proj:mem-def", "vans get tires")]);
        led.save(&db);
        let msg = compact_recovery_message(&db, "sess-x");
        assert!(msg.contains("[THOR judgment debt]"), "{msg}");
        assert!(msg.contains("2 memory hit(s)"), "{msg}");
        assert!(msg.contains("proj:mem-abc: the importer takes the same lock"), "{msg}");
        assert!(msg.contains("mark(entity_id, noise: true)"), "the repair path is spelled out: {msg}");
    }

    #[test]
    fn compact_recovery_message_caps_the_display_and_names_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut led = SessionLedger::load(&db, "sess-many");
        let hits: Vec<RecallHit> = (0..30).map(|i| hit(&format!("p:mem-{i:03}"), "b")).collect();
        led.record(&hits);
        led.save(&db);
        let msg = compact_recovery_message(&db, "sess-many");
        assert!(msg.contains("30 memory hit(s)"), "{msg}");
        assert!(msg.contains("and 10 more"), "the tail is named, not hidden: {msg}");
    }

    #[test]
    fn compact_boundary_reads_the_debt_before_clearing_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut led = SessionLedger::load(&db, "sess-order");
        led.record(&[hit("proj:mem-abc", "the importer takes the same lock")]);
        led.save(&db);
        // The message must be built from the served map even though the same
        // call clears it: read-before-clear is the invariant under test.
        let msg = compact_boundary(&db, "sess-order").expect("not silenced");
        assert!(msg.contains("proj:mem-abc"), "debt rendered from the pre-clear map: {msg}");
        assert!(
            crate::ledger::get(&db, "courier-seen", "sess-order").is_none(),
            "session ledger cleared for the fresh window"
        );
    }

    #[test]
    fn compact_boundary_is_silent_but_still_clears_under_thor_silent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut led = SessionLedger::load(&db, "sess-mute");
        led.record(&[hit("proj:mem-abc", "body")]);
        led.save(&db);
        std::fs::write(dir.path().join("THOR-SILENT.flag"), "").unwrap();
        assert!(compact_boundary(&db, "sess-mute").is_none(), "silenced -> no message");
        assert!(
            crate::ledger::get(&db, "courier-seen", "sess-mute").is_none(),
            "state hygiene still happens when silenced"
        );
    }
}
