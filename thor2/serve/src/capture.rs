//! Lane C - the capture guard (SPEC-ENFORCEMENT.md section 2): a durable
//! decision stated by the owner cannot end the turn unrecorded.
//!
//! NEVER for a subagent (2026-08-05 correction, `serve/src/bin/serve.rs`'s
//! `payload_is_from_a_subagent`): C1/C2/C3 all sit on hook surfaces
//! (`UserPromptSubmit`/`Stop`/`PreToolUse`) that DO fire inside a Task-tool
//! subagent. A subagent's own task prompt is written by an orchestrating
//! agent, not the owner, and is routinely full of the very words ("always",
//! "never", "from now on") this guard watches for - flagging it would
//! deadlock the subagent's own Stop on a "decision" the owner never made.
//! The gate lives in `serve.rs`, not here: this module's pure functions have
//! no notion of "subagent" at all, by design (they are payload-shape-free).
//!
//! Three moments, one mechanism:
//!
//! - C1 (UserPromptSubmit) - cheap and dumb about BLOCKING, deliberately:
//!   `flag_marker_text` records the raw prompt text and the store's current
//!   max event seq (`seq_at_flag`) for this session, unconditionally, for
//!   every non-empty prompt. No judge call happens here - a model call takes
//!   seconds, and paying that in front of every single prompt, before the
//!   model has even started answering, is far worse than paying it once at
//!   the end while the owner reads the reply (see `judge`'s own doc comment
//!   and `JUDGE-TRANSPORT.md`). C1 DOES still run the cheap, free,
//!   deterministic rulebook match (`decide_capture`) and stores its result
//!   as `Marker::provisional_tier` - available immediately, before Stop has
//!   judged anything, for C3 to warn on (never to block on - see C3 below
//!   and `sink_verdict`'s own doc comment for exactly why).
//! - C2 (Stop) - the AUTHORITATIVE classification happens HERE, once, at
//!   turn end. The caller (`serve/src/bin/serve.rs`'s `capture_stop_check`)
//!   reads the marker, checks whether the debt is already paid (`debt_paid`,
//!   unchanged from before - a later `fact_created`/`fact_revised` event
//!   proves it), and ONLY if unpaid asks the judge (`judge::run_judge`) what
//!   this prompt was. `decide_stop` below is the pure decision: a paid debt
//!   is always Allow, whatever the judge said; otherwise a `Block` verdict
//!   blocks turn end exactly once (`block_reason`, naming the judge's own
//!   citation), and a `Warn` verdict (or no verdict at all) never blocks.
//!   The judge is measured (2026-08-05, `AB-JUDGE-VS-RULEBOOK.md`) at
//!   100%/0% (catch/false-block) against the rulebook's 55%/11.4% on the
//!   SAME frozen hold-out - so the judge decides here, not the rulebook.
//! - The rulebook (`decide_capture`, `parse_rulebook`, unchanged matching
//!   logic) feeds TWO places, and may BLOCK turn end in NEITHER of them: as
//!   C1's `provisional_tier` above, and as C2's FALLBACK
//!   (`fallback_decision`), used exclusively when the judge is unreachable
//!   (no config, the command fails to spawn, it times out, or its answer
//!   does not parse). Both are capped to `Tier::Warn` NO MATTER WHAT TIER THE
//!   RULEBOOK RULE ITSELF CARRIES: the measured 11.4% false-block rate rules
//!   out ever blocking turn end - or a tool call - on the rulebook alone.
//!   This is the fail-open doctrine already in force for every other error
//!   path here, made explicit for the rulebook specifically.
//! - C3 (PreToolUse) - `sink_verdict` decides what happens to a Write/Edit
//!   at a known mirror sink (CLAUDE.md, BRAND.md, AGENTS.md, or a
//!   `decisions/` path segment), restored 2026-08-05 to fire WITHIN the same
//!   turn the decision was stated, not only after that turn's Stop: a
//!   `Some(Tier::Block)` `provisional_tier` (C1's cheap rulebook match, often
//!   available before Stop has even run) may only ever WARN - a visible,
//!   non-blocking nudge - never block, since the rulebook's own 11.4%
//!   false-block rate must never reach a real block through this path
//!   either. A REAL block requires the Stop-decided `tier` to be
//!   `Some(Tier::Block)` AND to have come from an actual judge verdict
//!   (`rule_id == JUDGE_RULE_ID`) - which, by construction, is the only way
//!   `tier` can ever BE `Some(Block)` at all (the Stop-time fallback can only
//!   ever produce `Warn`), so the `rule_id` check is a second line of
//!   defense, not the only one.
//!
//! A THIRD mode, added because the judge transport's own measured cost (9-16s
//! and roughly 3 cents per call on this machine, JUDGE-TRANSPORT.md's own
//! successor note - see OPTION4-IMPLEMENTATION.md) rules it out as an
//! always-on default: `CaptureMode::SelfAdjudicate`. No judge, no fallback
//! ladder - the assistant already in the loop judges itself, once, at Stop
//! (`decide_self_adjudicate`, `SELF_ADJUDICATE_MESSAGE`). Which of the three
//! modes (`Judge`/`SelfAdjudicate`/`Off`) is live is resolved by
//! `resolve_capture_mode` below, pure and explicit - never inferred silently
//! deep in a call chain. See OPTION4-IMPLEMENTATION.md for the full design
//! and how to switch modes.
//!
//! REUSES `respond::evaluate`/`respond::haystack` for the fallback matcher's
//! actual AND/OR/NOT/min_chars matching (SPEC-ENFORCEMENT.md section 0: "the
//! capture guard REUSES this matcher... it does not get a matcher of its
//! own") - see `rule_fires` below for how a `CaptureRule` is fed through it
//! without a second copy of that logic.
//!
//! Failure policy, identical to `respond`, `judge`, and B3
//! (SPEC-ENFORCEMENT.md 1.1): every error - unreadable/malformed rulebook,
//! unreadable/malformed marker file, a store that will not open or will not
//! read, an unreachable judge - is ALLOW or, at most, WARN, never BLOCK on
//! anything but a genuine, unpaid, judge-decided `Tier::Block`. Every
//! function below that can fail returns `None`/`false`/an unchanged
//! value on any such error, never panics, and the hook wiring
//! (`serve/src/bin/serve.rs`) never lets a write here block or slow a
//! prompt beyond the one configured judge timeout, enforced by `judge`
//! itself, never by this module.

use crate::judge::JudgeVerdict;
use crate::respond;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thor_core::event_store::EventKind;

/// Confidence tier for a capture decision, whichever side decided it (the
/// judge, or - only ever at `Warn` - the fallback rulebook).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Unambiguous normative statement - may reach a real BLOCK at Stop.
    /// Only ever produced by the judge; the fallback never returns this (see
    /// `fallback_decision`).
    Block,
    /// A weaker signal - may only ever WARN in spirit; never blocks turn end
    /// (see `decide_stop`).
    Warn,
}

impl Tier {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "block" => Some(Tier::Block),
            "warn" => Some(Tier::Warn),
            _ => None,
        }
    }
}

/// Which of the three Stop-time decision makers is live (OPTION4-IMPLEMENTATION.md).
/// Selection is explicit and inspectable by design: a person can find out
/// which mode governs a deployment by reading one small JSON file
/// (`guard-capture-mode.json`, see `default_mode_config_path`/
/// `parse_mode_config`) rather than having to infer it from code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// The external judge model decides (this file's own doc comment, and
    /// `judge`'s) - measured 100%/0% catch/false-block on a frozen hold-out,
    /// but costs several seconds and real money per call.
    Judge,
    /// No external process: the assistant already in the loop judges its own
    /// obligation, once, at Stop. The new DEFAULT whenever no judge is
    /// actually configured (`resolve_capture_mode`) - see
    /// `decide_self_adjudicate` and `SELF_ADJUDICATE_MESSAGE`.
    SelfAdjudicate,
    /// The capture guard's Stop and PreToolUse blocking are both fully
    /// disabled - a kill switch. Reachable ONLY through an explicit
    /// `guard-capture-mode.json`; nothing else can ever produce this value
    /// (see `resolve_capture_mode`).
    Off,
}

/// Parse `guard-capture-mode.json`'s one key: `{"mode": "judge"|"self"|"off"}`.
/// Missing file, malformed JSON, a missing `mode` key, or any value other
/// than the three recognized ones all yield `None` - fail-open, same stance
/// as every other config parser in this workspace: an unusable mode config is
/// never an error, it just means "no explicit choice was made here" and the
/// caller (`resolve_capture_mode`) falls through to its own default logic.
pub fn parse_mode_config(text: &str) -> Option<CaptureMode> {
    let v: Value = serde_json::from_str(text).ok()?;
    let mode = v.get("mode").and_then(|m| m.as_str())?.trim().to_ascii_lowercase();
    match mode.as_str() {
        "judge" => Some(CaptureMode::Judge),
        "self" => Some(CaptureMode::SelfAdjudicate),
        "off" => Some(CaptureMode::Off),
        _ => None,
    }
}

