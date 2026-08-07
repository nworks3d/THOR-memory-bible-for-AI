//! The judge transport for the capture guard (see `capture`'s own doc
//! comment for the whole picture). Measured 2026-08-05
//! (`AB-JUDGE-VS-RULEBOOK.md`, `FRESH-HOLDOUT-RESULTS.md`, both at the repo
//! root): a cheap judge model, given the exact prompt in `JUDGE_PROMPT`
//! below, catches 60/60 genuine durable decisions with 0/70 false blocks on
//! a frozen blind hold-out, against a deterministic substring rulebook's
//! 33/60 catch and 8/70 false-block rate on the SAME hold-out. That is the
//! whole reason this module exists: the rulebook (`capture::decide_capture`)
//! is demoted to a fallback, and this module is the primary decision-maker.
//!
//! No third-party HTTP client is added (workspace constraint: no new
//! dependency). The judge is instead ANY external command: THOR writes
//! `JUDGE_PROMPT` plus the owner's one prompt to the command's stdin and
//! reads its answer from stdout, under a configurable timeout enforced by
//! this module, never the OS. Configuration lives beside the store, same
//! style as the two existing rulebook files
//! (`guard-capture-rulebook.example.json`, `guard-response-rulebook.example.json`):
//! see `default_judge_config_path` / `parse_judge_config`, and
//! `guard-judge-config.example.json` at the repo root for a worked (UNVERIFIED
//! - see that file's own comment) example.
//!
//! Failure policy, identical in spirit to `respond`/`capture`: no config
//! file, a command that cannot even be spawned, a run that exceeds
//! `timeout_ms`, or output that does not parse into BLOCK/WARN/ALLOW are ALL
//! "no verdict" (`None`), never a panic, never an error surfaced to the
//! caller. The caller (`capture`'s Stop wiring, `serve/src/bin/serve.rs`)
//! is the one that turns "no verdict" into the rulebook fallback - this
//! module itself has no notion of a fallback; it only ever answers "here is
//! what the judge said" or "nothing".
//!
//! The judge is invoked ONLY at Stop (turn end), never at UserPromptSubmit -
//! a model call takes seconds, and paying that latency in front of every
//! single prompt (before the model has even started answering) is far worse
//! than paying it once, at the end, while the owner is reading the reply.
//! Nothing in this module is wired to C1; see `serve/src/bin/serve.rs`'s
//! `capture_flag` (C1) and `capture_stop_check` (C2) for where each half is
//! actually called, and `JUDGE-TRANSPORT.md` at the repo root for the design
//! this module implements plus everything about it that remains unmeasured
//! on this machine (no judge command, no `ANTHROPIC_API_KEY`, nothing
//! deployed).

use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// THE EXACT judge prompt that produced the 100%/0% result in
/// `AB-JUDGE-VS-RULEBOOK.md` (quoted verbatim there under "The judge prompt
/// (verbatim, used unchanged for all 130 items and for all latency tests)").
/// Copied byte-for-byte. Do NOT edit this string for phrasing, tone, or
/// "improvement" - a changed prompt invalidates the measurement it carries.
/// If this prompt is ever revised, the A/B measurement no longer describes
/// this code and must be re-run before the new wording is trusted.
pub const JUDGE_PROMPT: &str = r#"You are the durable-decision judge for THOR's capture guard. THOR is a memory system for an AI coding assistant. The guard fires at the end of the assistant's turn and decides whether the owner's prompt stated a DURABLE DECISION that must be recorded before the turn ends - a standing rule, a naming or style convention, or a policy change meant to apply going forward. If such a decision is stated and never recorded, it is lost.

A durable decision is a live, present-tense statement that establishes a rule for the future: "from now on...", "we always...", "new rule:...", "hard rule:...", a naming convention, a process the team should follow going forward, a reversal of a prior rule, etc. It does not need those exact words, but it must be adopting or declaring a standing rule right now, not merely discussing one.

It is NOT a durable decision when the prompt:
- only asks a QUESTION about an existing rule, without stating one
- QUOTES a rule that was already established elsewhere (a doc, a prior message, a teammate) and creates nothing new
- is HYPOTHETICAL or exploratory ("what if we did X", "suppose we always did Y", "stel dat we...") - thinking out loud about a possible rule is not adopting one
- describes ORDINARY WORK (a bug fix, a feature request, a one-off task) with no standing rule attached
- uses "always"/"never"/"nooit"/"voortaan" in a NON-NORMATIVE sense - describing a recurring bug, a fact, or a complaint, not declaring a rule
- describes a decision ALREADY MADE in the PAST and already in effect ("we already decided...", "that's been live since...") - nothing new needs capturing

