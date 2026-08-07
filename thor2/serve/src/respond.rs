//! The Response Guard: the one surface that watches the ASSISTANT, not the
//! store. It reads a rulebook of the owner's standing rules about how a reply
//! should look ("open with a TLDR when the answer is technical", "do not ask
//! me to do what you can do yourself", "no reflexive AI disclaimers") and, on
//! the Stop hook, blocks a reply that breaks one so the model reconsiders
//! before yielding.
//!
//! THE DEFECT THIS EXISTS TO UNDO, and it is the sharpest one of the whole
//! rebuild (found by the owner, 2026-08-03). 1.0 ran this guard on a Stop
//! hook. 2.0 was built with three of 1.0's four hook surfaces and this one -
//! the only one that watches the assistant's own behaviour - was left out
//! entirely. So for a full session of building 2.0, replies full of commit
//! hashes and "PASS" went out with no TLDR and nothing corrected them,
//! because the thing that would have was the thing not built. That is exactly
//! the failure class the whole project set out to make impossible - a surface
//! silently doing less than 1.0 - introduced by my own hand, on the surface
//! that guards my hand. It is restored here, byte-for-byte in behaviour: the
//! rulebook is the owner's own, ported unchanged.
//!
//! Matching is dependency-free, case-insensitive substring logic, identical to
//! 1.0's `guard::evaluate` so the same rulebook behaves the same. A rule fires
//! when: the haystack is at least `min_chars` long (a floor so a rule about the
//! SHAPE of a long report does not fire on a two-line status note that happens
//! to contain one technical word), AND every `all_of` term is present, AND
//! (`any_of` is empty OR at least one is present), AND no `none_of` term is
//! present (the escape hatch that lets a false-positive twin through).
//!
//! Failure policy: this is the ONE surface allowed to emit a block, but ONLY on
//! a genuine match. Every ERROR is hard fail-open - a missing or malformed
//! rulebook, unreadable stdin, anything - prints nothing and blocks nothing.
//! A guard that watches replies must never itself become the reason a reply
//! cannot be given.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// One response rule, parsed from the rulebook JSON.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub all_of: Vec<String>,
    pub any_of: Vec<String>,
    /// If ANY of these is present the rule does NOT fire - the false-positive
    /// escape (e.g. "false positive", "als voorbeeld", "any_of" so the rule's
    /// own definition text never trips it).
    pub none_of: Vec<String>,
    /// Length floor in characters. 0 = no floor. The TLDR rule uses 600 so a
    /// short status line carrying one technical word is never blocked.
    pub min_chars: usize,
    pub reminder: String,
}

/// The rulebook next to the store, so the Stop hook needs no path argument.
/// Never a CWD-relative name: a project directory could plant one and inject
/// reminders. `db` is the store path; the rulebook sits beside it.
pub fn default_rulebook_path(db: &Path) -> PathBuf {
    db.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("guard-response-rulebook.json")
}