/// The mode config sits beside the store, same stance as every other
/// per-deployment config file here (`default_rulebook_path`,
/// `judge::default_judge_config_path`): a project directory could otherwise
/// plant one and silently flip the guard's mode for every session in it.
pub fn default_mode_config_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("guard-capture-mode.json")
}

/// The pure mode-selection rule (I/O - reading the two config files this
/// depends on - lives in `serve/src/bin/serve.rs::load_capture_mode`, so this
/// decision itself is unit-tested without spawning anything):
///
/// 1. An explicit, parseable `mode_config_text` always wins outright - this
///    is the ONLY way `Off` is ever reached, and the only way `Judge` or
///    `SelfAdjudicate` can be FORCED even when the other signal below would
///    suggest differently (e.g. forcing `self` even though a judge is
///    configured, for a side-by-side comparison).
/// 2. Otherwise (no mode config file, or it fails to parse): `Judge` when a
///    real, usable judge is configured (`judge_configured` - preserves every
///    deployment that only ever wrote `guard-judge-config.json` and never
///    knew mode selection existed at all), else `SelfAdjudicate` - the new
///    default now that the judge transport's own measured cost rules it out
///    as an always-on choice (see this file's own doc comment).
pub fn resolve_capture_mode(mode_config_text: Option<&str>, judge_configured: bool) -> CaptureMode {
    if let Some(mode) = mode_config_text.and_then(parse_mode_config) {
        return mode;
    }
    if judge_configured {
        CaptureMode::Judge
    } else {
        CaptureMode::SelfAdjudicate
    }
}

/// One capture rule, parsed from the FALLBACK rulebook
/// (`guard-capture-rulebook.json`). Same shape as `respond::Rule`
/// (all_of/any_of/none_of/min_chars), plus `tier` - though `tier` is now only
/// read for prioritizing which fallback rule wins when several fire; the
/// resulting `FlagDecision` is always forced to `Tier::Warn` before it can
/// ever reach a marker (`fallback_decision`).
#[derive(Debug, Clone)]
pub struct CaptureRule {
    pub id: String,
    pub tier: Tier,
    pub all_of: Vec<String>,
    pub any_of: Vec<String>,
    pub none_of: Vec<String>,
    pub min_chars: usize,
}

/// Parse the fallback rulebook. Any malformed JSON, or a rule missing/carrying
/// an invalid `tier`, yields no rule for that entry (fail-open, same stance
/// as `respond::parse_rules`): a broken rulebook silences the fallback,
/// never errors, and a rule this code cannot classify into a tier is dropped
/// rather than guessed at.
pub fn parse_rulebook(text: &str) -> Vec<CaptureRule> {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    let strs = |v: &Value, key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    arr.iter()
        .filter_map(|r| {
            let tier = r.get("tier").and_then(|v| v.as_str()).and_then(Tier::from_str)?;
            Some(CaptureRule {
                id: r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                tier,
                all_of: strs(r, "all_of"),
                any_of: strs(r, "any_of"),
                none_of: strs(r, "none_of"),
                min_chars: r
                    .get("min_chars")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .try_into()
                    .unwrap_or(usize::MAX),
            })
        })
        .collect()
}

/// Adapt one `CaptureRule` into a `respond::Rule` so it can be run through
/// `respond::evaluate` - the ONE matcher this guard reuses rather than
/// reimplementing (SPEC-ENFORCEMENT.md section 0). `reminder` is
/// `respond::Rule`'s own required field and is otherwise unused by this
/// guard, so it is filled with the rule's own id: `evaluate` returns exactly
/// the fired rules' `reminder` strings, which turns that return value into
/// exactly "which rule id fired", for free.
fn to_respond_rule(rule: &CaptureRule) -> respond::Rule {
    respond::Rule {
        id: rule.id.clone(),
        all_of: rule.all_of.clone(),
        any_of: rule.any_of.clone(),
        none_of: rule.none_of.clone(),
        min_chars: rule.min_chars,
        reminder: rule.id.clone(),
    }
}

/// Does this one rule fire against this haystack? A single-rule call into
/// `respond::evaluate` - never a second AND/OR/NOT/min_chars implementation.
fn rule_fires(rule: &CaptureRule, haystack_lower: &str) -> bool {
    !respond::evaluate(std::slice::from_ref(&to_respond_rule(rule)), haystack_lower).is_empty()
}

/// What a capture decision settled on: the tier, the quoted trigger text,
/// which rule or source decided it, and whether it was a `style-` rule (C4,
/// SPEC-ENFORCEMENT.md 2.4 - only ever set by the fallback rulebook; the
/// judge has no notion of this dimension, see `from_judge_verdict`).
#[derive(Debug, Clone, PartialEq)]
pub struct FlagDecision {
    pub tier: Tier,
    pub quote: String,
    pub rule_id: String,
    pub house_style: bool,
}

/// The rule id recorded on every judge-decided `FlagDecision`
/// (`from_judge_verdict`) - not a real rulebook rule, but `block_reason`
/// still needs SOMETHING to name (SPEC-ENFORCEMENT.md 1.1's "must be able to
/// print WHICH rule fired" invariant), and this makes the source of a block
/// visible in its own reason text.
pub const JUDGE_RULE_ID: &str = "judge";

/// The fallback rulebook's decision (SPEC-ENFORCEMENT.md 2.1's matcher,
/// unchanged): does this prompt match a fallback rule, and which one wins?
/// Rulebook order is priority order - the first `block`-tier match wins
/// outright; only when none fires do we fall back to the first `warn`-tier
/// match. `rulebook_text` being unreadable, or parsing to zero rules, yields
/// `None` (fail-open) - never a guess.
///
/// This function's OWN `tier` field is NOT what reaches a marker unfiltered
/// any more - see `fallback_decision`, which forces `Tier::Warn` regardless
/// of what is returned here. `decide_capture` is kept exactly as before
/// (including its own tests) so the matching semantics are provably
/// unchanged; only what callers are allowed to DO with a `Block` result
/// changed.
pub fn decide_capture(rulebook_text: Option<&str>, prompt: &str) -> Option<FlagDecision> {
    let rules = rulebook_text.map(parse_rulebook).unwrap_or_default();
    if rules.is_empty() {
        return None;
    }
    let haystack = respond::haystack(prompt);
    let mut warn_hit: Option<&CaptureRule> = None;
    for rule in &rules {
        if !rule_fires(rule, &haystack) {
            continue;
        }
        match rule.tier {
            Tier::Block => return Some(make_decision(rule, prompt)),
            Tier::Warn if warn_hit.is_none() => warn_hit = Some(rule),
            Tier::Warn => {}
        }
    }
    warn_hit.map(|rule| make_decision(rule, prompt))
}

/// The fallback path, wired at Stop ONLY when the judge produced no verdict
/// (SPEC: "the fallback becomes the fallback, and the fallback NEVER
/// BLOCKS"). Identical matching to `decide_capture`, but the tier is always
/// forced to `Warn` before it is returned - a false-block rate of 11.4%
/// (measured, `FRESH-HOLDOUT-RESULTS.md`) is not a risk this guard may take
/// on the rulebook alone, no matter which tier the rulebook's own JSON
/// assigns a rule.
pub fn fallback_decision(rulebook_text: Option<&str>, prompt: &str) -> Option<FlagDecision> {
    let mut decision = decide_capture(rulebook_text, prompt)?;
    decision.tier = Tier::Warn;
    Some(decision)
}

/// The NARROW trigger rulebook (`guard-capture-rulebook.example.json`,
/// embedded verbatim at compile time - this task's own hard constraint
/// forbids ever editing that file's content, and `include_str!` only ever
/// reads it) - the compiled-in DEFAULT set of trigger rules, used whenever no
/// real `guard-capture-rulebook.json` exists yet beside the store. This is
/// what makes "the narrow trigger is the default" a real, running behavior
/// rather than only a documentation claim: an undeployed, freshly-set-up
/// guard already behaves as if the narrow rulebook were active (measured:
/// ~70% catch on real decisions, ~13% false-fire rate on ordinary prompts -
/// OPTION4-IMPLEMENTATION.md), instead of silently matching nothing. This
/// matters most for `CaptureMode::SelfAdjudicate`: unlike the old
/// judge-only design (where the rulebook was a rarely-hit fallback), self
/// mode's ENTIRE trigger signal is this rulebook firing at all, so a guard
/// with nothing copied anywhere would otherwise never do anything at all.
const NARROW_TRIGGER_DEFAULT: &str = include_str!("../../guard-capture-rulebook.example.json");