Given ONE prompt (Dutch or English, coding-assistant context), return exactly one verdict:

- BLOCK: a durable decision is clearly being stated right now. High confidence only.
- WARN: possibly a decision, genuinely ambiguous or borderline. Use this instead of guessing between BLOCK and ALLOW.
- ALLOW: no durable decision is being stated - covers questions, quotes, hypotheticals, ordinary work, non-normative language, and past-tense/already-recorded decisions.

Judge each prompt entirely on its own, as if it were the only prompt you will ever see. Do not let any other prompt in this batch shift your calibration.

For each prompt, respond with exactly this format:
VERDICT: <BLOCK|WARN|ALLOW>
JUSTIFICATION: <one line, must quote the exact phrase from the prompt you relied on>"#;

/// The default per-call timeout when a config file omits `timeout_ms`. Not a
/// measured number - see `JUDGE-TRANSPORT.md`'s latency section: this
/// environment has no judge command to measure a real round trip against, so
/// this is a judgment call (comfortably above the 3.9-6.9s `duration_ms`
/// band `AB-JUDGE-VS-RULEBOOK.md` measured through its own agent-harness
/// overhead), configurable per deployment via the config file.
pub const DEFAULT_JUDGE_TIMEOUT_MS: u64 = 8000;

/// One external judge command, parsed from `guard-judge-config.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeConfig {
    pub command: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

/// The judge config sits beside the store, same stance as the rulebooks
/// (`respond::default_rulebook_path`, `capture::default_rulebook_path`): a
/// project directory could otherwise plant one and redirect every judge call
/// somewhere the owner never chose.
pub fn default_judge_config_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("guard-judge-config.json")
}

/// Parse the judge config. Missing file, malformed JSON, a missing/empty
/// `command`, or any other shape mismatch all yield `None` - fail-open: no
/// usable config means no judge, not an error (SPEC: "no config file
/// present, or the command missing -> no judge -> fallback path... This must
/// be the quiet, ordinary case, not an error").
pub fn parse_judge_config(text: &str) -> Option<JudgeConfig> {
    let v: Value = serde_json::from_str(text).ok()?;
    let command = v.get("command").and_then(|c| c.as_str()).unwrap_or("").trim().to_string();
    if command.is_empty() {
        return None;
    }
    let args = v
        .get("args")
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let timeout_ms = v.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(DEFAULT_JUDGE_TIMEOUT_MS);
    Some(JudgeConfig { command, args, timeout_ms })
}

/// What the judge decided for one prompt, plus its own one-line citation
/// (SPEC: "parse three verdicts: BLOCK, WARN, ALLOW, plus its one-line
/// citation"). `Allow` carries no citation - there is nothing to record.
#[derive(Debug, Clone, PartialEq)]
pub enum JudgeVerdict {
    Block(String),
    Warn(String),
    Allow,
}

/// Build exactly what is written to the judge command's stdin: the frozen
/// `JUDGE_PROMPT` (unchanged, see its own doc comment) followed by the ONE
/// owner prompt to judge. This framing (the "PROMPT:" header below) is this
/// implementation's own addition, NOT part of the measured prompt text -
/// `AB-JUDGE-VS-RULEBOOK.md`'s own measurement invoked the model through the
/// Agent tool's separate system/user-message split, not a single concatenated
/// stdin blob, so this exact framing was never itself run against the frozen
/// hold-out. See `JUDGE-TRANSPORT.md` for this caveat stated plainly.
pub fn build_judge_stdin(prompt_text: &str) -> String {
    format!("{JUDGE_PROMPT}\n\nPROMPT:\n{prompt_text}\n")
}

/// Known-literal placeholder text from `JUDGE_PROMPT`'s own answer-format
/// instructions above (`VERDICT: <BLOCK|WARN|ALLOW>` and the
/// `JUSTIFICATION:` placeholder that follows it). A weak local model that
/// echoes the instructions back verbatim - instead of substituting a real
/// choice - can reproduce these two exact strings. Since `JUDGE_PROMPT` is
/// frozen (see its own doc comment: it must never be edited, the A/B
/// measurement depends on it staying byte-for-byte identical), these two
/// placeholders are known in advance, so they can be stripped before parsing
/// and never mistaken for a real answer.
const ECHOED_VERDICT_PLACEHOLDER: &str = "<block|warn|allow>";
const ECHOED_JUSTIFICATION_PLACEHOLDER: &str =
    "<one line, must quote the exact phrase from the prompt you relied on>";