/// Read a JSON array field as a list of strings, dropping any non-string
/// entries silently (same fail-soft stance as everything else in this
/// parser). Shared by every rule shape this file parses (`Rule` and
/// `OptInRule` further down) so the field-reading logic exists exactly once.
fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Parse one rulebook entry into a `Rule`. `None` (entry dropped, fail-open)
/// when `reminder` is missing or not a string - the one field every rule
/// must carry. Factored out of `parse_rules` so `OptInRule` (further down)
/// can build its own base `Rule` from the exact same logic instead of a
/// second copy that could silently drift from this one.
fn parse_one_rule(r: &Value) -> Option<Rule> {
    let reminder = r.get("reminder").and_then(|v| v.as_str())?.to_string();
    Some(Rule {
        id: r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        all_of: str_list(r, "all_of"),
        any_of: str_list(r, "any_of"),
        none_of: str_list(r, "none_of"),
        min_chars: r
            .get("min_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .try_into()
            .unwrap_or(usize::MAX),
        reminder,
    })
}

/// Parse the rulebook. Any malformed JSON yields no rules (fail-open): a broken
/// rulebook silences the guard, never errors.
pub fn parse_rules(text: &str) -> Vec<Rule> {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    arr.iter().filter_map(parse_one_rule).collect()
}

/// Normalise the assistant's message into one lowercase haystack: collapse
/// intra-line whitespace to a single space so multi-word terms ("git commit")
/// match regardless of extra spaces, but keep newlines so a term cannot span
/// two unrelated lines. Same shape as 1.0's `tool_input_text`.
pub fn haystack(message: &str) -> String {
    message
        .to_lowercase()
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `rule`'s AND/OR/NOT/min_chars conditions hold against this
/// haystack - the whole non-positional matcher, factored out of `evaluate`
/// so `evaluate_opt_in` (further down) can reuse it verbatim rather than
/// reimplementing AND/OR/NOT next to a second copy that could drift from
/// this one.
fn rule_matches(rule: &Rule, haystack_lower: &str) -> bool {
    if rule.min_chars > 0 && haystack_lower.chars().count() < rule.min_chars {
        return false;
    }
    if !rule.all_of.iter().all(|s| haystack_lower.contains(&s.to_lowercase())) {
        return false;
    }
    let any_ok =
        rule.any_of.is_empty() || rule.any_of.iter().any(|s| haystack_lower.contains(&s.to_lowercase()));
    if !any_ok {
        return false;
    }
    if rule.none_of.iter().any(|s| haystack_lower.contains(&s.to_lowercase())) {
        return false;
    }
    true
}

/// The pure matcher: every rule's reminder that fires for this haystack, in
/// rulebook order. Identical semantics to 1.0's `guard::evaluate`.
pub fn evaluate(rules: &[Rule], haystack_lower: &str) -> Vec<String> {
    rules.iter().filter(|r| rule_matches(r, haystack_lower)).map(|r| r.reminder.clone()).collect()
}

/// The block reason for an assistant message, or `None` if nothing fires.
/// `rulebook_text` is the raw rulebook file contents (or None if it could not
/// be read - then nothing fires, fail-open).
pub fn block_reason(rulebook_text: Option<&str>, message: &str) -> Option<String> {
    let rules = rulebook_text.map(parse_rules).unwrap_or_default();
    if rules.is_empty() {
        return None;
    }
    let fired = evaluate(&rules, &haystack(message));
    if fired.is_empty() {
        None
    } else {
        Some(format!("[THOR] {}", fired.join("  ||  ")))
    }
}

// ---------------------------------------------------------------- opt-in
//
// Two capabilities `Rule`/`parse_rules`/`evaluate`/`block_reason` above
// cannot express, added here without changing one byte of them: a
// POSITION-aware escape, and a WARN tier. Not wired into `block_reason`
// itself and not used by any caller yet - a rulebook has to opt in by
// adding a `before` and/or `tier` key before either does anything, and
// today's live rulebook adds neither (proven, not assumed - see
// `serve/tests/response_guard_live_rulebook_unchanged.rs`).
//
// WHY A NEW TYPE (`OptInRule`) INSTEAD OF NEW FIELDS ON `Rule` ITSELF: this
// module is not the only place a `Rule` gets built. `capture.rs`'s
// `to_respond_rule` constructs one with a plain struct literal naming
// exactly today's six fields, to run its OWN, unrelated fallback rulebook
// through this matcher (SPEC-ENFORCEMENT.md section 0: "the capture guard
// REUSES this matcher... it does not get a matcher of its own"). A Rust
// struct literal must name every field of the type; a new REQUIRED field on
// `Rule` stops that call site compiling, and `capture.rs` is out of this
// task's edit scope. A second, separate type that WRAPS a `Rule` sidesteps
// this: `Rule`, `parse_rules`, `evaluate` and `block_reason` are untouched
// below this line, so every existing caller - `capture.rs`,
// `serve/src/bin/serve.rs`, and this file's own pre-existing tests above -
// keeps compiling and behaving exactly as it does today.
//
// POSITION - the shape chosen (`before: Vec<String>`, anchored to the
// rule's own `any_of`), and why over two alternatives considered:
//
// - A fixed "opening window" (does term X occur within the first N
//   characters), independent of where the trigger term sits. Rejected: it
//   cannot tell "TLDR: ... commit ..." (compliant) apart from "commit ...
//   TLDR: ..." (not compliant) when both markers land inside the same
//   window - it checks presence-in-a-region, not ORDER, and the defect this
//   exists to fix (RESPONSE-GUARD-BLIND.md's b01 false block) and this
//   task's own required tests are both specifically about order: the same
//   two words, swapped, must flip the verdict. A window check cannot
//   express that on its own.
// - A fully general "A before B" primitive taking two independently-named
//   term lists, rather than reusing `any_of`. More reusable in the
//   abstract, but it is a second vocabulary to read, validate and default
//   on every rule, for a relation this rulebook shape already has half of:
//   `any_of` already IS "the trigger vocabulary" for any rule that has one.
//   Anchoring `before` to `any_of`'s own earliest hit reuses that instead of
//   naming the same terms twice, and is the smaller shape for the one case
//   this task asks to fix. A fully general two-list version could still be
//   added later without breaking this one - the same way this one was added
//   without breaking `Rule`.
//
// `before` empty (every rulebook shipped so far, since the field does not
// exist in any of them) means no positional constraint at all - the rule
// evaluates exactly as `evaluate` would score it. Byte offsets from
// `str::find` (not char offsets) are compared throughout: correct for
// ordering purposes, since UTF-8 byte order and char order agree for any
// valid string, and `min_chars` above is the only place that ever needed a
// true char COUNT rather than an offset.
//
// WARN TIER - `tier: Tier`, defaulting to `Tier::Block`: SPEC-ENFORCEMENT.md
// section 1.1's three verdicts (BLOCK/WARN/ALLOW), restricted to the two a
// rulebook rule can choose between (ALLOW is simply "did not fire", which
// needs no field of its own). Missing OR unrecognised -> `Block`,
// deliberately stricter defaulting than `capture::parse_rulebook`'s (which
// DROPS a rule that carries no valid tier): every rule shipped so far has no
// `tier` key at all and blocks when it fires, and this parser has to keep
// meaning that, byte for byte - dropping instead of defaulting would
// silently turn every one of today's six live rules into dead weight the
// moment this parser is used at all.
//
// HOW A WARN VERDICT REACHES THE CALLER: `guard_verdict` below returns it as
// `GuardVerdict::warn_reason`, formatted exactly like `block_reason`'s own
// return value. That is as far as this file can take it. Actually
// surfacing that text to the owner without stopping the reply needs a
// change in `serve/src/bin/serve.rs`, outside this task's edit scope - see
// the task report for precisely what and where.

/// BLOCK (today's only behaviour, and the default) or WARN
/// (SPEC-ENFORCEMENT.md 1.1: emit text, never stop the reply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Block,
    Warn,
}

impl Tier {
    fn from_json(s: &str) -> Option<Self> {
        match s {
            "block" => Some(Tier::Block),
            "warn" => Some(Tier::Warn),
            _ => None,
        }
    }
}

/// A `Rule` plus the two capabilities it cannot express on its own - see
/// this section's own doc comment for why this is a separate type from
/// `Rule` rather than new fields on it. Parsed by `parse_opt_in_rules`,
/// matched by `evaluate_opt_in`.
#[derive(Debug, Clone)]
pub struct OptInRule {
    pub base: Rule,
    /// Terms whose earliest occurrence, if strictly before the earliest
    /// occurrence of whichever `any_of` term matched, suppresses firing -
    /// the rule is then treated as compliant. Empty = no positional
    /// constraint (today's behaviour).
    pub before: Vec<String>,
    /// Defaults to `Block` when the rulebook carries no `tier` key, or one
    /// this parser does not recognise.
    pub tier: Tier,
}

/// Parse the rulebook into `OptInRule`s: every field `parse_rules` reads,
/// plus `before` and `tier`. Same fail-open stance as `parse_rules` -
/// malformed JSON, or an entry missing `reminder`, drops that entry rather
/// than erroring.
pub fn parse_opt_in_rules(text: &str) -> Vec<OptInRule> {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    arr.iter()
        .filter_map(|r| {
            let base = parse_one_rule(r)?;
            let before = str_list(r, "before");
            let tier =
                r.get("tier").and_then(|v| v.as_str()).and_then(Tier::from_json).unwrap_or(Tier::Block);
            Some(OptInRule { base, before, tier })
        })
        .collect()
}

/// The byte offset (see this section's own doc comment for why that is the
/// right unit here) of the earliest occurrence of any of `terms` in
/// `haystack_lower`, or `None` if none occur at all.
fn earliest_index(terms: &[String], haystack_lower: &str) -> Option<usize> {
    terms.iter().filter_map(|t| haystack_lower.find(&t.to_lowercase())).min()
}

/// Whether `rule.before` positionally excuses a match that `rule_matches`
/// already confirmed: true when at least one `before` term occurs strictly
/// earlier than the earliest matched `any_of` term. `before` empty, or no
/// `any_of` term to measure against (nothing for a bare `all_of` rule to be
/// "before"), both mean the constraint does not apply - never an escape,
/// exactly as if `before` were empty.
fn positionally_escaped(rule: &OptInRule, haystack_lower: &str) -> bool {
    if rule.before.is_empty() {
        return false;
    }
    let Some(trigger_at) = earliest_index(&rule.base.any_of, haystack_lower) else {
        return false;
    };
    rule.before.iter().any(|t| haystack_lower.find(&t.to_lowercase()).is_some_and(|i| i < trigger_at))
}

/// `evaluate`'s own AND/OR/NOT/min_chars matcher, reused verbatim
/// (`rule_matches`), plus the positional escape layered on top. Returns
/// every fired rule's tier and reminder, in rulebook order - the same order
/// `evaluate` returns its reminders in.
pub fn evaluate_opt_in(rules: &[OptInRule], haystack_lower: &str) -> Vec<(Tier, String)> {
    rules
        .iter()
        .filter(|r| rule_matches(&r.base, haystack_lower) && !positionally_escaped(r, haystack_lower))
        .map(|r| (r.tier, r.base.reminder.clone()))
        .collect()
}

/// SPEC-ENFORCEMENT.md section 1.1's three verdicts, restricted to the two
/// this matcher can ever produce - ALLOW is simply both fields `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardVerdict {
    /// Same shape, same "[THOR] ... || ..." formatting, as `block_reason`'s
    /// own return value - built only from `Tier::Block` fires. `Some` means
    /// exactly what it always meant: stop the reply.
    pub block_reason: Option<String>,
    /// The same formatting, built only from `Tier::Warn` fires. Never a
    /// reason to stop anything (SPEC-ENFORCEMENT.md 1.1). See this
    /// section's own doc comment for what still has to change elsewhere
    /// before this can reach the owner without blocking.
    pub warn_reason: Option<String>,
}