/// Resolve the rulebook text actually used to match a prompt, wherever a
/// caller would otherwise read `default_rulebook_path` and get nothing: an
/// on-disk `guard-capture-rulebook.json` beside the store, when present,
/// ALWAYS wins outright - an explicit, deliberate override, the same stance
/// every other config file in this workspace takes. Its absence falls back
/// to the compiled-in narrow trigger set (`NARROW_TRIGGER_DEFAULT`) rather
/// than matching nothing. To switch to the wide trigger set instead (higher
/// catch, much higher false-fire rate - OPTION4-IMPLEMENTATION.md), copy
/// `guard-capture-trigger-wide.example.json` over `guard-capture-rulebook.json`
/// beside the store: any real on-disk file, of either kind, always beats this
/// compiled-in default.
pub fn resolve_rulebook_text(on_disk: Option<String>) -> String {
    on_disk.unwrap_or_else(|| NARROW_TRIGGER_DEFAULT.to_string())
}

/// Turn a judge verdict into the same shape the fallback path produces, so
/// `decide_stop`/`block_reason` never need to care which side decided.
/// `Allow` carries nothing to capture - `None`. `house_style` is always
/// `false`: the judge prompt has no notion of that dimension (it is a
/// rulebook-only enrichment, C4/SPEC-ENFORCEMENT.md 2.4), so a judge-decided
/// block never gets the extra "store the pointer, not the value" sentence.
pub fn from_judge_verdict(verdict: JudgeVerdict) -> Option<FlagDecision> {
    match verdict {
        JudgeVerdict::Allow => None,
        JudgeVerdict::Warn(citation) => Some(FlagDecision {
            tier: Tier::Warn,
            quote: truncate_chars(citation.trim(), QUOTE_MAX_CHARS),
            rule_id: JUDGE_RULE_ID.to_string(),
            house_style: false,
        }),
        JudgeVerdict::Block(citation) => Some(FlagDecision {
            tier: Tier::Block,
            quote: truncate_chars(citation.trim(), QUOTE_MAX_CHARS),
            rule_id: JUDGE_RULE_ID.to_string(),
            house_style: false,
        }),
    }
}

fn make_decision(rule: &CaptureRule, prompt: &str) -> FlagDecision {
    FlagDecision {
        tier: rule.tier,
        quote: matched_quote(prompt, rule),
        rule_id: rule.id.clone(),
        house_style: rule.id.starts_with("style-"),
    }
}

/// A quote (the fallback's matched line, or the judge's own citation) is
/// capped at this many characters (SPEC-ENFORCEMENT.md 2.1: "the matched
/// line, trimmed to 200 chars").
const QUOTE_MAX_CHARS: usize = 200;

/// "The matched line" (SPEC-ENFORCEMENT.md 2.1), for the FALLBACK path only:
/// the first line of the original prompt (original casing, for a real quote)
/// that alone satisfies the winning rule, trimmed to `QUOTE_MAX_CHARS`
/// chars. Falls back to the whole prompt (also trimmed) when no single line
/// does - an `all_of` rule whose required terms are spread across more than
/// one line - so a real match is never quoted as empty.
fn matched_quote(prompt: &str, rule: &CaptureRule) -> String {
    for line in prompt.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if rule_fires(rule, &respond::haystack(line)) {
            return truncate_chars(line, QUOTE_MAX_CHARS);
        }
    }
    truncate_chars(prompt.trim(), QUOTE_MAX_CHARS)
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The marker state, keyed by `session_id`, stored in `capture-owed.json`
/// beside the store. `prompt` is what C1 records - the raw prompt text, fed
/// to the judge at THIS turn's Stop. `tier`/`quote`/`rule_id`/`house_style`
/// are the AUTHORITATIVE, Stop-decided verdict: all `None`/empty until Stop
/// has actually classified this prompt (`decide_stop` fills them in on a
/// `Block`; a `Warn` or nothing at all clears the marker instead) - so a
/// marker freshly written by C1 always has `tier: None`.
///
/// `provisional_tier`/`provisional_quote`/`provisional_rule_id` are a SEPARATE,
/// cheaper signal: C1's own immediate rulebook match (`decide_capture`),
/// available from the moment the prompt is recorded, well before Stop has
/// judged anything. Restored 2026-08-05 (see `capture`'s own doc comment and
/// `JUDGE-TRANSPORT.md`'s "C3 restored" section) specifically so C3 has
/// SOMETHING to act on within the same turn - but it may only ever WARN
/// (`sink_verdict`), never block: it is the same 11.4%-false-block-rate
/// rulebook, just consulted earlier.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Marker {
    #[serde(default)]
    pub prompt: String,
    pub seq_at_flag: i64,
    #[serde(default)]
    pub blocked_once: bool,
    #[serde(default)]
    pub tier: Option<Tier>,
    #[serde(default)]
    pub quote: String,
    #[serde(default)]
    pub rule_id: String,
    #[serde(default)]
    pub house_style: bool,
    #[serde(default)]
    pub provisional_tier: Option<Tier>,
    #[serde(default)]
    pub provisional_quote: String,
    #[serde(default)]
    pub provisional_rule_id: String,
}

/// The fallback rulebook and the marker file both sit beside the store,
/// exactly like the Response Guard's own rulebook
/// (`respond::default_rulebook_path`) - a project directory could otherwise
/// plant one and inject verdicts.
pub fn default_rulebook_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("guard-capture-rulebook.json")
}

pub fn default_marker_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("capture-owed.json")
}

/// Parse the marker file. Malformed JSON (or a file that never existed)
/// yields an empty map - fail-open, same stance as everything else here: a
/// broken marker file means no session has anything owed, not an error.
pub fn read_markers(text: &str) -> HashMap<String, Marker> {
    serde_json::from_str(text).unwrap_or_default()
}

/// C1's write side (SPEC-ENFORCEMENT.md 2.1, corrected 2026-08-05 per
/// `JUDGE-TRANSPORT.md`: cheap and dumb about BLOCKING - the judge, not the
/// rulebook, decides whether turn end may be blocked, and there is no
/// rulebook prefilter gating whether a prompt gets recorded at all). Folds a
/// fresh marker for `session_id` into whatever the marker file already held
/// (parsed via `read_markers`, so a broken existing file is silently
/// replaced rather than refused) and returns the new file contents to write.
/// Runs unconditionally on every non-empty prompt. `rulebook_text` (the SAME
/// fallback rulebook `fallback_decision` reads at Stop) is matched here too,
/// via the ordinary, unforced `decide_capture` - its result is stored ONLY as
/// `provisional_tier`/`provisional_quote`/`provisional_rule_id`, restored
/// 2026-08-05 so C3 has a same-turn signal to WARN on (never block on - see
/// `sink_verdict`). The AUTHORITATIVE `tier` stays `None` here regardless -
/// that field is Stop's alone to fill in, via the judge. A later call for
/// the same session always REPLACES the earlier one.
pub fn flag_marker_text(
    existing_text: Option<&str>,
    session_id: &str,
    prompt: &str,
    seq_at_flag: i64,
    rulebook_text: Option<&str>,
) -> Option<String> {
    let mut markers = existing_text.map(read_markers).unwrap_or_default();
    let provisional = decide_capture(rulebook_text, prompt);
    markers.insert(
        session_id.to_string(),
        Marker {
            prompt: prompt.to_string(),
            seq_at_flag,
            blocked_once: false,
            tier: None,
            quote: String::new(),
            rule_id: String::new(),
            house_style: false,
            provisional_tier: provisional.as_ref().map(|d| d.tier),
            provisional_quote: provisional.as_ref().map(|d| d.quote.clone()).unwrap_or_default(),
            provisional_rule_id: provisional.as_ref().map(|d| d.rule_id.clone()).unwrap_or_default(),
        },
    );
    serde_json::to_string(&markers).ok()
}

/// Whether a `fact_created` (item-declaring) or `fact_revised`
/// (item-revising) event is present among `kinds` - the whole satisfaction
/// mechanism (SPEC-ENFORCEMENT.md 2.1/2.2): the caller passes the kinds of
/// every event with `seq > seq_at_flag` (`EventStore::events_since`), and if
/// either kind is anywhere in there, the debt is paid, no matter what else
/// happened in between - including what the judge, asked anyway or not,
/// might say.
pub fn debt_paid(kinds: &[EventKind]) -> bool {
    kinds.iter().any(|k| matches!(k, EventKind::FactCreated | EventKind::FactRevised))
}