/// Remove every case-insensitive occurrence of `needle` from `text`.
fn strip_ci(text: &str, needle: &str) -> String {
    let mut out = text.to_string();
    loop {
        let lower = out.to_ascii_lowercase();
        let Some(idx) = lower.find(needle) else { break };
        out.replace_range(idx..idx + needle.len(), "");
    }
    out
}

/// A line consisting only of a markdown fence delimiter (`` ``` `` or `~~~`,
/// with an optional language tag such as `` ```json ``).
fn is_fence_marker(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("```") || t.starts_with("~~~")
}

/// Text prepared for line-based verdict/justification scanning: the two
/// known echoed-instruction placeholders are removed (see
/// `ECHOED_VERDICT_PLACEHOLDER` above), markdown fence-delimiter lines are
/// dropped (their CONTENT is still scanned - only the `` ``` `` markers
/// themselves are noise), and markdown emphasis characters (backtick,
/// asterisk) are stripped so `` `VERDICT: BLOCK` `` or `**VERDICT: BLOCK**`
/// read the same as a plain `VERDICT: BLOCK`. Never applied to the text
/// handed to `first_json_object`, which must see the original bytes.
fn normalize_for_line_scan(raw: &str) -> String {
    let sans_placeholder = strip_ci(raw, ECHOED_VERDICT_PLACEHOLDER);
    let sans_placeholder = strip_ci(&sans_placeholder, ECHOED_JUSTIFICATION_PLACEHOLDER);
    sans_placeholder
        .lines()
        .filter(|line| !is_fence_marker(line))
        .map(|line| line.chars().filter(|c| !matches!(c, '`' | '*')).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `tok` (already uppercased) is one of the three real verdicts.
fn is_verdict_token(tok: &str) -> bool {
    matches!(tok, "BLOCK" | "WARN" | "ALLOW")
}

/// The short alphabetic token immediately following the LAST occurrence of
/// `marker` on this one line (case-insensitive), skipping whatever
/// punctuation or whitespace sits in between (`:`, a stray markdown
/// character, quotes). `None` if `marker` does not appear on this line, or
/// nothing alphabetic follows it. "Last", not first, so a line naming the
/// marker twice reads its own real answer over an earlier mention of the
/// word (e.g. "My VERDICT: WARN reflects genuine ambiguity here").
fn token_after_marker(line: &str, marker: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let marker_lower = marker.to_ascii_lowercase();
    let idx = lower.rfind(&marker_lower)?;
    let rest = &line[idx + marker.len()..];
    let token: String =
        rest.trim_start_matches(|c: char| !c.is_ascii_alphabetic()).chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if token.is_empty() {
        None
    } else {
        Some(token.to_ascii_uppercase())
    }
}

/// The full remainder of the line after the FIRST occurrence of `marker`
/// (case-insensitive), with a leading `:` and whitespace stripped. Unlike
/// `token_after_marker` this keeps the whole citation text, not just one
/// word. `None` if `marker` does not appear on this line at all.
fn text_after_marker(line: &str, marker: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let marker_lower = marker.to_ascii_lowercase();
    let idx = lower.find(&marker_lower)?;
    let rest = &line[idx + marker.len()..];
    Some(rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace()).trim().to_string())
}

/// Whether an entire trimmed line IS one of the three verdict tokens and
/// nothing else, once simple surrounding punctuation (quotes, a trailing
/// period or colon) is stripped - the shape of a minimal answer that skips
/// the `VERDICT:` label entirely and prints just e.g. `BLOCK` on its own
/// line. `None` otherwise.
fn bare_token_line(line: &str) -> Option<String> {
    let stripped = line.trim_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '.' | ':'));
    let upper = stripped.to_ascii_uppercase();
    if is_verdict_token(&upper) {
        Some(upper)
    } else {
        None
    }
}