/// `block_reason`'s richer sibling: same two parameters, same fail-open
/// stance (no rulebook, or zero parseable rules, yields both fields
/// `None`), but reads a rulebook that may carry `before`/`tier` and keeps a
/// WARN-tier fire out of the block reason entirely, instead of discarding
/// the tier and treating every fire alike.
pub fn guard_verdict(rulebook_text: Option<&str>, message: &str) -> GuardVerdict {
    let rules = rulebook_text.map(parse_opt_in_rules).unwrap_or_default();
    if rules.is_empty() {
        return GuardVerdict { block_reason: None, warn_reason: None };
    }
    let fired = evaluate_opt_in(&rules, &haystack(message));
    let format_tier = |tier: Tier| -> Option<String> {
        let reasons: Vec<&str> = fired.iter().filter(|(t, _)| *t == tier).map(|(_, r)| r.as_str()).collect();
        if reasons.is_empty() {
            None
        } else {
            Some(format!("[THOR] {}", reasons.join("  ||  ")))
        }
    };
    GuardVerdict { block_reason: format_tier(Tier::Block), warn_reason: format_tier(Tier::Warn) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULEBOOK: &str = r#"[
      {"id":"tldr","any_of":["md5","commit ","gepusht","fsck","exit=0"],
       "none_of":["tldr","kort:","any_of"],"min_chars":600,
       "reminder":"This reads like results but has no TLDR - open with one."},
      {"id":"amnesia","any_of":["kun je checken","can you check","ik heb geen toegang"],
       "none_of":["any_of"],
       "reminder":"Check whether you can reach it yourself before asking."}
    ]"#;

    fn long(marker: &str) -> String {
        format!("{marker} {}", "x".repeat(700))
    }

    /// THE DEFECT THIS PREVENTS: a long technical answer with a commit hash and
    /// no TLDR goes out unchallenged. It was true for a whole session.
    #[test]
    fn a_long_technical_answer_without_tldr_is_blocked() {
        let msg = long("the commit fsck run is done, all gepusht");
        let reason = block_reason(Some(RULEBOOK), &msg).expect("must fire");
        assert!(reason.contains("TLDR"), "{reason}");
    }

    #[test]
    fn the_same_answer_with_a_tldr_passes() {
        let msg = long("TLDR: it works. the commit fsck run is done, all gepusht");
        assert!(block_reason(Some(RULEBOOK), &msg).is_none());
    }

    /// The min_chars floor: a two-line status note with one technical word is
    /// not a report and must not be blocked (measured defect in 1.0: a
    /// 230-char closing line blocked for the word "pushed").
    #[test]
    fn a_short_technical_note_is_below_the_floor_and_passes() {
        assert!(block_reason(Some(RULEBOOK), "commit done, gepusht").is_none());
    }

    /// The none_of escape: a message discussing the rule itself must not trip
    /// it. "any_of" appears in every rule's own none_of exactly for this.
    #[test]
    fn discussing_the_rulebook_does_not_trip_a_rule() {
        let msg = long("the tldr rule has any_of [md5, commit ] and fires on a commit ");
        assert!(block_reason(Some(RULEBOOK), &msg).is_none());
    }

    #[test]
    fn capability_amnesia_is_caught_regardless_of_length() {
        let reason = block_reason(Some(RULEBOOK), "kun je checken wat er in staat?").expect("must fire");
        assert!(reason.contains("yourself"), "{reason}");
    }

    /// Fail-open: a broken or absent rulebook blocks nothing.
    #[test]
    fn a_broken_or_absent_rulebook_blocks_nothing() {
        assert!(block_reason(None, &long("commit fsck gepusht")).is_none());
        assert!(block_reason(Some("{ not json"), &long("commit fsck gepusht")).is_none());
        assert!(block_reason(Some("[]"), &long("commit fsck gepusht")).is_none());
    }

    #[test]
    fn the_rulebook_sits_beside_the_store() {
        let p = default_rulebook_path(Path::new("C:/Users/x/thor2/thor.db"));
        assert!(p.ends_with("guard-response-rulebook.json"));
        assert_eq!(p.parent(), Some(Path::new("C:/Users/x/thor2")));
    }

    // ---------------------------------------------------------- position
    //
    // Deliberately does NOT put the summary marker ("tldr"/"kort:") in
    // `none_of` the way the live rulebook does: that would let the OLD,
    // position-blind escape pass these tests for the wrong reason. Here the
    // ONLY thing that can excuse a match is the NEW `before` field, so a
    // passing test actually proves the positional check works.
    const POSITIONED_RULEBOOK: &str = r#"[
      {"id":"tldr-positioned",
       "any_of":["commit ","fsck","gepusht"],
       "none_of":["any_of"],
       "before":["tldr","kort:"],
       "min_chars":600,
       "reminder":"open with a TLDR before the jargon"}
    ]"#;

    fn long_prefix(marker: &str) -> String {
        format!("{marker} {}", "detail ".repeat(100))
    }

    fn long_suffix(marker: &str) -> String {
        format!("{} {marker}", "detail ".repeat(100))
    }

    /// THE DEFECT THIS PREVENTS: a summary that genuinely comes first, before
    /// any jargon, still trips a summary-first rule - the shape of the b01
    /// false block (RESPONSE-GUARD-BLIND.md), fixed here at the position
    /// level instead of by growing the escape vocabulary.
    #[test]
    fn a_reply_that_opens_with_a_summary_then_jargon_does_not_trip_the_rule() {
        let msg = format!("TLDR: it works. {}", long_suffix("commit "));
        assert!(guard_verdict(Some(POSITIONED_RULEBOOK), &msg).block_reason.is_none());
    }

    /// Same words, reversed order: the jargon now comes first, so the rule
    /// must fire - proves the check is genuinely about ORDER, not merely
    /// presence of the summary marker somewhere in the text.
    #[test]
    fn the_same_words_in_the_other_order_trips_the_rule() {
        let msg = format!("{} TLDR: it works.", long_prefix("commit "));
        let reason = guard_verdict(Some(POSITIONED_RULEBOOK), &msg).block_reason.expect("must fire");
        assert!(reason.contains("before the jargon"), "{reason}");
    }

    /// No jargon at all: `any_of` never matches, so the positional escape is
    /// never even asked to decide anything - `before` must never fire a rule
    /// on its own.
    #[test]
    fn a_reply_with_no_jargon_at_all_never_trips_the_position_aware_rule() {
        let msg = format!("TLDR: it works. {}", "detail ".repeat(100));
        assert!(guard_verdict(Some(POSITIONED_RULEBOOK), &msg).block_reason.is_none());
    }

    /// A rulebook that never mentions `before` at all - every rule the owner
    /// has shipped so far - must evaluate through the new path exactly as it
    /// did through the old one. Reuses this file's own pre-existing
    /// `RULEBOOK`/`long` fixtures so the comparison is apples to apples.
    #[test]
    fn a_rulebook_without_the_before_field_behaves_exactly_as_before() {
        let blocked = long("the commit fsck run is done, all gepusht");
        assert_eq!(block_reason(Some(RULEBOOK), &blocked), guard_verdict(Some(RULEBOOK), &blocked).block_reason);

        let compliant = long("TLDR: it works. the commit fsck run is done, all gepusht");
        assert_eq!(
            block_reason(Some(RULEBOOK), &compliant),
            guard_verdict(Some(RULEBOOK), &compliant).block_reason
        );

        let short = "commit done, gepusht";
        assert_eq!(block_reason(Some(RULEBOOK), short), guard_verdict(Some(RULEBOOK), short).block_reason);
    }

    // ------------------------------------------------------------- tier
    const TIERED_RULEBOOK: &str = r#"[
      {"id":"warn-example","tier":"warn","any_of":["ik zou liever"],"none_of":["any_of"],
       "reminder":"consider phrasing this as a preference, not a directive"},
      {"id":"block-example","tier":"block","any_of":["nooit meer doen"],"none_of":["any_of"],
       "reminder":"this is a hard rule, not a suggestion"},
      {"id":"untiered-example","any_of":["mystery phrase"],"none_of":["any_of"],
       "reminder":"no tier key at all - must still behave like block"},
      {"id":"bogus-tier-example","tier":"maybe","any_of":["bogus phrase"],"none_of":["any_of"],
       "reminder":"an unrecognized tier value - must still behave like block"}
    ]"#;

    /// THE DEFECT THIS PREVENTS: a rule the owner has explicitly marked WARN
    /// (SPEC-ENFORCEMENT.md 1.1 - "emit text, never stop the action") still
    /// ends up stopping the reply, because `respond.rs` parsed the tier and
    /// then discarded it.
    #[test]
    fn a_warn_tier_rule_never_blocks() {
        let v = guard_verdict(Some(TIERED_RULEBOOK), "ik zou liever dit anders zien");
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
        let warn = v.warn_reason.expect("the warn tier must still say something");
        assert!(warn.contains("preference"), "{warn}");
    }

    #[test]
    fn a_block_tier_rule_still_blocks() {
        let v = guard_verdict(Some(TIERED_RULEBOOK), "dat nooit meer doen, begrepen?");
        let reason = v.block_reason.expect("a block-tier match must still block");
        assert!(reason.contains("hard rule"), "{reason}");
    }

    /// Today, every rule in every shipped rulebook has no `tier` key at all
    /// and blocks when it fires. A rule with no tier, or with a tier value
    /// this parser does not recognise, must default to that SAME behaviour -
    /// never silently downgrade to non-blocking just because the field is
    /// absent or misspelled.
    #[test]
    fn an_unknown_or_missing_tier_defaults_to_block() {
        let missing = guard_verdict(Some(TIERED_RULEBOOK), "a mystery phrase appears here");
        assert!(missing.block_reason.is_some(), "no tier key at all must still block");

        let bogus = guard_verdict(Some(TIERED_RULEBOOK), "a bogus phrase appears here");
        assert!(bogus.block_reason.is_some(), "an unrecognised tier value must still block");
    }

    /// A message that trips both a block-tier and a warn-tier rule at once:
    /// the warn's text must never leak into the block reason (SPEC-
    /// ENFORCEMENT.md 1.1: BLOCK requires a conclusive, cited violation, not
    /// one diluted with a merely-possible one) and vice versa.
    #[test]
    fn block_and_warn_reasons_never_mix() {
        let msg = "ik zou liever dit anders zien, en dat nooit meer doen, begrepen?";
        let v = guard_verdict(Some(TIERED_RULEBOOK), msg);
        let block = v.block_reason.expect("block-example must still fire");
        let warn = v.warn_reason.expect("warn-example must still fire");
        assert!(!block.contains("preference"), "{block}");
        assert!(warn.contains("preference"), "{warn}");
        assert!(!warn.contains("hard rule"), "{warn}");
        assert!(block.contains("hard rule"), "{block}");
    }
}