/// C2's verdict (SPEC-ENFORCEMENT.md 2.2).
#[derive(Debug, Clone, PartialEq)]
pub enum StopVerdict {
    Allow,
    Block(String),
}

/// C2's decision, pure: given this session's marker, whether the debt has
/// been paid (computed by the caller from the live store - see `debt_paid` -
/// never assumed here), and the classification the caller already obtained
/// for `marker.prompt` (the judge's verdict turned into a `FlagDecision` via
/// `from_judge_verdict`, or the fallback's own `fallback_decision` when the
/// judge had nothing to say), decide the verdict and the marker's next
/// state.
///
/// `debt_paid` wins unconditionally, even over a judge `Block` - THE DEFECT
/// THIS PREVENTS: an already-recorded decision must never block turn end
/// just because a judge call (paid for anyway, or run before the caller
/// bothered to check) said BLOCK. The store-sequence check governs; the
/// judge does not override it.
///
/// `classification: None` means nothing fired (an ALLOW verdict, or no
/// judge and nothing in the fallback rulebook either) - always `Allow`,
/// clearing the marker. A `Warn`-tier classification also always clears the
/// marker without blocking - the fallback can never reach `Block` by
/// construction (`fallback_decision`), and a `Warn` from the judge is a
/// genuine "possibly, but not confident enough" that this guard has never
/// blocked on either.
///
/// `None` for the next marker state means "clear it" (the caller removes
/// this session's key entirely); `Some` means "keep it", now carrying the
/// decided tier/quote/rule_id and `blocked_once` set. The caller is
/// responsible for persisting whichever state comes back - this function
/// does no I/O.
pub fn decide_stop(
    marker: &Marker,
    debt_paid: bool,
    classification: Option<&FlagDecision>,
) -> (StopVerdict, Option<Marker>) {
    if debt_paid {
        return (StopVerdict::Allow, None);
    }
    let Some(decision) = classification else {
        return (StopVerdict::Allow, None);
    };
    if decision.tier != Tier::Block {
        // Warn tier never blocks turn end (SPEC 1.1) - and the fallback can
        // never produce Block in the first place, so this is the ONLY way a
        // non-judge classification ever reaches here.
        return (StopVerdict::Allow, None);
    }
    if marker.blocked_once {
        // Already spent this flag's one block (SPEC 2.2 point 5, "one block
        // per flag, ever" - kept as a defensive check even though, under
        // this design, `decide_stop` runs at most once per turn; see this
        // module's own doc comment on why cross-turn survival changed).
        return (StopVerdict::Allow, None);
    }
    let next = Marker {
        tier: Some(Tier::Block),
        quote: decision.quote.clone(),
        rule_id: decision.rule_id.clone(),
        house_style: decision.house_style,
        blocked_once: true,
        ..marker.clone()
    };
    let reason = block_reason(&next);
    (StopVerdict::Block(reason), Some(next))
}

/// C2's block reason (SPEC-ENFORCEMENT.md 2.2 point 4, plus 2.4's house-style
/// addition, plus a rule/source-id addition - SPEC-ENFORCEMENT.md 1.1's
/// invariant that every block must be able to print WHICH rule fired). The
/// literal sentence from the spec is kept byte-for-byte; only appended to.
/// For a judge-decided block, `marker.quote` IS the judge's own citation and
/// `marker.rule_id` is `JUDGE_RULE_ID` - so this reason always carries the
/// judge's citation when the judge is the one who decided it.
pub fn block_reason(marker: &Marker) -> String {
    let mut reason = format!(
        "[THOR] You were told a durable decision (\"{}\") and did not record it in THOR. Record it now with remember or revise.",
        marker.quote
    );
    if marker.house_style {
        reason.push_str(
            " Store the pointer (which source file is authoritative) plus the rule, never a copied value.",
        );
    }
    if !marker.rule_id.is_empty() {
        reason.push_str(&format!(" (matched capture rule: {})", marker.rule_id));
    }
    reason
}

/// The self-adjudicate block message (`CaptureMode::SelfAdjudicate`) - THE
/// INSTRUMENT this mode runs on, so its text lives in exactly ONE place, as
/// this constant, never duplicated or paraphrased at a call site. `{QUOTE}`
/// is substituted with the matched phrase by `self_adjudicate_block_reason`,
/// the only function that renders it.
///
/// Every clause is a deliberate, individually-justified choice, each one
/// tied to a measured failure it exists to prevent (OPTION4-SELF-JUDGING.md:
/// 95% correct self-judgment, 3.8% pollution on the narrow trigger;
/// WIDE-TRIGGER-POLLUTION.md: pollution rises to 16.1% at the wide trigger's
/// higher fire rate, more than 4x - which is WHY this guard keeps the narrow
/// rulebook as its default, see `NARROW_TRIGGER_DEFAULT`). A future editor
/// must not delete a line below without re-reading which number it carries:
///
/// 1. "The owner may have just stated a NEW durable decision this turn" -
///    states the possibility plainly, in the THIRD person (the OWNER, never
///    "you"): a claim about what was SAID, not an accusation about what the
///    assistant did or failed to do. "NEW" and "this turn" scope the
///    question to the current turn's own words, never the whole session.
/// 2. The exact matched phrase is quoted verbatim, so the assistant judges
///    the SAME text the trigger fired on, never a paraphrase.
/// 3. "That only counts if it is genuinely new - not the owner quoting,
///    citing, or restating a rule that already exists" - directly targets
///    the QUOTED-RULE failure class, the single sharpest, most consistent
///    pollution source measured at BOTH trigger widths (50% on n=2 in
///    OPTION4-SELF-JUDGING.md's arm-4b, 28.6% on n=7 in
///    WIDE-TRIGGER-POLLUTION.md): the assistant mistaking the owner citing an
///    EXTERNAL rule ("the style guide says...", "the linked ticket quotes the
///    team lead...") for the owner stating a NEW one. Without this line,
///    both measured pollution cases from that class (`B014`, `B018`,
///    WIDE-TRIGGER-POLLUTION.md) would very plausibly recur.
/// 4. "If it is new, record it now with remember or revise before
///    finishing" - the one instruction this whole guard exists to produce,
///    phrased as an ordinary next step, not a demand.
/// 5. "If not, one line saying so is enough, then finish as normal" - given
///    EQUAL weight to the recording instruction, never a grudging
///    afterthought: the trigger is deliberately wide (most firings are
///    false), so a one-line dismissal must read as the CORRECT, expected
///    outcome most of the time, not a burden to justify.
/// 6. "this is one narrow question about that phrase, not a cue to run any
///    other capture rule you know" - targets a SECOND, distinct failure
///    mechanism WIDE-TRIGGER-POLLUTION.md surfaced (its `B057`/`B058`, both
///    non-normative-trigger): the assistant correctly judged "not a durable
///    decision" and then recorded a fact ANYWAY, reasoning from its OWN
///    unrelated standing capture instructions rather than the narrow
///    question this note actually asked. This line exists specifically to
///    stop this hook from being read as a general invitation to run any
///    other capture habit - it is one question about one phrase, nothing
///    else is in scope here.
/// 7. Nothing here says "you forgot", "you were told and did not record", or
///    any other phrasing that implies a mistake already happened. The
///    measured risk of an accusatory tone is DEFENSIVE OVER-RECORDING, which
///    pollutes the store with facts the owner never actually stated - a
///    message that reads as blame would be worse than one that said nothing
///    at all.
pub const SELF_ADJUDICATE_MESSAGE: &str = "[THOR] The owner may have just stated a NEW durable decision this turn: \"{QUOTE}\". That only counts if it is genuinely new - not the owner quoting, citing, or restating a rule that already exists. If it is new, record it now with remember or revise before finishing. If not, one line saying so is enough, then finish as normal - this is one narrow question about that phrase, not a cue to run any other capture rule you know.";

/// Render `SELF_ADJUDICATE_MESSAGE` with the matched phrase substituted in -
/// the only place `{QUOTE}` is ever replaced. See the constant's own doc
/// comment for why every other word in this message is fixed.
pub fn self_adjudicate_block_reason(quote: &str) -> String {
    SELF_ADJUDICATE_MESSAGE.replacen("{QUOTE}", quote, 1)
}