/// Find the first balanced top-level `{...}` substring in `text` (brace
/// depth counting, aware of quoted strings so a `}` inside a string value
/// never closes the object early). `None` if there is no `{` at all, or its
/// braces never balance (e.g. truncated output). Byte-indexed but ASCII-safe
/// even with non-ASCII text elsewhere in `text`: none of `{`, `}`, `"`, `\`
/// can ever occur as a continuation byte of a multi-byte UTF-8 sequence, so
/// scanning for them by byte never lands mid-character.
fn first_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (offset, &b) in bytes[start..].iter().enumerate() {
        let c = b as char;
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + offset + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A `{"verdict": "...", ...}`-shaped answer: the value of a top-level key
/// named `verdict` (case-insensitive), plus - best effort - a citation from
/// whichever of `reason`/`justification`/`citation` (case-insensitive) is
/// present first. `None` when no balanced JSON object is found at all, the
/// object it found is not actually an object, or it has no `verdict` key.
fn json_verdict_candidate(text: &str) -> Option<(String, Option<String>)> {
    let obj_str = first_json_object(text)?;
    let value: Value = serde_json::from_str(obj_str).ok()?;
    let map = value.as_object()?;
    let verdict = map.iter().find(|(k, _)| k.eq_ignore_ascii_case("verdict"))?.1.as_str()?.trim().to_ascii_uppercase();
    let citation = ["reason", "justification", "citation"].iter().find_map(|key| {
        map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).and_then(|(_, v)| v.as_str()).map(str::to_string)
    });
    Some((verdict, citation))
}

/// Parse the judge's raw stdout into a `JudgeVerdict`. Pure, no I/O - the
/// transport (`run_judge_command`) and the parsing are deliberately separate
/// so each can be tested on its own.
///
/// Written for a small LOCAL model, not just a well-behaved hosted one:
/// besides the clean `VERDICT: <token>` / `JUSTIFICATION: <text>` shape this
/// also reads a verdict wrapped in surrounding prose (before AND after),
/// inside a markdown code fence (with or without a language tag, and even
/// wrapped in inline backticks or bold `**...**` markers), inside a
/// `{"verdict": "...", "reason": "..."}` JSON object (however that JSON
/// itself is wrapped), restated after the model repeats the prompt or the
/// instructions back first, and regardless of case or whitespace around any
/// of the above.
///
/// THE ONE-SENTENCE RESOLUTION RULE (the hard boundary - read this before
/// touching this function): every recognized verdict signal in the answer -
/// a `VERDICT:` marker anywhere on a line, a JSON object's `verdict` field,
/// or a line that is just one bare token - is reduced to its BLOCK/WARN/ALLOW
/// value, and the result is that value ONLY when every signal found agrees
/// on exactly one distinct value; zero signals (nothing recognized at all)
/// or two-or-more DIFFERENT values both yield `None` - no verdict, never a
/// guess.
///
/// This is why a hedge like "this could be BLOCK or ALLOW" is `None` (free
/// prose merely mentioning a token, with no `VERDICT:` marker, no JSON
/// field, and not alone on its own line, is not itself a recognized signal
/// at all), and why two contradicting `VERDICT:` lines, or a JSON verdict
/// that disagrees with a `VERDICT:` line elsewhere in the same answer, are
/// also both `None` (two recognized signals that disagree, not one). `None`
/// degrades to the existing rulebook fallback (`capture::fallback_decision`),
/// which can only ever WARN - so an ambiguous or garbage judge answer can
/// warn at most, and can never, by construction, cause a block.
pub fn parse_judge_output(raw: &str) -> Option<JudgeVerdict> {
    let normalized = normalize_for_line_scan(raw);

    let mut tokens: Vec<String> = Vec::new();
    let mut justification: Option<String> = None;

    for line in normalized.lines() {
        let trimmed = line.trim();
        if let Some(tok) = token_after_marker(trimmed, "VERDICT") {
            if is_verdict_token(&tok) {
                tokens.push(tok);
            }
        }
        if let Some(tok) = bare_token_line(trimmed) {
            tokens.push(tok);
        }
        if justification.is_none() {
            if let Some(text) = text_after_marker(trimmed, "JUSTIFICATION") {
                if !text.is_empty() {
                    justification = Some(text);
                }
            }
        }
    }

    if let Some((tok, citation)) = json_verdict_candidate(raw) {
        if is_verdict_token(&tok) {
            tokens.push(tok);
        }
        if justification.is_none() {
            if let Some(c) = citation {
                if !c.is_empty() {
                    justification = Some(c);
                }
            }
        }
    }

    tokens.sort();
    tokens.dedup();
    if tokens.len() != 1 {
        // Zero recognized signals, or more than one DISTINCT value among the
        // signals found: no verdict, ever. This is the entire safety
        // property this function exists to uphold - see the doc comment
        // above.
        return None;
    }

    let citation = justification.unwrap_or_default();
    match tokens[0].as_str() {
        "BLOCK" => Some(JudgeVerdict::Block(citation)),
        "WARN" => Some(JudgeVerdict::Warn(citation)),
        "ALLOW" => Some(JudgeVerdict::Allow),
        _ => None,
    }
}