/// C2's decision under `CaptureMode::SelfAdjudicate` (OPTION4-IMPLEMENTATION.md):
/// no judge, no fallback ladder - the ONLY signal is C1's own cheap rulebook
/// match (`marker.provisional_tier`/`provisional_quote`), counted at ANY tier
/// (Block or Warn alike: OPTION4-SELF-JUDGING.md's own "any-tier recall"
/// framing - the rulebook's job here is only to trigger a look, never to
/// grade confidence, since the assistant itself does the actual judging once
/// asked). `debt_paid` wins unconditionally, exactly like `decide_stop` - an
/// already-recorded decision is never re-litigated. `blocked_once` is the
/// same "one block per flag, ever" mechanism `decide_stop` already uses - a
/// self-adjudicate block that already fired must never fire again for the
/// same flag. Never touches `tier`/`quote`/`rule_id` on the returned marker:
/// those AUTHORITATIVE, judge-only fields stay exactly as they were (`None`
/// in practice, since no judge ever runs in this mode) - `sink_verdict` (C3)
/// is therefore unaffected by self mode, and continues to only ever WARN from
/// `provisional_tier`, never block, exactly as it does today.
pub fn decide_self_adjudicate(marker: &Marker, debt_paid: bool) -> (StopVerdict, Option<Marker>) {
    if debt_paid {
        return (StopVerdict::Allow, None);
    }
    if marker.provisional_tier.is_none() {
        return (StopVerdict::Allow, None);
    }
    if marker.blocked_once {
        return (StopVerdict::Allow, None);
    }
    let reason = self_adjudicate_block_reason(&marker.provisional_quote);
    let next = Marker { blocked_once: true, ..marker.clone() };
    (StopVerdict::Block(reason), Some(next))
}

/// C3's block reason (SPEC-ENFORCEMENT.md 2.3, plus the trigger-quote and
/// rule-id additions).
pub fn sink_block_reason(marker: &Marker) -> String {
    let mut reason = format!(
        "[THOR] this is a durable decision (\"{}\"); record it in THOR first - the .md is a mirror, never the source.",
        marker.quote
    );
    if !marker.rule_id.is_empty() {
        reason.push_str(&format!(" (matched capture rule: {})", marker.rule_id));
    }
    reason
}

/// Is `file_path` a known mirror sink (SPEC-ENFORCEMENT.md 2.3): the
/// basename is `CLAUDE.md`, `BRAND.md`, or `AGENTS.md` (case-insensitive -
/// Windows paths carry no case distinction in practice, and a stricter exact-
/// case match would just as easily miss the very file this guard exists to
/// catch), or any path component is exactly `decisions`.
pub fn is_mirror_sink(file_path: &str) -> bool {
    let path = Path::new(file_path);
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if ["CLAUDE.md", "BRAND.md", "AGENTS.md"].iter().any(|known| name.eq_ignore_ascii_case(known)) {
            return true;
        }
    }
    path.components().any(|c| c.as_os_str().to_str().is_some_and(|s| s.eq_ignore_ascii_case("decisions")))
}

/// C3's verdict (SPEC-ENFORCEMENT.md 2.3, restored 2026-08-05 to fire within
/// the same turn - see `capture`'s own doc comment and `JUDGE-TRANSPORT.md`'s
/// "C3 restored" section).
#[derive(Debug, Clone, PartialEq)]
pub enum SinkVerdict {
    Allow,
    Warn(String),
    Block(String),
}

/// C3's warning text for a merely PROVISIONAL (C1's own rulebook match, not
/// yet judge-confirmed) owed capture - never a real block, see
/// `sink_verdict`.
fn provisional_sink_warning(marker: &Marker) -> String {
    let mut reason = format!(
        "[THOR] possible durable decision (\"{}\") flagged by the fast rulebook, not yet confirmed by the judge; consider recording it in THOR before this edit.",
        marker.provisional_quote
    );
    if !marker.provisional_rule_id.is_empty() {
        reason.push_str(&format!(" (matched capture rule: {})", marker.provisional_rule_id));
    }
    reason
}

/// C3's decision (SPEC-ENFORCEMENT.md 2.3), pure: outside the intersection of
/// "the debt is unpaid" (`debt_paid` is false, computed by the caller exactly
/// like C2's) and "`file_path` names a known mirror sink", always `Allow` -
/// deliberately narrow, so an ordinary edit to CLAUDE.md is never touched
/// just because it IS CLAUDE.md. Inside that intersection, TWO independent
/// signals decide the rest:
///
/// - `marker.tier == Some(Tier::Block)` AND `marker.rule_id == JUDGE_RULE_ID`
///   (an ACTUAL judge verdict, decided at this session's most recent Stop) ->
///   `Block`. By construction this is the only way `tier` can ever be
///   `Some(Block)` at all (the Stop-time fallback can only ever produce
///   `Warn` - `fallback_decision`), so checking `rule_id` here is a second
///   line of defense, not the only one.
/// - Otherwise, `marker.provisional_tier == Some(Tier::Block)` (C1's own
///   cheap, immediate rulebook match - possibly all that exists yet, if
///   Stop has not run this turn) -> `Warn`, never `Block`: the rulebook's
///   measured 11.4% false-block rate (`FRESH-HOLDOUT-RESULTS.md`) must never
///   reach a real block through this path.
/// - Neither signal present -> `Allow`.
pub fn sink_verdict(marker: Option<&Marker>, debt_paid: bool, file_path: Option<&str>) -> SinkVerdict {
    let Some(marker) = marker else { return SinkVerdict::Allow };
    if debt_paid {
        return SinkVerdict::Allow;
    }
    let Some(file_path) = file_path else { return SinkVerdict::Allow };
    if !is_mirror_sink(file_path) {
        return SinkVerdict::Allow;
    }
    if marker.tier == Some(Tier::Block) && marker.rule_id == JUDGE_RULE_ID {
        return SinkVerdict::Block(sink_block_reason(marker));
    }
    if marker.provisional_tier == Some(Tier::Block) {
        return SinkVerdict::Warn(provisional_sink_warning(marker));
    }
    SinkVerdict::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic rulebook, generic content only (no real session text - see
    /// the workspace-level constraint on private data in test fixtures).
    /// Mirrors the terms SPEC-ENFORCEMENT.md 2.1 names as examples.
    const RULEBOOK: &str = r#"[
      {"id":"decision-generic","tier":"block",
       "any_of":["from now on","vanaf nu","never again","nooit meer","voortaan","always ","altijd ","the new rule"],
       "none_of":["any_of","for example","bijvoorbeeld","als voorbeeld","?"]},
      {"id":"style-color","tier":"block",
       "any_of":["the new primary color","de nieuwe huisstijl"],
       "none_of":["any_of","for example","bijvoorbeeld","als voorbeeld","?"]},
      {"id":"decision-soft","tier":"warn",
       "any_of":["i would prefer","ik wil liever","beter is"],
       "none_of":["any_of","?"]}
    ]"#;

    fn fresh_marker(prompt: &str, seq_at_flag: i64) -> Marker {
        Marker {
            prompt: prompt.to_string(),
            seq_at_flag,
            blocked_once: false,
            tier: None,
            quote: String::new(),
            rule_id: String::new(),
            house_style: false,
            provisional_tier: None,
            provisional_quote: String::new(),
            provisional_rule_id: String::new(),
        }
    }

    /// A marker with a provisional (C1 rulebook, not yet judge-confirmed)
    /// signal - what C1 would actually write when `RULEBOOK` matches the
    /// prompt, before this turn's Stop has run at all.
    fn provisional_marker(prompt: &str, seq_at_flag: i64) -> Marker {
        let mut marker = fresh_marker(prompt, seq_at_flag);
        let provisional = decide_capture(Some(RULEBOOK), prompt).expect("test prompt must match RULEBOOK");
        marker.provisional_tier = Some(provisional.tier);
        marker.provisional_quote = provisional.quote;
        marker.provisional_rule_id = provisional.rule_id;
        marker
    }

    fn judged_marker(decision: &FlagDecision, seq_at_flag: i64) -> Marker {
        // What decide_stop's own return would look like right before a
        // Block - used by tests that want to hand-craft "already blocked
        // once" or C3 states without going through decide_stop first.
        Marker {
            prompt: String::new(),
            seq_at_flag,
            blocked_once: false,
            tier: Some(decision.tier),
            quote: decision.quote.clone(),
            rule_id: decision.rule_id.clone(),
            house_style: decision.house_style,
            provisional_tier: None,
            provisional_quote: String::new(),
            provisional_rule_id: String::new(),
        }
    }

    // ---------------------------------------------------------- C1 (flag)

    /// THE DEFECT THIS PREVENTS: session keying is wrong and one session's
    /// flagged prompt leaks into (or is read by) an unrelated session.
    #[test]
    fn a_marker_from_another_session_is_ignored() {
        let text =
            flag_marker_text(None, "session-a", "from now on always run the linter first", 5, Some(RULEBOOK))
                .unwrap();
        let markers = read_markers(&text);
        assert!(markers.get("session-b").is_none(), "session-b must see no marker at all");
        let a = markers.get("session-a").expect("session-a must see its own marker");
        assert!(a.tier.is_none(), "a freshly flagged marker must carry no AUTHORITATIVE verdict yet");
        assert_eq!(a.prompt, "from now on always run the linter first");
    }

    /// C1 never decides whether turn end may block - it records ANY
    /// non-empty prompt, including one that would never match any rulebook
    /// rule at all. This is the whole point: the judge, not a keyword match,
    /// decides the AUTHORITATIVE verdict at Stop.
    #[test]
    fn c1_records_every_prompt_unconditionally_with_no_authoritative_verdict() {
        let text =
            flag_marker_text(None, "s1", "please fix the failing test in parser.rs", 3, Some(RULEBOOK)).unwrap();
        let marker = read_markers(&text).remove("s1").unwrap();
        assert!(marker.tier.is_none());
        assert_eq!(marker.seq_at_flag, 3);
        assert!(!marker.blocked_once);
        assert!(marker.provisional_tier.is_none(), "no rulebook rule fires on this ordinary work request either");
    }

    /// C1 DOES store a cheap, immediate PROVISIONAL signal from the rulebook
    /// - restored 2026-08-05 so C3 has something to warn on within the same
    /// turn - but that signal never reaches the AUTHORITATIVE `tier` field,
    /// which stays `None` until Stop actually judges this prompt.
    #[test]
    fn c1_stores_a_provisional_rulebook_tier_but_never_an_authoritative_one() {
        let text = flag_marker_text(
            None,
            "s1",
            "from now on always run the linter first",
            5,
            Some(RULEBOOK),
        )
        .unwrap();
        let marker = read_markers(&text).remove("s1").unwrap();
        assert!(marker.tier.is_none(), "the AUTHORITATIVE tier is Stop's alone to set");
        assert_eq!(marker.provisional_tier, Some(Tier::Block), "the cheap rulebook match must be recorded");
        assert_eq!(marker.provisional_rule_id, "decision-generic");
        assert!(marker.provisional_quote.contains("from now on always run the linter first"));
    }

    /// A missing/broken rulebook at C1 time yields no provisional signal
    /// either - fail-open, same stance as everywhere else here.
    #[test]
    fn a_missing_rulebook_at_c1_yields_no_provisional_signal() {
        let text =
            flag_marker_text(None, "s1", "from now on always run the linter first", 5, None).unwrap();
        let marker = read_markers(&text).remove("s1").unwrap();
        assert!(marker.provisional_tier.is_none());
    }

    // ------------------------------------------------- the fallback matcher

    /// THE DEFECT THIS PREVENTS (spec's own core instruction): the fallback
    /// rulebook, on its own, ever ends a turn. Measured false-block rate
    /// (11.4%, FRESH-HOLDOUT-RESULTS.md) rules this out - the fallback path
    /// may WARN, but its `FlagDecision` must never carry `Tier::Block`, even
    /// though the underlying rulebook JSON tags this exact rule "block".
    #[test]
    fn the_fallback_path_can_warn_but_never_block() {
        let decision = fallback_decision(Some(RULEBOOK), "from now on always run the linter first")
            .expect("a real rulebook match must still be reported");
        assert_eq!(decision.tier, Tier::Warn, "the fallback must never reach Block, whatever the rulebook says");

        let style = fallback_decision(Some(RULEBOOK), "the new primary color is now used everywhere").unwrap();
        assert_eq!(style.tier, Tier::Warn);

        // A genuine warn-tier rulebook rule stays Warn, as it always did.
        let soft = fallback_decision(Some(RULEBOOK), "i would prefer we ran the linter first").unwrap();
        assert_eq!(soft.tier, Tier::Warn);
    }

    /// Fail-open: no rulebook, unreadable rulebook, or an empty rulebook all
    /// yield no fallback decision - never a guess, never a crash.
    #[test]
    fn a_missing_or_broken_capture_rulebook_yields_no_fallback() {
        let prompt = "from now on always run the linter first";
        assert!(fallback_decision(None, prompt).is_none());
        assert!(fallback_decision(Some("{ not json"), prompt).is_none());
        assert!(fallback_decision(Some("[]"), prompt).is_none());
    }

    /// A question about a rulebook trigger phrase is still not a match -
    /// unchanged matcher semantics, now feeding the fallback instead of a
    /// direct block.
    #[test]
    fn a_question_about_a_rule_still_does_not_match() {
        assert!(decide_capture(Some(RULEBOOK), "should we agree that from now on we always run the linter first?")
            .is_none());
    }

    // --------------------------------------------------- the judge verdict

    /// THE DEFECT THIS PREVENTS: a BLOCK verdict from the judge is silently
    /// dropped, or its citation never reaches the block reason a person (or
    /// the model) actually sees.
    #[test]
    fn a_judge_block_verdict_carries_its_citation_into_the_block_reason() {
        let decision = from_judge_verdict(JudgeVerdict::Block(
            "from now on always run the linter first".to_string(),
        ))
        .expect("a Block verdict must produce a decision");
        assert_eq!(decision.tier, Tier::Block);
        assert_eq!(decision.rule_id, JUDGE_RULE_ID);
        assert!(!decision.house_style, "the judge has no notion of house-style");

        let marker = fresh_marker("from now on always run the linter first", 5);
        let (verdict, next) = decide_stop(&marker, false, Some(&decision));
        match verdict {
            StopVerdict::Block(reason) => {
                assert!(reason.contains("from now on always run the linter first"), "{reason}");
                assert!(reason.contains(JUDGE_RULE_ID), "{reason}");
            }
            StopVerdict::Allow => panic!("a judge Block on an unpaid debt must block turn end"),
        }
        assert_eq!(next.and_then(|m| m.tier), Some(Tier::Block));
    }

    /// A judge WARN verdict never blocks - same as a fallback warn always
    /// did.
    #[test]
    fn a_judge_warn_verdict_never_blocks() {
        let decision = from_judge_verdict(JudgeVerdict::Warn("maybe a rule, unclear".to_string())).unwrap();
        assert_eq!(decision.tier, Tier::Warn);
        let marker = fresh_marker("maybe a rule, unclear", 5);
        let (verdict, next) = decide_stop(&marker, false, Some(&decision));
        assert_eq!(verdict, StopVerdict::Allow);
        assert!(next.is_none());
    }

    /// An ALLOW verdict carries nothing to capture at all.
    #[test]
    fn a_judge_allow_verdict_yields_no_decision() {
        assert!(from_judge_verdict(JudgeVerdict::Allow).is_none());
    }

    // -------------------------------------------------------------- C2/Stop

    /// THE DEFECT THIS PREVENTS: the owner states a durable decision, the
    /// model never calls remember/revise, and the turn ends with the
    /// decision living nowhere but the transcript - now decided by the
    /// judge rather than a keyword match.
    #[test]
    fn a_stated_decision_that_was_never_recorded_blocks_turn_end() {
        let decision = from_judge_verdict(JudgeVerdict::Block("hard rule: no raw sql string concat".to_string()))
            .unwrap();
        let marker = fresh_marker("hard rule: no raw sql string concat, ever", 5);
        let (verdict, next) = decide_stop(&marker, false, Some(&decision));
        assert!(matches!(verdict, StopVerdict::Block(_)));
        assert_eq!(next.map(|m| m.blocked_once), Some(true));
    }

    /// THE DEFECT THIS PREVENTS: the owner DOES record the decision (a
    /// remember/revise landed after the flag), but the guard still blocks
    /// because it never checked whether the debt was paid - and this must
    /// hold EVEN WHEN the judge itself says BLOCK: the store-sequence check
    /// governs, not the judge (SPEC's own explicit test case).
    #[test]
    fn an_already_recorded_decision_does_not_block_even_when_the_judge_says_block() {
        let decision =
            from_judge_verdict(JudgeVerdict::Block("from now on always run the linter first".to_string())).unwrap();
        let marker = fresh_marker("from now on always run the linter first", 5);
        // debt_paid = true: a fact_created/fact_revised landed since seq_at_flag.
        let (verdict, next) = decide_stop(&marker, true, Some(&decision));
        assert_eq!(verdict, StopVerdict::Allow, "a paid debt must win over any judge verdict");
        assert!(next.is_none(), "a paid debt must clear the marker");
    }

    /// The same flag never blocks twice (SPEC 2.2 point 5) - defensive: under
    /// this design `decide_stop` runs at most once per turn, but the
    /// invariant must still hold if it were ever invoked again against a
    /// marker that already spent its one block.
    #[test]
    fn the_same_flag_never_blocks_twice() {
        let decision = from_judge_verdict(JudgeVerdict::Block("from now on always run the linter first".into())).unwrap();
        let marker = fresh_marker("from now on always run the linter first", 5);
        let (first, next) = decide_stop(&marker, false, Some(&decision));
        assert!(matches!(first, StopVerdict::Block(_)));
        let already_blocked = next.expect("a block must keep the marker with blocked_once flipped");
        assert!(already_blocked.blocked_once);
        let (second, next2) = decide_stop(&already_blocked, false, Some(&decision));
        assert_eq!(second, StopVerdict::Allow, "a flag that already blocked once must never block again");
        assert!(next2.is_none());
    }

    /// No classification at all (no judge configured AND the fallback found
    /// nothing either) must never block - this is the "no config, no
    /// rulebook match, on real uncertainty: silence" end of the ladder.
    #[test]
    fn no_classification_at_all_never_blocks() {
        let marker = fresh_marker("please fix the failing test in parser.rs", 5);
        let (verdict, next) = decide_stop(&marker, false, None);
        assert_eq!(verdict, StopVerdict::Allow);
        assert!(next.is_none());
    }

    // ------------------------------------------------- capture mode selection

    /// THE DEFECT THIS PREVENTS: self mode is meant to be the new default the
    /// moment no judge is configured - if this silently stayed on the old
    /// judge-only behavior instead, the whole point of this task (a working
    /// default on a machine where the judge transport is too costly to run)
    /// would be lost.
    #[test]
    fn self_mode_is_the_default_when_no_judge_config_is_present() {
        assert_eq!(resolve_capture_mode(None, false), CaptureMode::SelfAdjudicate);
        assert_eq!(resolve_capture_mode(Some("{ not json"), false), CaptureMode::SelfAdjudicate);
        assert_eq!(resolve_capture_mode(Some("{}"), false), CaptureMode::SelfAdjudicate, "no mode key at all");
    }

    /// THE DEFECT THIS PREVENTS: a real, usable judge transport being
    /// silently ignored in favor of the new self-adjudicate default - every
    /// existing deployment that only ever wrote `guard-judge-config.json`
    /// (and never knew mode selection existed) must keep using the judge,
    /// unchanged, with no mode file of its own.
    #[test]
    fn a_configured_judge_still_wins_over_self_mode() {
        assert_eq!(resolve_capture_mode(None, true), CaptureMode::Judge);
        assert_eq!(resolve_capture_mode(Some("{ not json"), true), CaptureMode::Judge);
    }

    /// `off` can only ever be reached through an explicit mode config -
    /// there is no other signal that could ever mean "disable the guard".
    #[test]
    fn off_is_only_ever_reached_through_an_explicit_mode_config() {
        assert_eq!(resolve_capture_mode(Some(r#"{"mode":"off"}"#), false), CaptureMode::Off);
        assert_eq!(resolve_capture_mode(Some(r#"{"mode":"off"}"#), true), CaptureMode::Off, "off wins even over a real judge");
    }

    /// An explicit mode config can force EITHER of the other two modes as
    /// well, overriding what the judge-configured signal alone would pick -
    /// useful for a side-by-side comparison without deleting the judge
    /// config.
    #[test]
    fn an_explicit_mode_config_overrides_the_inferred_default_either_direction() {
        assert_eq!(resolve_capture_mode(Some(r#"{"mode":"self"}"#), true), CaptureMode::SelfAdjudicate);
        assert_eq!(resolve_capture_mode(Some(r#"{"mode":"judge"}"#), false), CaptureMode::Judge);
    }

    /// Fail-open: an unrecognized mode string is treated exactly like a
    /// missing mode config, never a guess and never a panic.
    #[test]
    fn an_unrecognized_mode_string_falls_through_to_the_default() {
        assert!(parse_mode_config(r#"{"mode":"maybe"}"#).is_none());
        assert_eq!(resolve_capture_mode(Some(r#"{"mode":"maybe"}"#), false), CaptureMode::SelfAdjudicate);
    }

    // ------------------------------------------------------ self-adjudicate

    /// THE DEFECT THIS PREVENTS: a capture is owed (C1's own rulebook match
    /// is on record) and unpaid, but self-adjudicate mode never hands the
    /// judgment back to the assistant at all - the entire mechanism this
    /// mode exists to provide.
    #[test]
    fn self_adjudicate_mode_blocks_once_when_a_capture_is_owed() {
        let marker = provisional_marker("from now on always run the linter first", 5);
        let (verdict, next) = decide_self_adjudicate(&marker, false);
        match verdict {
            StopVerdict::Block(reason) => {
                assert!(reason.contains("from now on always run the linter first"), "{reason}");
                assert!(reason.contains("owner may have just stated a NEW durable decision"), "{reason}");
                assert!(reason.contains("If not"), "dismissal must be offered, not just recording: {reason}");
            }
            StopVerdict::Allow => panic!("an owed, unpaid capture must block once in self-adjudicate mode"),
        }
        let next = next.expect("a block must keep the marker with blocked_once flipped");
        assert!(next.blocked_once);
        assert!(next.tier.is_none(), "self mode must never fill in the AUTHORITATIVE (judge-only) tier");
    }

    /// THE DEFECT THIS PREVENTS: the same flag blocks the turn over and over,
    /// every time Stop happens to run again - the exact "one block per flag,
    /// ever" invariant `decide_stop` already upholds for the judge path, now
    /// required for self-adjudicate mode too.
    #[test]
    fn self_adjudicate_mode_never_blocks_twice_for_the_same_flag() {
        let marker = provisional_marker("from now on always run the linter first", 5);
        let (first, next) = decide_self_adjudicate(&marker, false);
        assert!(matches!(first, StopVerdict::Block(_)));
        let already_blocked = next.expect("a block must keep the marker");
        assert!(already_blocked.blocked_once);
        let (second, next2) = decide_self_adjudicate(&already_blocked, false);
        assert_eq!(second, StopVerdict::Allow, "a flag that already blocked once must never block again");
        assert!(next2.is_none());
    }

    /// THE DEFECT THIS PREVENTS: an already-recorded decision (a real
    /// remember/revise landed since the flag) still gets re-litigated in
    /// self-adjudicate mode, asking the assistant to justify something it
    /// already did.
    #[test]
    fn a_paid_debt_never_blocks_even_in_self_adjudicate_mode() {
        let marker = provisional_marker("from now on always run the linter first", 5);
        let (verdict, next) = decide_self_adjudicate(&marker, true);
        assert_eq!(verdict, StopVerdict::Allow, "a paid debt must win over self-adjudicate mode too");
        assert!(next.is_none(), "a paid debt must clear the marker");
    }

    /// No rulebook signal at all (C1's own match never fired) must never
    /// block in self-adjudicate mode either - on real uncertainty, silence.
    #[test]
    fn no_provisional_signal_never_blocks_in_self_adjudicate_mode() {
        let marker = fresh_marker("please fix the failing test in parser.rs", 5);
        let (verdict, next) = decide_self_adjudicate(&marker, false);
        assert_eq!(verdict, StopVerdict::Allow);
        assert!(next.is_none());
    }

    /// THE INSTRUMENT ITSELF: the self-adjudicate block message must always
    /// carry the exact matched phrase, whatever it is - the assistant is
    /// asked to judge the SAME text the trigger fired on, not a paraphrase.
    #[test]
    fn the_self_adjudicate_message_always_contains_the_quoted_phrase() {
        for quote in [
            "from now on always run the linter first",
            "de nieuwe huisstijl is voortaan blauw",
            "a phrase with \"nested quotes\" in it",
        ] {
            let reason = self_adjudicate_block_reason(quote);
            assert!(reason.contains(quote), "message must quote the phrase verbatim: {reason}");
        }
    }

    /// THE DEFECT THIS PREVENTS: the QUOTED-RULE pollution class
    /// (WIDE-TRIGGER-POLLUTION.md: 28.6% on n=7, and 50% on n=2 in
    /// OPTION4-SELF-JUDGING.md - the single sharpest, most consistent
    /// pollution source measured at both trigger widths) - the owner citing
    /// an already-existing rule mistaken for the owner stating a NEW one.
    /// The message must explicitly exclude this case.
    #[test]
    fn the_self_adjudicate_message_excludes_quoted_and_already_known_rules() {
        assert!(SELF_ADJUDICATE_MESSAGE.contains("genuinely new"));
        assert!(SELF_ADJUDICATE_MESSAGE.to_ascii_lowercase().contains("quoting"));
        assert!(SELF_ADJUDICATE_MESSAGE.to_ascii_lowercase().contains("already exists"));
    }

    /// THE DEFECT THIS PREVENTS: the second, distinct pollution mechanism
    /// WIDE-TRIGGER-POLLUTION.md surfaced (`B057`/`B058`, non-normative-trigger,
    /// 20% pollution in that class) - the assistant correctly judges "not a
    /// durable decision" and then records something ANYWAY, reasoning from
    /// its own unrelated standing capture rules rather than the narrow
    /// question this note actually asks. The message must scope itself
    /// explicitly to one phrase, one question - never a general invitation to
    /// run any other capture habit.
    #[test]
    fn the_self_adjudicate_message_scopes_the_question_narrowly() {
        assert!(SELF_ADJUDICATE_MESSAGE.contains("one narrow question"));
        assert!(SELF_ADJUDICATE_MESSAGE.to_ascii_lowercase().contains("not a cue"));
    }

    // -------------------------------------------------------------------- C3

    /// THE DEFECT THIS PREVENTS (C3 narrowness): ordinary maintenance edits
    /// to CLAUDE.md get treated as owed even when nothing is, turning
    /// routine work into a nag.
    #[test]
    fn an_edit_to_claude_md_without_an_owed_capture_is_not_touched() {
        assert_eq!(
            sink_verdict(None, false, Some("C:/project/CLAUDE.md")),
            SinkVerdict::Allow,
            "no owed capture must mean no block and no warn, no matter the file"
        );
    }

    /// A freshly flagged marker with NEITHER an authoritative NOR a
    /// provisional signal (C1 just ran, the prompt matched no rulebook rule
    /// at all, Stop has not judged it yet) must never be treated as owed by
    /// C3 - on real uncertainty, this guard stays silent rather than
    /// guessing.
    #[test]
    fn a_marker_with_no_signal_at_all_never_touches_a_sink_write() {
        let marker = fresh_marker("please fix the failing test in parser.rs", 5);
        assert_eq!(sink_verdict(Some(&marker), false, Some("C:/project/CLAUDE.md")), SinkVerdict::Allow);
    }

    /// THE DEFECT THIS RESTORES (2026-08-05): before Stop has judged
    /// anything this turn, C1's own cheap rulebook match is ALREADY in the
    /// marker as `provisional_tier` - this is what lets C3 act WITHIN the
    /// same turn again, rather than only after that turn's Stop. It may
    /// only ever WARN, never block: same rulebook, same measured 11.4%
    /// false-block rate.
    #[test]
    fn c3_only_warns_when_the_tier_came_from_the_rulebook() {
        let marker = provisional_marker("from now on always run the linter first", 5);
        assert!(marker.tier.is_none(), "Stop has not run yet in this scenario");
        match sink_verdict(Some(&marker), false, Some("C:/project/CLAUDE.md")) {
            SinkVerdict::Warn(reason) => {
                assert!(reason.contains("from now on always run the linter first"), "{reason}");
                assert!(reason.contains("not yet confirmed by the judge"), "{reason}");
            }
            other => panic!("a provisional (rulebook-only) Block must WARN, never block or stay silent: {other:?}"),
        }
    }

    /// The other half of C3's restoration: an owed, unpaid capture whose
    /// AUTHORITATIVE tier came from an ACTUAL judge verdict DOES block a
    /// mirror-sink edit, but never an edit to an unrelated file.
    #[test]
    fn an_owed_judge_decided_capture_blocks_only_the_mirror_sink_not_any_file() {
        let decision = from_judge_verdict(JudgeVerdict::Block("from now on always run the linter first".into())).unwrap();
        let marker = judged_marker(&decision, 5);
        assert!(matches!(
            sink_verdict(Some(&marker), false, Some("C:/project/CLAUDE.md")),
            SinkVerdict::Block(_)
        ));
        assert!(matches!(
            sink_verdict(Some(&marker), false, Some("C:/project/decisions/2026-08-05.md")),
            SinkVerdict::Block(_)
        ));
        assert_eq!(
            sink_verdict(Some(&marker), false, Some("C:/project/src/lib.rs")),
            SinkVerdict::Allow,
            "an ordinary source file must never be caught by this guard"
        );
    }

    /// A `Warn` tier marker (from the judge, or the Stop-time fallback) never
    /// reaches a C3 block either - only an ACTUAL judge `Block` is owed
    /// strongly enough to gate a sink write, and this must hold even when a
    /// provisional signal is absent.
    #[test]
    fn a_warn_tier_marker_never_blocks_a_sink_write() {
        let decision = from_judge_verdict(JudgeVerdict::Warn("maybe a rule".into())).unwrap();
        let marker = judged_marker(&decision, 5);
        assert_eq!(sink_verdict(Some(&marker), false, Some("C:/project/CLAUDE.md")), SinkVerdict::Allow);
    }

    /// A paid debt silences C3 even when a provisional Block is still on
    /// record - the moment the decision is actually recorded, nothing is
    /// owed any more, provisional or not.
    #[test]
    fn a_paid_debt_silences_even_a_provisional_sink_warning() {
        let marker = provisional_marker("from now on always run the linter first", 5);
        assert_eq!(sink_verdict(Some(&marker), true, Some("C:/project/CLAUDE.md")), SinkVerdict::Allow);
    }

    // ---------------------------------------------------------------- C4

    /// C4 (SPEC-ENFORCEMENT.md 2.4): a house-style capture's block reason
    /// carries the extra pointer-not-value sentence when the FALLBACK
    /// decided it (the judge never sets house_style - see
    /// `from_judge_verdict`).
    #[test]
    fn a_house_style_fallback_capture_names_the_pointer_not_the_value() {
        let decision = fallback_decision(Some(RULEBOOK), "the new primary color is now used everywhere").unwrap();
        assert!(decision.house_style, "a style- rule must be flagged house_style");
        let marker = fresh_marker("the new primary color is now used everywhere", 5);
        let (verdict, _) = decide_stop(&marker, false, Some(&decision));
        // The fallback is Warn-only, so this never blocks - but the pointer
        // sentence still needs to work correctly when constructed directly,
        // in case a future surface prints a Warn reason from the same data.
        assert_eq!(verdict, StopVerdict::Allow);
        let judged = judged_marker(&decision, 5);
        assert!(block_reason(&judged).contains("pointer"), "{}", block_reason(&judged));
    }

    /// Safety model test 11, independent of surface: every block reason this
    /// guard can produce names the rule/source that fired AND the text that
    /// triggered it - a block that cannot say why is a bug (SPEC-
    /// ENFORCEMENT.md 1.1).
    #[test]
    fn every_block_reason_names_the_rule_and_the_trigger() {
        let marker = Marker {
            prompt: String::new(),
            tier: Some(Tier::Block),
            quote: "from now on always run the linter first".to_string(),
            seq_at_flag: 0,
            blocked_once: false,
            rule_id: JUDGE_RULE_ID.to_string(),
            house_style: false,
            provisional_tier: None,
            provisional_quote: String::new(),
            provisional_rule_id: String::new(),
        };
        let reason = block_reason(&marker);
        assert!(reason.contains(&marker.quote), "must name the trigger: {reason}");
        assert!(reason.contains(&marker.rule_id), "must name the rule: {reason}");

        let sink_reason = sink_block_reason(&marker);
        assert!(sink_reason.contains(&marker.quote), "must name the trigger: {sink_reason}");
        assert!(sink_reason.contains(&marker.rule_id), "must name the rule: {sink_reason}");
    }

    // ------------------------------------------------------------- support

    /// debt_paid in isolation: only the two recording kinds pay it off; any
    /// other kind of event in between (a delivery, a mark, a retract) does
    /// not.
    #[test]
    fn debt_paid_only_on_a_declare_or_revise_event() {
        assert!(!debt_paid(&[]));
        assert!(!debt_paid(&[EventKind::ItemServed, EventKind::FactRetracted]));
        assert!(debt_paid(&[EventKind::ItemServed, EventKind::FactCreated]));
        assert!(debt_paid(&[EventKind::FactRevised]));
    }

    /// The quote is capped at 200 chars even for a very long matching line
    /// (fallback path) or a very long judge citation (judge path).
    #[test]
    fn the_quote_is_trimmed_to_200_chars() {
        let long_tail = "x".repeat(400);
        let prompt = format!("from now on always {long_tail}");
        let decision = decide_capture(Some(RULEBOOK), &prompt).unwrap();
        assert!(decision.quote.chars().count() <= 200, "quote was {} chars", decision.quote.chars().count());

        let judge_decision = from_judge_verdict(JudgeVerdict::Block(long_tail)).unwrap();
        assert!(judge_decision.quote.chars().count() <= 200);
    }
}