/// Run the configured command once, draining its stdout on a background
/// thread so a command that writes more than one OS pipe buffer's worth of
/// output before exiting can never deadlock this side's timeout poll (a
/// poll-without-reading loop could otherwise stall forever against a child
/// blocked writing to a full pipe). Returns the raw stdout text, or `None` on
/// ANY failure: the command cannot be spawned, it does not finish within
/// `config.timeout_ms` (the child is killed and this is treated as "no
/// verdict", never an error and never a block), its output is not valid
/// UTF-8, or (see below) this process is itself already running as a
/// descendant of a judge invocation. Parsing that text into a verdict is
/// `parse_judge_output`'s job, not this function's.
///
/// THE REENTRANCY GUARD (see `crate::reentry`'s own doc comment for the full
/// hazard and design): a real judge command - the headless `claude` CLI, in
/// particular - starts a REAL child session that may load THOR's own hooks.
/// Without a guard, that child session's Stop would spawn ANOTHER judge,
/// unbounded. Two independent checks close this:
///
/// 1. `reentry::is_reentrant()`, checked FIRST, before anything else in this
///    function: if THIS process is already running under the mark (i.e. it
///    is itself a descendant of an earlier judge call - the belt-and-braces
///    case; the primary guard is `cmd_hook` in `serve/src/bin/serve.rs`,
///    which exits before ever reaching this call at all), refuse to spawn
///    anything and return `None` - "no verdict", the exact same fail-open
///    shape as a command that cannot be spawned or a run that times out.
///    Never a block, never a panic, never a hang.
/// 2. `reentry::mark_child`, applied to the `Command` below before it is
///    spawned: the ONE child this function ever starts (the configured judge
///    command itself) is marked one level deeper than this process' own
///    depth. Every process THAT command goes on to start - the `claude`
///    binary, any hook it fires, any MCP server it registers - inherits the
///    mark automatically, the OS's own child-process env inheritance, not
///    something this function has to arrange further down the tree.
pub fn run_judge_command(config: &JudgeConfig, prompt_text: &str) -> Option<String> {
    if crate::reentry::is_reentrant() {
        // A judge call somehow nested inside another judge call's own
        // descendant tree - refuse outright, spawn nothing. See this
        // function's own doc comment and `crate::reentry` for why this can
        // only ever degrade to "no verdict", never a block.
        return None;
    }

    let stdin_text = build_judge_stdin(prompt_text);

    let mut command = Command::new(&config.command);
    command.args(&config.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::reentry::mark_child(&mut command);
    let mut child: Child = command.spawn().ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_text.as_bytes());
        // `stdin` is dropped at the end of this block, closing the write end
        // so the command sees EOF and can stop waiting for more input.
    }

    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let deadline = Instant::now() + Duration::from_millis(config.timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // The command never finished in time: kill it and report
                    // no verdict - never an error, never a block (SPEC: "an
                    // overrun is treated as 'no verdict'").
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }

    // The process has already exited, so the draining thread should finish
    // almost immediately; a short grace period, not a second timeout policy.
    let buf = rx.recv_timeout(Duration::from_millis(2000)).ok()?;
    String::from_utf8(buf).ok()
}

/// The whole judge call, transport plus parsing: spawn the configured
/// command, feed it this one prompt, and parse its answer - or `None` on any
/// failure at either stage. The one function `serve/src/bin/serve.rs`'s Stop
/// wiring actually calls.
pub fn run_judge(config: &JudgeConfig, prompt_text: &str) -> Option<JudgeVerdict> {
    let raw = run_judge_command(config, prompt_text)?;
    parse_judge_output(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_judge_config_sits_beside_the_store() {
        let p = default_judge_config_path(Path::new("C:/Users/x/thor2/thor.db"));
        assert!(p.ends_with("guard-judge-config.json"));
        assert_eq!(p.parent(), Some(Path::new("C:/Users/x/thor2")));
    }

    /// THE DEFECT THIS PREVENTS: a missing or unreadable config file is
    /// treated as an error instead of the quiet, ordinary "no judge
    /// configured" case.
    #[test]
    fn a_missing_or_malformed_config_yields_no_judge() {
        assert!(parse_judge_config("{ not json").is_none());
        assert!(parse_judge_config("{}").is_none(), "no command at all");
        assert!(parse_judge_config(r#"{"command":""}"#).is_none(), "an empty command must not be usable");
        assert!(parse_judge_config(r#"{"command":"   "}"#).is_none(), "whitespace-only is still empty");
    }

    #[test]
    fn a_well_formed_config_parses_command_args_and_timeout() {
        let cfg = parse_judge_config(r#"{"command":"claude","args":["-p","--model","haiku"],"timeout_ms":5000}"#)
            .expect("must parse");
        assert_eq!(cfg.command, "claude");
        assert_eq!(cfg.args, vec!["-p", "--model", "haiku"]);
        assert_eq!(cfg.timeout_ms, 5000);
    }

    #[test]
    fn a_missing_timeout_falls_back_to_the_default() {
        let cfg = parse_judge_config(r#"{"command":"claude"}"#).expect("must parse");
        assert_eq!(cfg.timeout_ms, DEFAULT_JUDGE_TIMEOUT_MS);
        assert!(cfg.args.is_empty());
    }

    #[test]
    fn a_well_formed_block_answer_parses_with_its_citation() {
        let raw = "VERDICT: BLOCK\nJUSTIFICATION: \"from now on always run the linter first\"";
        match parse_judge_output(raw) {
            Some(JudgeVerdict::Block(citation)) => {
                assert!(citation.contains("from now on always run the linter first"), "{citation}")
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn warn_and_allow_also_parse() {
        assert_eq!(
            parse_judge_output("VERDICT: WARN\nJUSTIFICATION: ambiguous"),
            Some(JudgeVerdict::Warn("ambiguous".to_string()))
        );
        assert_eq!(parse_judge_output("VERDICT: ALLOW\nJUSTIFICATION: just a question"), Some(JudgeVerdict::Allow));
    }

    #[test]
    fn parsing_is_case_and_whitespace_insensitive() {
        assert_eq!(
            parse_judge_output("  verdict:   block  \n  justification:   quoted text  "),
            Some(JudgeVerdict::Block("quoted text".to_string()))
        );
    }

    /// THE DEFECT THIS PREVENTS: a judge answer that does not parse into one
    /// of the three verdicts is treated as some other verdict (e.g. silently
    /// defaulting to BLOCK) instead of "no verdict at all".
    #[test]
    fn an_unparseable_answer_is_no_verdict() {
        assert!(parse_judge_output("").is_none());
        assert!(parse_judge_output("I think this is fine, no rule stated.").is_none());
        assert!(parse_judge_output("VERDICT: MAYBE\nJUSTIFICATION: unsure").is_none());
        assert!(parse_judge_output("JUSTIFICATION: only a citation, no verdict line").is_none());
    }

    #[test]
    fn a_missing_justification_still_yields_a_verdict_with_an_empty_citation() {
        assert_eq!(parse_judge_output("VERDICT: BLOCK"), Some(JudgeVerdict::Block(String::new())));
    }

    // ---- Tolerant parsing for a small local model's messier answers ----
    // (Task: `parse_judge_output` must handle a messier answer than a hosted
    // model gives - see the fixture set in
    // `serve/tests/judge_parse_fixtures.rs` for a much larger, labeled set
    // of realistic messy outputs and their per-class parse rates. These six
    // are the specific named regressions called out for this module.)

    /// THE DEFECT THIS PREVENTS: a model that reasons out loud before (and
    /// after) stating its verdict - normal behavior for a small local model,
    /// unlike the hosted model the original clean-only parser assumed - is
    /// read as "no verdict" just because the VERDICT line is not the only
    /// thing on stdout.
    #[test]
    fn a_verdict_wrapped_in_prose_is_still_read() {
        let raw = "Let me think about this prompt carefully.\n\
                   The user is clearly stating a standing rule for the future, not asking a question.\n\n\
                   VERDICT: BLOCK\n\
                   JUSTIFICATION: \"from now on always run the linter first\"\n\n\
                   Hope that reasoning makes sense!";
        match parse_judge_output(raw) {
            Some(JudgeVerdict::Block(citation)) => {
                assert!(citation.contains("from now on always run the linter first"), "{citation}")
            }
            other => panic!("expected Block despite the surrounding prose, got {other:?}"),
        }
    }

    /// THE DEFECT THIS PREVENTS: a chat-tuned local model wraps its whole
    /// answer in a markdown code fence (plain, language-tagged, or even
    /// wraps just the VERDICT line in inline backticks/bold) and the fence
    /// markers themselves are read as part of the verdict line, or hide it
    /// from a strict `starts_with` check.
    #[test]
    fn a_fenced_verdict_is_still_read() {
        assert_eq!(
            parse_judge_output("```\nVERDICT: WARN\nJUSTIFICATION: genuinely borderline\n```"),
            Some(JudgeVerdict::Warn("genuinely borderline".to_string())),
            "plain triple-backtick fence"
        );
        assert_eq!(
            parse_judge_output("```text\nVERDICT: ALLOW\nJUSTIFICATION: just a question\n```"),
            Some(JudgeVerdict::Allow),
            "language-tagged fence"
        );
        assert_eq!(
            parse_judge_output("**VERDICT: BLOCK**\n**JUSTIFICATION:** \"the new rule is\""),
            Some(JudgeVerdict::Block("\"the new rule is\"".to_string())),
            "bolded verdict and justification lines"
        );
    }

    /// THE DEFECT THIS PREVENTS: the judge answers in the
    /// `{"verdict": "...", "reason": "..."}` shape a JSON-preferring local
    /// model naturally reaches for, and it is read as unparseable because it
    /// never contains a `VERDICT:`-prefixed line at all.
    #[test]
    fn a_json_shaped_verdict_is_still_read() {
        assert_eq!(
            parse_judge_output(r#"{"verdict": "BLOCK", "reason": "from now on always run the linter first"}"#),
            Some(JudgeVerdict::Block("from now on always run the linter first".to_string())),
            "clean top-level json"
        );
        assert_eq!(
            parse_judge_output("Sure, here is my answer:\n\n{\"verdict\": \"allow\", \"justification\": \"just a question\"}\n\nLet me know if you need more."),
            Some(JudgeVerdict::Allow),
            "json wrapped in prose, lowercase value"
        );
        assert_eq!(
            parse_judge_output("```json\n{\"verdict\": \"WARN\", \"reason\": \"could go either way\"}\n```"),
            Some(JudgeVerdict::Warn("could go either way".to_string())),
            "json inside a fence"
        );
        // Even TRUNCATED json - missing its closing brace, so
        // `first_json_object` never finds a balanced object at all, and the
        // structured "reason" field is never recovered either - still gets
        // its VERDICT read, because the raw text still literally contains an
        // unambiguous `"verdict": "BLOCK"` for the generic line-based
        // marker scan to find on its own, independent of JSON parsing
        // succeeding. This is deliberate tolerance, not an accident: the
        // line scan and the JSON extractor are two independent signal
        // sources that happen to agree on the verdict here, even though only
        // the JSON path could ever have recovered the citation, and it
        // didn't get the chance to.
        assert_eq!(
            parse_judge_output("{\"verdict\": \"BLOCK\", \"reason\": \"from now on always\""),
            Some(JudgeVerdict::Block(String::new())),
            "truncated json missing its closing brace: verdict still readable as plain text, citation lost"
        );
    }

    /// THE DEFECT THIS PREVENTS: an answer that names more than one verdict
    /// in a way nothing here can safely resolve - two contradicting
    /// `VERDICT:` lines, a hedge naming two tokens with no structured signal
    /// at all, or a JSON verdict that disagrees with a `VERDICT:` line
    /// elsewhere - gets resolved by guessing (e.g. "first one wins") instead
    /// of yielding no verdict at all. This is the hard safety boundary: see
    /// `parse_judge_output`'s own doc comment for the one-sentence rule.
    #[test]
    fn an_ambiguous_answer_yields_no_verdict() {
        assert!(
            parse_judge_output(
                "VERDICT: BLOCK\nJUSTIFICATION: looks like a rule\n\
                 ...actually, on reflection...\n\
                 VERDICT: ALLOW\nJUSTIFICATION: no, it's just a question"
            )
            .is_none(),
            "two contradicting VERDICT: lines"
        );
        assert!(
            parse_judge_output("This is a tough one - it could be BLOCK or ALLOW depending on how you read it.").is_none(),
            "a hedge naming two verdicts, with no structured signal at all"
        );
        assert!(
            parse_judge_output("{\"verdict\": \"BLOCK\"}\n\nVERDICT: WARN\nJUSTIFICATION: unsure").is_none(),
            "a json verdict that disagrees with a VERDICT: line elsewhere in the same answer"
        );
    }

    /// THE DEFECT THIS PREVENTS: unrelated, empty, or truncated output from
    /// a misbehaving judge command is read as some verdict (worst case,
    /// BLOCK) instead of "no verdict at all" - the same property
    /// `an_unparseable_answer_is_no_verdict` already covers for the
    /// well-behaved-hosted-model shape, restated here against messier,
    /// small-local-model-shaped garbage.
    #[test]
    fn garbage_yields_no_verdict_and_never_blocks() {
        for raw in [
            "",
            "   \n\n  ",
            "I'm sorry, I can't help with that request.",
            "VERD",
            // A json blob with no "verdict" key at all: json extraction
            // finds no candidate, and nothing on the line reads as a
            // VERDICT: marker or a bare token either.
            "{\"status\": \"ok\", \"note\": \"model warmed up\"}",
            // The common English word "block" appearing in ordinary
            // prose/code must never be mistaken for the verdict token BLOCK:
            // there is no "VERDICT" marker anywhere in this text at all.
            "Sure, let's look at this code block:\n```rust\nfn foo() { block_on(fut) }\n```",
        ] {
            assert_eq!(parse_judge_output(raw), None, "expected no verdict for {raw:?}");
        }
    }

    /// THE DEFECT THIS PREVENTS: the safety property stated as one sentence
    /// in `parse_judge_output`'s own doc comment - "no verdict" degrading
    /// into a guessed BLOCK anywhere along the tolerant-parsing path - is
    /// exercised directly here as its own invariant, over every ambiguous
    /// and garbage fixture above: not one of them may ever come back as
    /// `Some(JudgeVerdict::Block(_))`. `None` can only ever reach the
    /// rulebook fallback (`capture::fallback_decision`, untouched by this
    /// module), which is itself forced to `Tier::Warn` regardless of what
    /// tier its own rulebook JSON assigns - so nothing on this path can ever
    /// cause a block.
    #[test]
    fn no_verdict_can_never_produce_a_block() {
        let no_verdict_inputs = [
            "",
            "   \n\n  ",
            "I'm sorry, I can't help with that request.",
            "VERD",
            "{\"status\": \"ok\", \"note\": \"model warmed up\"}",
            "VERDICT: BLOCK\nJUSTIFICATION: a\n\nVERDICT: ALLOW\nJUSTIFICATION: b",
            "This is a tough one - it could be BLOCK or ALLOW depending on how you read it.",
            "{\"verdict\": \"BLOCK\"}\n\nVERDICT: WARN\nJUSTIFICATION: unsure",
            "VERDICT: MAYBE\nJUSTIFICATION: unsure",
        ];
        for raw in no_verdict_inputs {
            let result = parse_judge_output(raw);
            assert_ne!(
                result,
                Some(JudgeVerdict::Block(String::new())),
                "must never be Block for {raw:?}, got {result:?}"
            );
            assert!(
                !matches!(result, Some(JudgeVerdict::Block(_))),
                "must never be ANY Block for {raw:?}, got {result:?}"
            );
            assert_eq!(result, None, "these inputs must all yield no verdict at all, not just 'not Block', for {raw:?}");
        }
    }

    /// THE DEFECT THIS PREVENTS: a model that echoes `JUDGE_PROMPT`'s own
    /// literal answer-format placeholder (`VERDICT: <BLOCK|WARN|ALLOW>`)
    /// before giving its real answer has that placeholder text misread as
    /// the verdict `BLOCK` (the first alphabetic run inside the placeholder)
    /// - or, worse, creates a spurious second distinct token that makes a
    /// real, unambiguous answer look ambiguous.
    #[test]
    fn an_echoed_format_placeholder_does_not_shadow_the_real_answer() {
        let raw = "You asked me to answer in the format:\n\
                   VERDICT: <BLOCK|WARN|ALLOW>\n\
                   JUSTIFICATION: <one line, must quote the exact phrase from the prompt you relied on>\n\n\
                   My real answer:\n\
                   VERDICT: ALLOW\n\
                   JUSTIFICATION: just a question";
        assert_eq!(parse_judge_output(raw), Some(JudgeVerdict::Allow));
    }

    /// THE DEFECT THIS PREVENTS: spawning a command that does not exist on
    /// this system panics or hangs instead of failing open.
    #[test]
    fn a_command_that_cannot_be_spawned_yields_no_verdict() {
        let cfg = JudgeConfig {
            command: "this-command-does-not-exist-anywhere-12345".to_string(),
            args: vec![],
            timeout_ms: 2000,
        };
        assert!(run_judge_command(&cfg, "from now on always run the linter first").is_none());
    }

    // The judge's OWN hard refusal when the reentrancy mark is already
    // present (belt-and-braces, independent of `cmd_hook`'s guard in
    // `serve/src/bin/serve.rs`) is proven in `serve/tests/
    // judge_refuses_when_marked.rs`, not here: `CARGO_BIN_EXE_*` (needed to
    // locate the real `fake_judge` fixture binary) is only defined for
    // integration test binaries, not for this crate's own `--lib` unit
    // tests - see that file's own doc comment for the full reasoning,
    // including why it is deliberately the ONLY test in its own file.
}
