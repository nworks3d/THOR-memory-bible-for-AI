//! The ONE owner of the memory footer format:
//! `[memory/<type> | tags: <t1 t2> | project: <key|global> | mimir:<id>]`
//! (the mimir-compatible convention; the trailing mimir field only appears on
//! imported facts). Composing at write time and parsing at read time used to
//! live in four call sites that shared the format by convention only - the MCP
//! writer, the type classifier, the dedup/snippet stripper, and the backfill
//! project parser. A format drift would break them silently and asymmetrically
//! (facts written by one side, unreadable by another), so BOTH sides live here
//! and the old call sites keep thin shims.

use crate::event_store::{Event, EventKind};
use crate::repo::FactType;
use std::collections::HashMap;

/// Compose the footer for a fact written at type-aware write time (MCP
/// remember). Fields are sanitized here so a caller can never corrupt the
/// format: see field_safe. `project_label` is a project key or "global".
/// `triggers` is the author-declared firing vocabulary ("when should this
/// fact surface?" - single task words, space-joined); `anchors` are exact
/// file paths / command strings the guard matches verbatim (comma-joined:
/// an anchor may contain spaces). Empty lists = no field, so every
/// pre-existing footer stays byte-identical.
pub fn compose(
    fact_type: &str,
    tags: &[String],
    project_label: &str,
    triggers: &[String],
    anchors: &[String],
    expires: Option<&str>,
) -> String {
    compose_full(fact_type, tags, project_label, triggers, anchors, expires, None)
}

/// Like `compose`, plus an optional `provenance` field (verified | inferred) -
/// the epistemic origin of the fact at write time. Written BEFORE the `project`
/// field so project stays the footer's last field (the project parser keys on
/// that). Stripped for ranking like every other footer field; only the courier
/// reads it, to append a reconcile hint to an inferred fact when it resurfaces.
#[allow(clippy::too_many_arguments)]
pub fn compose_full(
    fact_type: &str,
    tags: &[String],
    project_label: &str,
    triggers: &[String],
    anchors: &[String],
    expires: Option<&str>,
    provenance: Option<&str>,
) -> String {
    let ty = {
        let t = field_safe(fact_type).to_lowercase();
        if t.is_empty() { "note".to_string() } else { t }
    };
    let tags = join_words(tags);
    let fires = join_words(triggers);
    let anchors = join_anchors(anchors);
    let mut out = format!("[memory/{} | tags: {}", ty, tags);
    if !fires.is_empty() {
        out.push_str(&format!(" | fires-when: {}", fires));
    }
    if !anchors.is_empty() {
        out.push_str(&format!(" | anchors: {}", anchors));
    }
    if let Some(exp) = expires {
        let exp = field_safe(exp);
        if !exp.is_empty() {
            out.push_str(&format!(" | expires: {}", exp));
        }
    }
    if let Some(p) = provenance {
        let p = field_safe(p);
        if !p.is_empty() {
            out.push_str(&format!(" | provenance: {}", p));
        }
    }
    out.push_str(&format!(" | project: {}]", project_label));
    out
}

/// Space-joined, field-safe word list (the tags / fires-when serialization).
fn join_words(xs: &[String]) -> String {
    xs.iter().map(|t| field_safe(t)).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Comma-joined, field-safe anchor list. An anchor may contain spaces (a
/// command phrase), so entries are comma-separated; commas INSIDE an anchor
/// would split it and are folded to spaces. A space-joined anchor list is
/// the measured dead-anchor class: it parses as ONE never-matching anchor.
fn join_anchors(xs: &[String]) -> String {
    xs.iter()
        .map(|t| field_safe(t))
        .map(|a| a.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// File stems that name a file's ROLE, not its subject. As a bare anchor (no
/// directory) each of these fires on EVERY file with that name, in every
/// project - the guard matches `a == name_l` (guard.rs).
const GENERIC_STEMS: [&str; 12] = [
    "main", "mod", "lib", "index", "test", "tests", "util", "utils", "config", "types", "app",
    "readme",
];
/// Bare tool names. As a one-word command anchor each fires on every command
/// that mentions the tool at all - `git` would advise on every git call.
const BARE_TOOLS: [&str; 12] = [
    "git", "cargo", "docker", "npm", "npx", "ssh", "scp", "python", "node", "bash", "powershell",
    "pwsh",
];

/// Is this anchor too broad or dead to be a gate? Returns the reason, or None
/// when the anchor is specific enough to be worth writing.
///
/// WHY THIS EXISTS: an anchor is the ONLY way a fact reaches the moment-of-action
/// guard, so raising anchor coverage is the highest-value write-side change -
/// but the guard's ratchet has zero noise headroom (16 catch / 0 noise), and a
/// broad anchor is worse than none. It re-fires on every command it happens to
/// hit, and - because a touched declared anchor is the strong evidence leg of
/// auto-echo - it then manufactures its own proof of usefulness, which feeds
/// ranking and the decay pass. Bad anchors here are self-reinforcing, so they
/// are refused at the write, not cleaned up later.
///
/// THE ONE DEFINITION: this is the single answer to "could the guard ever match
/// this, and would it match the right thing". The write refusal (MCP remember/
/// revise) and the write-time proposal (anchor_candidate, which consolidate's
/// work list also reads) all pass through here, so the floor and the proposal
/// cannot drift apart. Measured 2026-07-26: before this, both proposal-side
/// calls were structurally inert - the floor only knew one-word shapes, the
/// proposal only produced paths and two-word commands - so nothing the
/// proposal generated could ever be refused, and 14 dead proposals shipped.
///
/// Deliberately narrow: the two measured BROAD classes (a bare role-name file,
/// a bare tool name) plus the provably DEAD single-token path shapes (glob,
/// truncation, ref-prefix, glued enumeration - see dead_path_anchor). A
/// distinctive bare filename like `courier.rs` is fine and stays allowed - the
/// point is specificity, not punctuation.
pub fn overbroad_anchor(anchor: &str) -> Option<String> {
    let a = anchor.trim();
    if a.is_empty() {
        return None;
    }
    let lower = a.to_lowercase();
    let looks_like_path = lower.contains('/') || lower.contains('\\');
    if looks_like_path {
        if !a.contains(char::is_whitespace) {
            return dead_path_anchor(a, &lower);
        }
        // A command phrase containing a path ("scp x admin@host:/dest",
        // "bash deploy/run.sh"): a colon or wildcard can be literal command
        // text there, and the guard matches commands by substring - legal.
        return None;
    }
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.len() == 1 {
        let one = words[0];
        let stem = one.rsplit_once('.').map(|(s, _)| s).unwrap_or(one);
        if one.contains('.') && GENERIC_STEMS.contains(&stem) {
            return Some(format!(
                "'{a}' names a file's role, not its subject - as a bare anchor it fires on every \
                 {one} in every project. Anchor the PATH instead (e.g. src/{one})"
            ));
        }
        if !one.contains('.') && BARE_TOOLS.contains(&one) {
            return Some(format!(
                "'{a}' is a bare tool name - it would advise on every {a} command. Anchor the \
                 specific invocation instead (e.g. '{a} <subcommand> <target>')"
            ));
        }
    }
    None
}

/// Single-token path shapes the guard can NEVER match: the file pass compares
/// an anchor verbatim against real touched paths (full path, suffix, or bare
/// name - guard.rs), so each of these looks like a gate and gates nothing,
/// while still counting toward anchor coverage. Only single tokens come here -
/// a multi-word anchor is a command phrase, handled above.
fn dead_path_anchor(a: &str, lower: &str) -> Option<String> {
    if lower.contains('*') || lower.contains('?') {
        return Some(format!(
            "'{a}' contains a glob - the guard matches paths verbatim and never expands one. \
             Anchor one concrete path (or the full command that uses the glob)"
        ));
    }
    if lower.contains("...") {
        return Some(format!(
            "'{a}' is truncated ('...') and can never match a real path. Anchor the full path"
        ));
    }
    // A `<ref>:` prefix (a chunk id or project ref, "Proj:src/a.rs") is not a
    // path. A single letter before the colon is a Windows drive and stays legal.
    let first_sep = lower.find(['/', '\\']).unwrap_or(lower.len());
    if lower[..first_sep].find(':').is_some_and(|i| i >= 2) {
        return Some(format!(
            "'{a}' carries a '<ref>:' prefix (a chunk id or project ref, not a path) - the \
             guard compares against real file paths. Anchor the path part alone"
        ));
    }
    // Two file names glued with a slash ("main.rs/lib.rs"): the guard would
    // match neither. A single-letter extension on a DIRECTORY is the conf.d
    // convention, not a file, so only >= 2 letters count as glue evidence.
    let comps: Vec<&str> = lower.split(['/', '\\']).collect();
    if comps[..comps.len() - 1].iter().any(|c| alpha_extension(c).is_some_and(|e| e.len() >= 2)) {
        return Some(format!(
            "'{a}' glues two file names with a slash - the guard would match neither. \
             Write them as separate anchors"
        ));
    }
    None
}

/// The trailing extension of `file` when it reads as a REAL extension
/// (non-empty stem, 1-5 letters). Without this, a version range like
/// "0.9.073/0.9.074" reads as a path with extension "074".
fn alpha_extension(file: &str) -> Option<&str> {
    file.rsplit_once('.').and_then(|(stem, ext)| {
        (!stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphabetic()))
        .then_some(ext)
    })
}

/// Tools whose invocations have a subcommand vocabulary worth proposing. The
/// interpreter/transport tools in BARE_TOOLS (bash, python, node, ssh, scp,
/// powershell, pwsh, npx) take a TARGET, not a subcommand - their proposable
/// anchor is the target path, which the path branch already finds - so they
/// never generate a command proposal. Measured 2026-07-26: "bash en" (Dutch
/// prose read as an invocation) was half of the 14 false proposals on the
/// live store; a subcommand vocabulary kills the whole class.
const TOOL_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("git", &[
        "add", "bisect", "branch", "checkout", "cherry-pick", "clone", "commit", "diff", "fetch",
        "log", "merge", "pull", "push", "rebase", "remote", "reset", "restore", "stash", "status",
        "switch", "tag",
    ]),
    ("cargo", &[
        "add", "bench", "build", "check", "clippy", "doc", "fmt", "install", "publish", "run",
        "test", "tree", "update",
    ]),
    ("docker", &[
        "build", "compose", "cp", "exec", "inspect", "logs", "ps", "pull", "push", "restart",
        "rm", "rmi", "run", "start", "stop",
    ]),
    ("npm", &["audit", "ci", "install", "link", "pack", "publish", "run", "test", "update"]),
];

/// The most specific file path or command invocation this body names, if any -
/// the anchor a fact PROBABLY wants. Used only to make a write-time proposal
/// concrete ("this body names X"); nothing is ever anchored automatically, and
/// a wrong guess costs one sentence in a tool reply.
///
/// Deliberately conservative: a path with a directory separator and a file
/// extension, or a known tool followed by one of ITS OWN subcommands
/// (TOOL_SUBCOMMANDS). Every candidate then passes through `overbroad_anchor` -
/// the same floor the write refusal uses - so the proposal can never offer an
/// anchor the store would then reject, and tightening the floor tightens the
/// proposal in the same change.
pub fn anchor_candidate(body: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for raw in body.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '\'' | '`')) {
        let t = raw.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '.' | '[' | ']' | '!' | '?'));
        if t.len() < 6 || t.len() > 80 {
            continue;
        }
        // A line reference is still the same file: routes/qms.js:30 -> routes/qms.js.
        let t = t.split_once(':').map_or(t, |(head, tail)| {
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) { head } else { t }
        });
        if t.contains('=') {
            continue; // an env assignment names a value, not a file the guard can watch
        }
        let file = t.rsplit(['/', '\\']).next().unwrap_or(t);
        let looks_path = (t.contains('/') || t.contains('\\'))
            && alpha_extension(file).is_some()
            && !t.starts_with("http")
            && !t.contains("://");
        if looks_path && overbroad_anchor(t).is_none() {
            // Prefer the first path named: bodies lead with their subject.
            best.get_or_insert_with(|| t.to_string());
        }
    }
    if best.is_some() {
        return best;
    }
    // No path - try a tool invocation ("cargo build", "docker compose") from
    // word PAIRS of the original text. Tokenizing gives the word boundary
    // ("device_bash en draai" is not a bash call - a real miss before this),
    // and the tool is matched case-sensitively lowercase: a command is typed
    // in lowercase, so prose that merely names the tool ("PowerShell venster",
    // a real false proposal) skips.
    let words: Vec<&str> = body
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| {
                matches!(c, ',' | ';' | ':' | '.' | '(' | ')' | '[' | ']' | '"' | '\'' | '`' | '!' | '?')
            })
        })
        .collect();
    for pair in words.windows(2) {
        let Some((tool, subs)) = TOOL_SUBCOMMANDS.iter().find(|(t, _)| *t == pair[0]) else {
            continue;
        };
        if subs.contains(&pair[1]) {
            let cand = format!("{tool} {}", pair[1]);
            // By construction the pair passes the floor; the call stays so the
            // shared definition is enforced here, not by convention.
            if overbroad_anchor(&cand).is_none() {
                return Some(cand);
            }
        }
    }
    None
}

/// Metadata overrides for `edit_footer`: `None` = leave that field exactly as
/// it is; `Some(empty)` = remove the field (tags stay present but empty - the
/// format always writes them). Born from the dead-anchor repair sessions,
/// where changing ONE field meant hand-retyping the whole footer and three
/// separate gotchas guarded the ways that goes wrong.
#[derive(Default)]
pub struct FieldEdits {
    pub fact_type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub triggers: Option<Vec<String>>,
    pub anchors: Option<Vec<String>>,
    /// `Some(None)` clears the date; `Some(Some(d))` sets it.
    pub expires: Option<Option<String>>,
    /// `Some(None)` clears; `Some(Some(p))` sets.
    pub provenance: Option<Option<String>>,
}

impl FieldEdits {
    pub fn is_empty(&self) -> bool {
        self.fact_type.is_none()
            && self.tags.is_none()
            && self.triggers.is_none()
            && self.anchors.is_none()
            && self.expires.is_none()
            && self.provenance.is_none()
    }
}

/// Field surgery on a footer LINE: apply `edits` and leave every other field
/// byte-for-byte as it was - including the `project:` field (reproject owns
/// that) and a trailing `mimir:<id>` import marker (the has_source_ref
/// idempotence key). Fields are (re)written at their canonical position:
/// tags, fires-when, anchors, expires, provenance, project, mimir.
/// Returns None when `footer` is not a `[memory/...]` line.
pub fn edit_footer(footer: &str, edits: &FieldEdits) -> Option<String> {
    let inner = footer.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut segments = inner.split(" | ");
    let ty_seg = segments.next()?;
    let old_ty = ty_seg.strip_prefix("memory/")?;
    // Collect the existing fields verbatim; unknown names ride along behind
    // provenance so nothing an older or newer binary wrote is dropped.
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut mimir_tail: Option<String> = None;
    for seg in segments {
        if seg.starts_with("mimir:") {
            mimir_tail = Some(seg.to_string());
        } else if let Some((name, value)) = seg.split_once(": ") {
            fields.push((name.to_string(), value.to_string()));
        } else if let Some(name) = seg.strip_suffix(':') {
            fields.push((name.to_string(), String::new()));
        } else {
            fields.push((seg.to_string(), String::new()));
        }
    }
    fn set(fields: &mut Vec<(String, String)>, name: &str, value: Option<String>) {
        match value.filter(|v| !v.is_empty()) {
            Some(v) => {
                if let Some(f) = fields.iter_mut().find(|(n, _)| n == name) {
                    f.1 = v;
                } else {
                    fields.push((name.to_string(), v));
                }
            }
            None => fields.retain(|(n, _)| n != name),
        }
    }
    if let Some(tags) = &edits.tags {
        // tags is always present in the format, possibly empty
        if let Some(f) = fields.iter_mut().find(|(n, _)| n == "tags") {
            f.1 = join_words(tags);
        } else {
            fields.push(("tags".to_string(), join_words(tags)));
        }
    }
    if let Some(triggers) = &edits.triggers {
        set(&mut fields, "fires-when", Some(join_words(triggers)));
    }
    if let Some(anchors) = &edits.anchors {
        set(&mut fields, "anchors", Some(join_anchors(anchors)));
    }
    if let Some(exp) = &edits.expires {
        set(&mut fields, "expires", exp.as_ref().map(|d| field_safe(d)));
    }
    if let Some(prov) = &edits.provenance {
        set(&mut fields, "provenance", prov.as_ref().map(|p| field_safe(p)));
    }
    let ty = match &edits.fact_type {
        Some(t) => {
            let t = field_safe(t).to_lowercase();
            if t.is_empty() { old_ty.to_string() } else { t }
        }
        None => old_ty.to_string(),
    };
    // Rebuild in canonical order; anything unknown keeps its relative place
    // after the known fields (before project).
    const ORDER: &[&str] = &["tags", "fires-when", "anchors", "expires", "provenance"];
    let mut out = format!("[memory/{}", ty);
    let mut emitted: Vec<usize> = Vec::new();
    for name in ORDER {
        if let Some(i) = fields.iter().position(|(n, _)| n == name) {
            out.push_str(&format!(" | {}: {}", name, fields[i].1));
            emitted.push(i);
        } else if *name == "tags" {
            out.push_str(" | tags: ");
        }
    }
    for (i, (n, v)) in fields.iter().enumerate() {
        if emitted.contains(&i) || n == "project" {
            continue;
        }
        if v.is_empty() {
            out.push_str(&format!(" | {}", n));
        } else {
            out.push_str(&format!(" | {}: {}", n, v));
        }
    }
    if let Some(i) = fields.iter().position(|(n, _)| n == "project") {
        out.push_str(&format!(" | project: {}", fields[i].1));
    }
    if let Some(m) = mimir_tail {
        out.push_str(&format!(" | {}", m));
    }
    out.push(']');
    Some(out)
}

/// Parse the footer's `| provenance: <verified|inferred>` field: the fact's
/// epistemic origin at write time. None when absent. Read only by the courier.
pub fn provenance(body: &str) -> Option<String> {
    let idx = body.find("| provenance: ")?;
    let rest = &body[idx + "| provenance: ".len()..];
    let v = rest.split(" |").next()?.trim().trim_end_matches(']').trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Parse the footer's `| expires: YYYY-MM-DD` field: the date after which the
/// fact stops surfacing in recall (history keeps it - losslessness holds; the
/// filter is rank-time, never an eviction). None when absent.
pub fn expires(body: &str) -> Option<String> {
    let idx = body.find("| expires: ")?;
    let rest = &body[idx + "| expires: ".len()..];
    let date = rest.split(" |").next()?.trim().trim_end_matches(']').trim();
    (!date.is_empty()).then(|| date.to_string())
}

/// Today as YYYY-MM-DD (UTC), for the rank-time expiry compare.
pub fn today() -> String {
    days_from_today(0)
}

/// A UTC calendar date `days` from today, as YYYY-MM-DD. Civil-date from
/// days-since-epoch (Howard Hinnant's algorithm) - no chrono dependency, and
/// deliberately NOT usable from the fold modules (cas/auditor stay clock-free;
/// test_2_purity_no_time enforces that).
pub fn days_from_today(days: i64) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + days * 86_400;
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Does this body declare ITSELF a milestone / progress report in its opening
/// words? Keyed on the author's own opener, never on guessed content - a fact is
/// report-shaped because it says so, exactly like the historical-demotion rule.
///
/// Why the write path cares. Measured on this project's own store (2026-07-25):
/// 61 such facts, 11.6% of ALL stored text, a third longer than the average
/// fact, and not one carried an expiry. They are long by nature (a day's work
/// written up) and length is what lets them cover any question by mass, so a
/// chain of them outranks the actual answer on every question about their
/// subject: expiring nine superseded release reports took one real question from
/// 3 noise hits out of 5 to 1. Cleaning that up by hand does not scale and does
/// not last, which is why the tool now defaults it - see REPORT_EXPIRY_DAYS.
/// Deliberately NARROW, and it stays narrow: this is the predicate behind the
/// SILENT half of the lifecycle (mcp.rs auto_expiry dates the fact without
/// asking). Measured 2026-07-29 on the real store: every wider opener rule that
/// finds the reports this one misses also mislabels about one in four - and a
/// mislabel here silently dates a RULE, the failure the doctrine exists to
/// prevent. The wider net lives in `reads_as_report`, on the reviewed surface.
pub fn report_shaped(body: &str) -> bool {
    opens_with(body, &SHIPPED_OPENERS)
}

/// The two openers this store has shipped with since the auto-expiry landed.
const SHIPPED_OPENERS: [&str; 2] = ["MILESTONE", "MIJLPAAL"];

/// Reads as a progress report to a human, on the wider vocabulary - for the
/// REVIEWED surface only (`thor consolidate`, which proposes and never applies).
///
/// Why this is a second predicate and not a widening of `report_shaped`.
/// Measured 2026-07-29, real store, 816 live facts, three blind classifiers on
/// the catch (two agreed exactly, the third returned one label for everything
/// and carried no signal):
///   - shipped openers:            0 caught. The live "FASE 2 KLAAR" and
///     "... STEP 4 DONE" reports were invisible to BOTH mechanisms.
///   - this rule:                 36 caught, 31 real reports, 5 mislabels (86%).
///   - marker anywhere in line 1: 42 caught, and it degenerates - THOR facts are
///     one long paragraph, so `lines()` returns the whole body. It matched FASE
///     inside "GEFASEERDE" and STEP inside "STEP file standard" (a CAD format).
/// A fourth idea died on the same data: the footer's fact_type does not separate
/// them. The mislabels are typed `decision` and `note`, exactly like the reports.
///
/// So 86% is good enough to hand an agent a worklist, and not good enough to
/// date a fact behind their back. That split is the whole design.
///
/// What acting on the list actually bought, measured end to end on the same day
/// over the 504-question corpus, through POST /inject (the block a session is
/// really served - NOT the CLI's `thor recall`, which is bm25 only, and would
/// have compared bm25 to bm25): expiring 16 confirmed reports took the served
/// facts drawn from them from 21 to 0, and from 8 first-slot hits to 0, with the
/// total served count unchanged and no question left empty. A blind three-judge
/// panel over the 20 attributable changes: 7 better, 1 worse, 12 indifferent.
/// The one loss is honest and expected - a question that asks about a historical
/// plan is best answered by that plan.
pub fn reads_as_report(body: &str) -> bool {
    report_shaped(body)
        || (!rule_shaped(body)
            && (opens_with(body, &WIDE_OPENERS) || head_names(body, &WIDE_OPENERS)))
}

/// Openers a body ALSO uses to declare itself a report, measured on the real
/// store 2026-07-29: live progress reports ("FASE 2 KLAAR", "acme-shop deploy
/// FASE 2 - STEP 4 DONE", "FASE 3 DEPLOY - HISTORISCHE CONFIG-NOTITIE") were
/// invisible to SHIPPED_OPENERS, so they never got an expiry at the write AND
/// never reached the consolidate worklist - the two mechanisms that exist to
/// catch exactly them.
///
/// Every term below earned its place, and the ones that did not are gone.
/// Leave-one-out on the real store: AFGEROND carries 10 catches, FASE, HISTORI
/// and KLAAR 2 each, VOORTGANG and ACHTERHAALD 1. Candidates were then measured
/// the same way and kept only at or above the list's own precision (blind
/// panel, three judges): GEBOUWD 6/6, GESHIPT 5/5, SHIPPED 1/1, VOLTOOID 1/1.
/// REJECTED with their numbers, so nobody re-proposes them: LIVE catches 10 but
/// only 6 are reports (60% - it fires on "draait LIVE op ...", which is standing
/// config), OPGELOST 7 of 9 (78% - no gain over the list, and one miss is a
/// pointer registry). DONE, COMPLETE and WORKING caught nothing at all.
/// STEP was DROPPED: zero unique catches (a "STEP 4" title always carries FASE
/// too) against a real collision - "STEP file" is a CAD format, ordinary
/// vocabulary in this store.
///
/// THE ENGLISH HALF IS REASONED, NOT MEASURED, and that has to be said out
/// loud. The store this was measured on has ONE Dutch author, so a term like
/// DONE catching nothing here is evidence about how HE writes, not about
/// whether it opens a report. Judging English terms by that measurement would
/// ship a detector that only works in Dutch - a defect for every other user,
/// and the same trap the tool's own worklist exists to prevent. So each English
/// entry is the mirror of a Dutch term that DID earn its place: DONE, COMPLETE
/// and FINISHED for AFGEROND (10 catches), BUILT for GEBOUWD (6), SHIPPED for
/// GESHIPT (5), PHASE for FASE (2), PROGRESS for VOORTGANG, OUTDATED and
/// SUPERSEDED for ACHTERHAALD. Verified inert on the measured store: adding
/// them catches nothing new there, so they cost that store no false positives.
/// What is NOT known is their precision on an English store. Measure before
/// trusting the 86% figure for one.
const WIDE_OPENERS: [&str; 18] = [
    "FASE", "PHASE", "HISTORI", "PROGRESS", "VOORTGANG", "ACHTERHAALD", "KLAAR", "AFGEROND",
    "GEBOUWD", "GESHIPT", "SHIPPED", "VOLTOOID", "DONE", "COMPLETE", "FINISHED", "BUILT",
    "OUTDATED", "SUPERSEDED",
];

/// Body STARTS with one of these openers (the shipped match: prefix of the
/// trimmed body, case-insensitive).
fn opens_with(body: &str, openers: &[&str]) -> bool {
    let head = body.trim_start();
    openers
        .iter()
        .any(|o| head.len() >= o.len() && head[..o.len()].eq_ignore_ascii_case(o))
}

/// How far into the body a self-declaration still counts as an opener. A report
/// names itself in its title ("acme-shop deploy FASE 2 - STEP 4 DONE"), which
/// is why a strict prefix match misses it, but THOR facts are written as
/// ONE long paragraph - so `lines()` gives the whole body back and a first-line
/// match degenerates into "anywhere". Measured 2026-07-29: that degeneration is
/// what caught FASE inside "GEFASEERDE" and STEP inside "STEP file standard"
/// (a CAD format, ordinary vocabulary in this store).
///
/// 120 is not a round number, it is the top of a plateau. Swept on the real
/// store: 40 -> 20 caught, 80 -> 23, 120 -> 23, 200 -> 26, 400 -> 29. Then the
/// positions say why: past 120 the next markers sit at 215, 270, 384, 397, 566,
/// 867, 1255, 2438 chars in - that is mid-paragraph, which is not a title and
/// therefore not a self-declaration at all. Three of those deep matches are
/// confirmed mislabels. Widen this and you are no longer reading how the author
/// named the fact, you are keyword-searching its contents.
const HEAD_WINDOW_CHARS: usize = 120;

/// The marker appears in the opening window, case-SENSITIVE - a lowercase
/// "fase" mid-prose is not a self-declaration. At position 0 `opens_with` is
/// case-insensitive instead: a body that STARTS with the word is declaring
/// itself whatever its capitalisation ("Fase 1 webhook GEBOUWD").
fn head_names(body: &str, markers: &[&str]) -> bool {
    let head: String = body.trim_start().chars().take(HEAD_WINDOW_CHARS).collect();
    markers.iter().any(|m| head.contains(m))
}

/// Does this body declare ITSELF a rule in its opening line? Keyed on the
/// author's own words (uppercase markers, substring of the FIRST non-empty
/// line so "MIJLPAAL + HARDE REGEL" classifies too), never on guessed content -
/// same philosophy as report_shaped and the historical-demotion rule.
///
/// Why the write path cares: the lifecycle doctrine (2026-07-26) is
/// "reports expire, RULES never". A rule that rides an expiry silently stops
/// surfacing in recall on its date - measured live: a HARDE REGEL fact expired
/// unnoticed in the 2026-07-24 report cleanup. Setting `expires` on a
/// rule-opening body is therefore probably a mistake; the write warns (never
/// refuses: a genuinely temporary rule - "pin to v1.9 until upstream fixes
/// it" - is legitimate and keeps its date).
pub fn rule_shaped(body: &str) -> bool {
    const MARKERS: [&str; 7] = [
        "HARDE REGEL", "HARD RULE", "GOTCHA", "REGEL:", "VOORKEUR", "PREFERENCE", "DOCTRINE",
    ];
    let Some(first) = body.lines().find(|l| !l.trim().is_empty()) else { return false };
    MARKERS.iter().any(|m| first.contains(m))
}

/// How long a self-declared report keeps competing in recall before it steps
/// aside. Six weeks: this project shipped eleven releases in three weeks, so a
/// release report is superseded within days, and the honest operational half-life
/// of "what we did that day" is weeks, not forever. Nothing is deleted - `get`
/// and `history` keep serving it in full, and one `revise` removes the expiry
/// for the rare report that must keep surfacing.
pub const REPORT_EXPIRY_DAYS: i64 = 42;

/// Valid `expires` value at write time: strictly YYYY-MM-DD with a plausible
/// month/day. Refusing malformed dates at the write keeps the recall-time
/// comparison a plain string compare (ISO dates order lexicographically).
pub fn valid_expiry(date: &str) -> bool {
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| date[r].chars().all(|c| c.is_ascii_digit());
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let month: u32 = date[5..7].parse().unwrap_or(0);
    let day: u32 = date[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// Strip characters that would corrupt the footer's field structure - including
/// control characters: an interior newline would make the footer span two
/// lines, which strip() no longer strips, permanently defeating the
/// near-duplicate checks for that fact.
pub fn field_safe(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '|' | '[' | ']'))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Leading first-line markers that convention already uses in hand-written
/// bodies. Case-sensitive uppercase on purpose: prose ("the decision was...")
/// must not classify, a deliberate "DECISION: ..." must. EN + NL.
const TYPE_MARKERS: &[(&str, FactType)] = &[
    ("GOTCHA", FactType::Gotcha),
    ("DECISION", FactType::Decision),
    ("BESLISSING", FactType::Decision),
    ("BESLUIT", FactType::Decision),
    ("PREFERENCE", FactType::Preference),
    ("VOORKEUR", FactType::Preference),
    ("WERKVOORKEUR", FactType::Preference),
    ("WERKWIJZE-VOORKEUR", FactType::Preference),
    ("HARDE REGEL", FactType::Preference),
    ("REGEL:", FactType::Preference),
    ("AFSPRAAK", FactType::Preference),
];

/// Classify a fact body: the `[memory/<type> ...]` footer (the exact format
/// compose() writes and the mimir import carries) wins, else a leading
/// uppercase marker on the first non-empty line. None for chunks, notes, and
/// everything untyped.
pub fn fact_type(body: &str) -> Option<FactType> {
    // Footer: the LAST line that starts with '[' and carries "memory/<type>".
    for line in body.lines().rev() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("[memory/") {
            let ty: String = rest.chars().take_while(|c| c.is_ascii_alphabetic() || *c == '-').collect();
            return match ty.as_str() {
                "gotcha" => Some(FactType::Gotcha),
                "decision" => Some(FactType::Decision),
                "preference" => Some(FactType::Preference),
                _ => None, // a typed footer of another class (note, insight, ...) is authoritative
            };
        }
    }
    // Leading marker on the first non-empty line.
    let first = body.lines().find(|l| !l.trim().is_empty())?.trim_start();
    TYPE_MARKERS
        .iter()
        .find(|(marker, _)| first.starts_with(marker))
        .map(|(_, ty)| *ty)
}

/// Strip a trailing single-line `[...]` metadata footer (the mimir/type/chunk
/// convention: separated by a blank line, one bracketed line, nothing after).
pub fn strip(body: &str) -> &str {
    let trimmed = body.trim_end();
    if !trimmed.ends_with(']') {
        return body;
    }
    match trimmed.rfind("\n\n[") {
        Some(i) if !trimmed[i + 2..].contains('\n') => &body[..i],
        _ => body,
    }
}

/// The trailing footer LINE itself (`[memory/... | project: X]`), or None when
/// the body has none. The inverse of [`strip`], which returns the content.
pub fn extract(body: &str) -> Option<&str> {
    let trimmed = body.trim_end();
    if !trimmed.ends_with(']') {
        return None;
    }
    match trimmed.rfind("\n\n[") {
        Some(i) if !trimmed[i + 2..].contains('\n') => Some(trimmed[i + 2..].trim()),
        _ => None,
    }
}

/// A revised body with the PREVIOUS head's footer re-attached, when the caller
/// dropped it. Returns None when nothing needs doing (the new body already
/// carries a footer, or the old head had none).
///
/// Why this exists: the footer is not a separate field, it is the body's tail -
/// so `revise` with a rewritten body silently drops the fact's type, tags,
/// fires-when vocabulary and the guard's anchors. The fact stays findable
/// (recall reads the content), so the loss is invisible: it just never fires at
/// the moment of action again, which was the whole point of writing it. That is
/// a correctness bug in the tool, not a caller mistake to be scolded for -
/// carrying the metadata across a CONTENT edit is what the caller meant.
///
/// Deliberately not "always overwrite": a new body that brings its own footer
/// wins, so retyping / re-anchoring a fact stays possible in one call.
pub fn carry_over(new_body: &str, prev_body: &str) -> Option<String> {
    if extract(new_body).is_some() {
        return None; // the caller supplied a footer - theirs wins
    }
    let prev_footer = extract(prev_body)?;
    Some(format!("{}\n\n{}", new_body.trim_end(), prev_footer))
}

/// Write-time footer integrity check for agent-supplied bodies (MCP
/// revise/remember). Catches the two defect classes measured live in the v5
/// diagnosis: (1) trailing garbage after the footer's closing `]` - typically
/// a "Kind: fact_created" line pasted back from a CLI dump - which breaks
/// strip() and fact_type(); (2) a footer glued to the content without the
/// blank-line separator, which strip() can never find. A body WITHOUT any
/// `[memory/...` marker passes (untyped facts are legitimate); a body WITH
/// one must round-trip. Returns a human-readable defect, or None when clean.
pub fn write_defect(body: &str) -> Option<String> {
    let Some(marker) = body.rfind("[memory/") else {
        return None; // no footer intended - nothing to validate
    };
    let trimmed = body.trim_end();
    if !trimmed.ends_with(']') {
        return Some(
            "footer is followed by trailing text after its closing ']' (did a CLI dump line like \
             'Kind: ...' get pasted into the body?) - strip()/fact_type() would break; end the \
             body at the footer's ']'"
                .to_string(),
        );
    }
    let has_separator = matches!(trimmed.rfind("\n\n["), Some(i) if !trimmed[i + 2..].contains('\n'));
    if !has_separator {
        return Some(
            "footer is not separated from the content by a blank line (the convention is \
             '<content>\\n\\n[memory/...]', one bracketed line, nothing after) - strip() would \
             never find it"
                .to_string(),
        );
    }
    // The bracketed tail must BE the marker's line (not a marker buried mid-body
    // with a different bracketed line at the end).
    if trimmed[marker..].contains('\n') {
        return Some(
            "the [memory/...] marker is not on the final footer line - move the footer to the \
             single trailing bracketed line"
                .to_string(),
        );
    }
    None
}

/// A live fact whose footer is damaged. The event log itself is always intact
/// here - this is CONTENT health, which is why `thor fsck` reports it without
/// failing: nothing is corrupt, a fact has just stopped carrying its metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Defect {
    /// The head carries no footer while an ancestor still did: the fingerprint
    /// of a revise written by a pre-carry_over binary (see carry_over). `footer`
    /// is the nearest ancestor's, ready to re-attach to the CURRENT body.
    Wiped { entity_id: String, rev: String, from_rev: String, footer: String },
    /// The head's footer is structurally broken (see write_defect).
    Malformed { entity_id: String, rev: String, reason: String },
}

impl Defect {
    pub fn entity_id(&self) -> &str {
        match self {
            Defect::Wiped { entity_id, .. } | Defect::Malformed { entity_id, .. } => entity_id,
        }
    }

    pub fn rev(&self) -> &str {
        match self {
            Defect::Wiped { rev, .. } | Defect::Malformed { rev, .. } => rev,
        }
    }
}

/// Every live fact whose footer is damaged, folded from the log (events in seq
/// order). The counterpart of carry_over on the READ side: carry_over stops the
/// damage at the write, this surfaces what an older binary already did - a
/// fact that silently stopped firing at the moment of action can otherwise only
/// be noticed by missing it.
///
/// Only CONTENT-bearing heads count (created/revised): a retract body is a
/// tombstone and a supersede points elsewhere, so neither is expected to carry
/// a footer - the same rule carry_over applies. Chunk ids are skipped: their
/// trailing `[repo file | ...]` line is the ingest's, not a memory's.
///
/// Why the ancestor comparison and not "no footer = defect": the footer is not
/// a separate field, it is the body's tail, and a fact that never had one is
/// legitimate (untyped facts exist by design). Only "an ancestor had one and
/// the head does not" is evidence of a LOSS.
pub fn defects(events: &[Event]) -> Vec<Defect> {
    let heads = crate::cas::compute_head_sets(events);
    let by_hash: HashMap<&str, &Event> = events.iter().map(|e| (e.this_hash.as_str(), e)).collect();

    let mut out = Vec::new();
    for (entity_id, head_set) in &heads {
        if crate::repo::is_chunk_id(entity_id) {
            continue;
        }
        for rev in &head_set.heads {
            let Some(head) = by_hash.get(rev.as_str()) else { continue };
            if !matches!(head.kind, EventKind::FactCreated | EventKind::FactRevised) {
                continue;
            }
            if let Some(reason) = write_defect(&head.body) {
                out.push(Defect::Malformed {
                    entity_id: entity_id.clone(),
                    rev: rev.clone(),
                    reason,
                });
                continue;
            }
            if extract(&head.body).is_some() {
                continue;
            }
            // Walk back to the nearest ancestor that still carried one. A
            // tombstone in between simply has no footer, so the walk passes it.
            let mut parent = head.parent_rev.as_deref();
            while let Some(p) = parent {
                let Some(ancestor) = by_hash.get(p) else { break };
                if let Some(footer) = extract(&ancestor.body) {
                    out.push(Defect::Wiped {
                        entity_id: entity_id.clone(),
                        rev: rev.clone(),
                        from_rev: ancestor.this_hash.clone(),
                        footer: footer.to_string(),
                    });
                    break;
                }
                parent = ancestor.parent_rev.as_deref();
            }
        }
    }
    // Head-sets fold into a HashMap, so sort for a stable, diffable report.
    out.sort_by(|a, b| (a.entity_id(), a.rev()).cmp(&(b.entity_id(), b.rev())));
    out
}

/// Parse the footer's `| project: <name> |` field, if present.
pub fn project(body: &str) -> Option<String> {
    let idx = body.find("| project: ")?;
    let rest = &body[idx + "| project: ".len()..];
    let proj = rest.split(" |").next()?.trim();
    // The field's value ends at the next separator OR the footer's closing
    // bracket (the project field is last when there is no mimir id).
    let proj = proj.trim_end_matches(']').trim();
    (!proj.is_empty()).then(|| proj.to_string())
}

/// True when the body carries a footer with a project attribution (the signal
/// review-scope trusts: mimir already attributed or confirmed-global the fact).
pub fn has_project_field(body: &str) -> bool {
    body.contains("| project: ")
}

#[cfg(test)]
mod write_defect_tests {
    use super::*;

    #[test]
    fn write_defect_catches_the_measured_defect_classes() {
        // clean typed body
        assert!(write_defect("a rule\n\n[memory/gotcha | tags: x | project: P]").is_none());
        // untyped body without any footer: legitimate
        assert!(write_defect("just a plain note without a footer").is_none());
        // defect 1: CLI-dump tail after the closing bracket
        let tail = "a rule\n\n[memory/gotcha | tags: x | project: P]\nKind: fact_created";
        assert!(write_defect(tail).unwrap().contains("trailing text"));
        // defect 2: footer glued to the content (no blank-line separator)
        let glued = "a rule\n[memory/decision | tags: x | project: P]";
        assert!(write_defect(glued).unwrap().contains("blank line"));
        // defect 3: marker buried mid-body, different bracketed tail
        let buried = "text [memory/gotcha | tags: x] more\n\n[other]";
        assert!(write_defect(buried).is_some());
    }
}

/// Parse the footer's `| fires-when: <words> |` field: the author-declared
/// firing vocabulary that recall's trigger bonus reads. None when absent.
pub fn fires_when(body: &str) -> Option<String> {
    let idx = body.find("| fires-when: ")?;
    let rest = &body[idx + "| fires-when: ".len()..];
    let words = rest.split(" |").next()?.trim().trim_end_matches(']').trim();
    (!words.is_empty()).then(|| words.to_string())
}

/// True when the footer's `| tags: <t1 t2> |` field carries this tag (case- and
/// order-insensitive, whole tag only - "wegwijzer" must not match "wegwijzers").
pub fn has_tag(body: &str, tag: &str) -> bool {
    let Some(idx) = body.find("| tags: ") else { return false };
    let rest = &body[idx + "| tags: ".len()..];
    let Some(field) = rest.split(" |").next() else { return false };
    let want = tag.to_lowercase();
    field.trim().trim_end_matches(']').split_whitespace().any(|t| t.to_lowercase() == want)
}

/// Parse the footer's `| anchors: <a1, a2> |` field: the exact file paths /
/// command strings the guard matches verbatim. Empty when absent.
pub fn anchors(body: &str) -> Vec<String> {
    let Some(idx) = body.find("| anchors: ") else { return Vec::new() };
    let rest = &body[idx + "| anchors: ".len()..];
    let Some(field) = rest.split(" |").next() else { return Vec::new() };
    field
        .trim()
        .trim_end_matches(']')
        .split(',')
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}

/// True when the TRAILING footer carries a source-store reference (`mimir:<id>`):
/// the fact is the import-synced copy of an external source of truth, so its
/// lifecycle (revision, decay) is decided THERE and flows in via the importer.
/// Anchored to the same footer shape strip() owns (blank-line-separated single
/// bracketed trailing line): prose that merely QUOTES the footer syntax
/// mid-body must never classify a native fact as imported.
pub fn has_source_ref(body: &str) -> bool {
    let trimmed = body.trim_end();
    if !trimmed.ends_with(']') {
        return false;
    }
    match trimmed.rfind("\n\n[") {
        Some(i) if !trimmed[i + 2..].contains('\n') => trimmed[i + 2..].contains("| mimir:"),
        _ => false,
    }
}

#[cfg(test)]
mod defect_tests {
    use super::*;

    fn mk(seq: i64, entity: &str, kind: EventKind, parent: Option<&str>, this: &str, body: &str) -> Event {
        Event {
            seq,
            event_uuid: format!("uuid-{seq}"),
            session_id: "s".to_string(),
            lineage_id: "l".to_string(),
            actor: "a".to_string(),
            kind,
            entity_id: entity.to_string(),
            parent_rev: parent.map(|s| s.to_string()),
            body: body.to_string(),
            body_ch: body.to_string(),
            prev_hash: String::new(),
            this_hash: this.to_string(),
        }
    }

    fn footer_of(ty: &str) -> String {
        compose(ty, &["x".into()], "global", &[], &["anchor.rs".into()], None)
    }

    fn typed(ty: &str, text: &str) -> String {
        format!("{}\n\n{}", text, footer_of(ty))
    }

    /// The whole point: a fact damaged by a pre-carry_over binary is invisible
    /// (it stays findable, it just never fires again), so the ONLY way to see
    /// it is the log itself - an ancestor had a footer, the head does not.
    #[test]
    fn defects_reports_a_wiped_footer_with_the_footer_to_re_attach() {
        let events = vec![
            mk(1, "mem-1", EventKind::FactCreated, None, "A", &typed("gotcha", "old")),
            mk(2, "mem-1", EventKind::FactRevised, Some("A"), "B", "rewritten body, footer dropped"),
        ];
        let got = defects(&events);
        assert_eq!(
            got,
            vec![Defect::Wiped {
                entity_id: "mem-1".to_string(),
                rev: "B".to_string(),
                from_rev: "A".to_string(),
                footer: footer_of("gotcha"),
            }],
            "the report must carry the footer itself, or the repair needs a history dig"
        );
    }

    #[test]
    fn defects_walks_back_past_intermediate_footerless_revisions() {
        // Damage found two revisions later must still cite the ORIGINAL footer.
        let events = vec![
            mk(1, "mem-1", EventKind::FactCreated, None, "A", &typed("decision", "v1")),
            mk(2, "mem-1", EventKind::FactRevised, Some("A"), "B", "v2 without footer"),
            mk(3, "mem-1", EventKind::FactRevised, Some("B"), "C", "v3 still without footer"),
        ];
        let got = defects(&events);
        assert_eq!(got.len(), 1, "one live head, one defect: {got:?}");
        assert!(matches!(&got[0], Defect::Wiped { rev, from_rev, footer, .. }
            if rev == "C" && from_rev == "A" && *footer == footer_of("decision")));
    }

    #[test]
    fn defects_stays_silent_on_every_legitimate_shape() {
        let events = vec![
            // footer carried across a revise: the fixed path
            mk(1, "mem-ok", EventKind::FactCreated, None, "A", &typed("gotcha", "old")),
            mk(2, "mem-ok", EventKind::FactRevised, Some("A"), "B", &typed("gotcha", "new")),
            // never had a footer: untyped facts are legitimate, not damage
            mk(3, "mem-untyped", EventKind::FactCreated, None, "C", "a plain note"),
            mk(4, "mem-untyped", EventKind::FactRevised, Some("C"), "D", "a plain note, edited"),
            // retracted: the tombstone body is not expected to carry a footer
            mk(5, "mem-gone", EventKind::FactCreated, None, "E", &typed("decision", "obsolete")),
            mk(6, "mem-gone", EventKind::FactRetracted, Some("E"), "F", "[retracted: superseded]"),
            // a chunk's trailing line is the ingest's, not a memory footer
            mk(7, "P:src/a.rs#0", EventKind::FactCreated, None, "G", "fn a() {}\n\n[repo file | P/src/a.rs | chunk 1/1]"),
            mk(8, "P:src/a.rs#0", EventKind::FactRevised, Some("G"), "H", "fn a() { b(); }"),
        ];
        assert_eq!(defects(&events), vec![], "no defect may be invented");
    }

    #[test]
    fn defects_reports_a_structurally_broken_footer() {
        let broken = format!("{}\nKind: fact_created", typed("gotcha", "a rule"));
        let events = vec![mk(1, "mem-1", EventKind::FactCreated, None, "A", &broken)];
        let got = defects(&events);
        assert!(matches!(&got[0], Defect::Malformed { rev, reason, .. }
            if rev == "A" && reason.contains("trailing text")), "{got:?}");
    }

    #[test]
    fn defects_reports_both_heads_of_a_diverged_fact() {
        // A diverged fact needs `resolve` before a repair can land, but the
        // damage must still be visible - silence would read as "clean".
        // Both writers revised from A: the second no longer cites a head, so it
        // branches instead of fast-forwarding (see cas::compute_head_sets).
        let events = vec![
            mk(1, "mem-1", EventKind::FactCreated, None, "A", &typed("gotcha", "v1")),
            mk(2, "mem-1", EventKind::FactRevised, Some("A"), "B", "branch one, no footer"),
            mk(3, "mem-1", EventKind::FactRevised, Some("A"), "C", "branch two, no footer"),
        ];
        let got = defects(&events);
        assert_eq!(got.len(), 2, "both live heads are damaged: {got:?}");
        assert_eq!(got[0].rev(), "B", "output is sorted, so the report is diffable");
        assert_eq!(got[1].rev(), "C");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A broad anchor is worse than no anchor: the guard matches a bare file
    /// NAME against every file with that name, and a touched anchor also feeds
    /// the fact usage credit - so a coincidental match manufactures its own
    /// proof of usefulness. The floor refuses exactly the two measured broad
    /// classes and nothing else; a distinctive bare filename stays legal.
    #[test]
    fn the_anchor_floor_refuses_role_names_and_bare_tools() {
        for broad in ["mod.rs", "main.rs", "index.ts", "README.md", "git", "docker", "CARGO"] {
            assert!(
                overbroad_anchor(broad).is_some(),
                "{broad} would fire on everything and must be refused"
            );
        }
        for fine in [
            "thor/src/courier.rs",
            "src/mod.rs",
            "courier.rs",
            "git push origin main",
            "docker compose up -d",
            "deploy-requested.flag",
        ] {
            assert!(overbroad_anchor(fine).is_none(), "{fine} is specific enough and must pass");
        }
    }

    /// The dead-path classes: single-token shapes the guard can never match
    /// against a real touched path. Before 2026-07-26 the floor knew none of
    /// these, which made BOTH of the proposal's floor checks structurally
    /// inert - nothing the proposal generated could ever be refused.
    #[test]
    fn the_anchor_floor_refuses_dead_path_shapes() {
        for dead in [
            "thor/src/*.rs",
            "src/**/*.ts",
            "docs/design.../notes.md",
            "The-AI-memory-bible:docs/REFERENCE.md",
            "main.rs/lib.rs",
        ] {
            assert!(overbroad_anchor(dead).is_some(), "{dead} can never fire and must be refused");
        }
        for fine in [
            "C:/Users/dev/thor/src/guard.rs", // a drive letter is not a ref prefix
            "conf.d/app.conf",               // a .d directory is not a glued file
            ".claude/settings.json",
            "scp backup.tar admin@host:/srv/backups", // command phrase: colon is literal
            "bash deploy/run.sh",
        ] {
            assert!(overbroad_anchor(fine).is_none(), "{fine} is matchable and must pass");
        }
    }

    /// The proposal has to be concrete or it is noise. It offers a path with a
    /// directory, or a real invocation - never the bare names the floor would
    /// then reject, because proposing a rejected anchor is worse than silence.
    #[test]
    fn the_anchor_proposal_is_concrete_or_absent() {
        assert_eq!(
            anchor_candidate("the watcher unpacks and rebuilds; see deploy/deploy-watcher.sh for the loop"),
            Some("deploy/deploy-watcher.sh".to_string())
        );
        assert_eq!(
            anchor_candidate("always build with cargo build --release --features semantic"),
            Some("cargo build".to_string())
        );
        assert_eq!(anchor_candidate("prefer the least surprising option"), None);
        // Shapes that LOOK like a path and are not - each was a real false
        // proposal on the live store before the rules that kill them.
        assert_eq!(anchor_candidate("bumped from 0.9.073/0.9.074 on the dev box"), None,
            "a version range is not a path: '074' is not an extension");
        assert_eq!(anchor_candidate("set ORDER_DB_PATH=/app/data/db/orders.db in compose"), None,
            "an env assignment names a value, not a file the guard can watch");
        assert_eq!(anchor_candidate("the check lives in routes/qms.js:30 today"),
            Some("routes/qms.js".to_string()), "a line reference is still the same file");
        assert_eq!(anchor_candidate("er ging een PowerShell venster open"), None,
            "prose mentioning a tool by name is not an invocation");
        assert_eq!(
            anchor_candidate("open https://example.com/thing.html in a browser"),
            None,
            "a URL is not a file this repo owns"
        );
        // Whatever it proposes must survive its own floor.
        for body in [
            "edit mod.rs when the module list changes",
            "run git when you are unsure",
            "the fix lives in thor/src/guard.rs",
        ] {
            if let Some(c) = anchor_candidate(body) {
                assert!(overbroad_anchor(&c).is_none(), "proposed a broad anchor: {c}");
            }
        }
        // The 2026-07-26 false-proposal classes, each measured on the live
        // store before the fix.
        assert_eq!(anchor_candidate("start device_bash en draai het script"), None,
            "a word ENDING in a tool name is not an invocation - tokenizing is the word boundary");
        assert_eq!(anchor_candidate("het script device_git push faalde"), None,
            "same boundary for a tool that DOES have a vocabulary");
        assert_eq!(anchor_candidate("gebruik git en cargo voor de build"), None,
            "'en' is prose - the second word must be the tool's own subcommand");
        assert_eq!(anchor_candidate("run git when you are unsure"), None,
            "'git when' passed the charset-only rule and shipped as a real proposal");
        assert_eq!(anchor_candidate("zie thor/src/*.rs voor de details"), None,
            "a glob is a dead anchor - the floor check is live now and refuses it");
        assert_eq!(anchor_candidate("de chunk The-AI-memory-bible:docs/REFERENCE.md zegt het"), None,
            "a chunk ref is not a path the guard can match");
        assert_eq!(anchor_candidate("run git push before leaving"), Some("git push".to_string()));
        assert_eq!(anchor_candidate("start het script met bash deploy/run.sh"),
            Some("deploy/run.sh".to_string()),
            "an interpreter's TARGET is the anchor, via the path branch");
        // Every proposal tool must also be a known bare tool - one vocabulary,
        // two strictnesses, no third list.
        for (tool, _) in TOOL_SUBCOMMANDS {
            assert!(BARE_TOOLS.contains(tool), "{tool} missing from BARE_TOOLS");
        }
    }

    /// The doctrine hinge (2026-07-26): reports expire, rules never - so the
    /// write must recognize a rule by the author's own opener; prose never
    /// classifies, and a marker past the first line never does either.
    #[test]
    fn rule_shaped_keys_on_the_authors_opening_line() {
        for rule in [
            "HARDE REGEL - nooit thor import draaien",
            "MIJLPAAL + HARDE REGEL: repo blijft prive",
            "GOTCHA cargo build --examples herbouwt de bin niet",
            "WERKVOORKEUR: eerst recall, dan pas plannen",
        ] {
            assert!(rule_shaped(rule), "{rule}");
        }
        for not_rule in [
            "MIJLPAAL: v9 gepubliceerd, keten mee",
            "de harde regel is dat we eerst meten",
            "verslag van de dag\nHARDE REGEL verderop telt niet",
            "",
        ] {
            assert!(!rule_shaped(not_rule), "{not_rule:?}");
        }
    }

    /// Every case below is a REAL body from the 2026-07-29 measurement, kept
    /// verbatim in shape. The split it pins: the wide reading may propose
    /// (`reads_as_report`), only the narrow one may date a fact silently
    /// (`report_shaped`), and the two false friends are why.
    #[test]
    fn reads_as_report_widens_the_proposal_without_widening_the_silent_expiry() {
        for report in [
            "FASE 2 KLAAR + BEWEZEN (2026-06-29): de centrale hub draait LIVE",
            "acme-shop deploy FASE 2 - STEP 4 DONE (2026-06-26). (4a) ...",
            "BOUW-VOORTGANG (2026-07-07, v2-workstreams gestart)",
            "THOR drift/stewardship-ronde AFGEROND (2026-07-09; destijds 3 commits)",
            "HISTORIE - NOOIT GEBOUWD, GEEN OPENSTAANDE ACTIE. PLAN-LOCK",
            "SPEED-FIX GESHIPT NAAR MAIN (2026-07-15, commit a6b2d35)",
            "NAS-DEPLOY v3 VOLTOOID (2026-07-10, sluit het open punt)",
            // Position 0 is a declaration whatever its capitalisation.
            "Fase 1 webhook GEBOUWD + live-bewezen op dev (2026-07-04)",
        ] {
            assert!(reads_as_report(report), "{report}");
            assert!(!report_shaped(report), "the silent half stays narrow: {report}");
        }

        // The false friends, measured: each was caught when the match was allowed
        // to scan the whole body, because THOR facts are one paragraph. The window
        // is what keeps a status word deep in prose from renaming the fact.
        let gefaseerde = "ONDERHOUDSKADER voor de oven: dagelijkse filtercontrole, wekelijkse \
             kalibratie, en verder een lange uitleg over het kader die pas veel \
             later spreekt over een geplande GEFASEERDE overgang naar het \
             nieuwe profiel.";
        let built_long_ago = "WERKWIJZE voor een nieuwe klantaanvraag: eerst de maatvoering \
             bevestigen, dan pas offerte, en houd er rekening mee dat de \
             rekenmodule zelf ooit GEBOUWD is op de oude tarieven.";
        for false_friend in [gefaseerde, built_long_ago] {
            assert!(!reads_as_report(false_friend), "{false_friend}");
        }

        // Terms measured to earn nothing are gone, and STEP is the one that also
        // collided: "STEP file" is a CAD format, not a status.
        assert!(!reads_as_report("Deliverables: STEP file standard, scan-data (STL/OBJ)"));

        // The English half. Nothing in the measured store opens like this, which
        // is exactly why it has to be pinned by a test instead: a store with one
        // Dutch author cannot tell us whether these fire.
        for english in [
            "Sprint 3 DONE (2026-07-04) - the importer now handles partial rows",
            "Migration FINISHED, old tables dropped, rollback note below",
            "v2 rollout COMPLETE - both regions serving, dashboards green",
            "Parser BUILT and benchmarked, numbers at the bottom",
            "OUTDATED - the queue moved to the new broker, kept for the why",
            "SUPERSEDED by the v3 plan, keeping this for the rejected options",
        ] {
            assert!(reads_as_report(english), "{english}");
            assert!(!report_shaped(english), "the silent half stays narrow: {english}");
        }
        // Same window rule for English: a status word deep in prose is not a title.
        assert!(!reads_as_report(
            "RUNBOOK for the nightly job: check the lock file, then the queue depth, \
             and only page someone when both look wrong - the import is usually just \
             not DONE yet at that hour."
        ));

        // A body that declares itself a RULE keeps the wide net off it entirely -
        // the direction that would silently retire something still governing.
        assert!(!reads_as_report("HARDE REGEL - FASE 2 blijft de enige geldende procedure"));

        // The shipped openers are untouched on both predicates.
        assert!(report_shaped("MIJLPAAL: v9 gepubliceerd"));
        assert!(reads_as_report("MIJLPAAL: v9 gepubliceerd"));
        assert!(!reads_as_report("de tweede oven draait op 230 volt"));
    }

    /// The bug this guards: a revise that rewrites the body drops the footer,
    /// which silently strips the guard's anchors and the fires-when boost. The
    /// fact stays findable, so nobody notices it stopped firing.
    #[test]
    fn carry_over_reattaches_a_dropped_footer() {
        let footer = compose(
            "decision",
            &["nas".into()],
            "global",
            &["ssh".into()],
            &["ssh admin@host".into(), "/usr/local/bin/docker".into()],
            None,
        );
        let prev = format!("old content\n\n{}", footer);

        let carried = carry_over("new content", &prev).expect("footer must be carried");
        assert_eq!(strip(&carried), "new content", "content is the caller's");
        assert_eq!(fact_type(&carried), Some(FactType::Decision), "type survives");
        assert_eq!(fires_when(&carried).as_deref(), Some("ssh"), "boost survives");
        assert_eq!(
            anchors(&carried),
            vec!["ssh admin@host", "/usr/local/bin/docker"],
            "the guard's anchors survive - the whole point"
        );
        assert!(write_defect(&carried).is_none(), "result must be a valid body");
    }

    #[test]
    fn carry_over_never_overrides_a_supplied_footer() {
        // Retyping/re-anchoring in one call must stay possible: a new body that
        // brings its own footer wins.
        let prev = format!(
            "old\n\n{}",
            compose("note", &["a".into()], "global", &[], &["old-anchor".into()], None)
        );
        let new = format!(
            "new\n\n{}",
            compose("gotcha", &["b".into()], "global", &[], &["new-anchor".into()], None)
        );
        assert_eq!(carry_over(&new, &prev), None, "caller's footer is left alone");
    }

    #[test]
    fn carry_over_is_a_noop_without_a_previous_footer() {
        assert_eq!(carry_over("new", "plain old body"), None);
    }

    #[test]
    fn compose_parse_roundtrip() {
        // The property the module exists for: whatever compose writes, every
        // parser reads back - writer and parsers can no longer drift apart.
        let footer = compose("gotcha", &["db".into(), "wal".into()], "ProjA", &[], &[], None);
        let body = format!("never open the db over SMB\n\n{}", footer);
        assert_eq!(fact_type(&body), Some(FactType::Gotcha));
        assert_eq!(project(&body).as_deref(), Some("ProjA"));
        assert!(has_project_field(&body));
        assert_eq!(fires_when(&body), None, "no triggers = no field");
        assert_eq!(strip(&body), "never open the db over SMB");
    }

    #[test]
    fn compose_full_roundtrips_provenance_and_keeps_project_last() {
        let footer = compose_full("gotcha", &["k".into()], "ProjA", &[], &[], None, Some("inferred"));
        let body = format!("the metrics port is 9090\n\n{}", footer);
        assert_eq!(provenance(&body).as_deref(), Some("inferred"));
        assert_eq!(project(&body).as_deref(), Some("ProjA"), "project stays last + parseable");
        assert_eq!(fact_type(&body), Some(FactType::Gotcha));
        assert_eq!(strip(&body), "the metrics port is 9090");
        // plain compose writes no provenance field
        let plain = format!("x\n\n{}", compose("note", &[], "g", &[], &[], None));
        assert_eq!(provenance(&plain), None);
    }

    #[test]
    fn carry_over_preserves_provenance_unless_the_new_body_overrides_it() {
        // The promotion code-trap: a content-only revise keeps the OLD footer,
        // including its provenance, so inferred->verified needs an explicit
        // re-typed footer - never a silent flip.
        let prev = format!("v1\n\n{}", compose_full("decision", &[], "P", &[], &[], None, Some("inferred")));
        let carried = carry_over("v2 corrected", &prev).expect("footerless revise carries the old footer");
        assert_eq!(provenance(&carried).as_deref(), Some("inferred"), "old provenance preserved");
        let retyped = format!("v2\n\n{}", compose_full("decision", &[], "P", &[], &[], None, Some("verified")));
        assert_eq!(carry_over(&retyped, &prev), None, "a re-typed footer wins");
    }

    #[test]
    fn compose_parse_roundtrip_with_triggers() {
        let footer = compose(
            "gotcha",
            &["deploy".into()],
            "ProjA",
            &["docker compose".into(), "deploy.flag".into()],
            &[],
            None,
        );
        let body = format!("the deploy rule\n\n{}", footer);
        assert_eq!(fires_when(&body).as_deref(), Some("docker compose deploy.flag"));
        // every other parser still reads its own field through the new one
        assert_eq!(fact_type(&body), Some(FactType::Gotcha));
        assert_eq!(project(&body).as_deref(), Some("ProjA"));
        assert_eq!(strip(&body), "the deploy rule");
        // hostile trigger content cannot corrupt the footer structure
        let hostile = compose("note", &[], "global", &["a|b\n[x]".into()], &[], None);
        assert!(!hostile.contains('\n'), "single line survives: {hostile}");
        let body2 = format!("f\n\n{}", hostile);
        assert_eq!(project(&body2).as_deref(), Some("global"));
    }

    #[test]
    fn field_safe_strips_control_chars() {
        // A multi-line footer would defeat strip() and thereby BOTH
        // near-duplicate checks - control chars must never reach the footer.
        assert_eq!(field_safe("gotcha\nweird"), "gotcha weird");
        assert_eq!(field_safe("tag\r\nwith\tcontrols"), "tag with controls");
        assert_eq!(field_safe("a[b]|c"), "abc");
    }

    #[test]
    fn compose_sanitizes_hostile_fields() {
        // A newline or bracket in a field must never produce a multi-line or
        // structurally broken footer.
        let footer = compose("gotcha\nweird", &["a|b".into(), "[x]".into()], "global", &[], &[], None);
        assert!(!footer.contains('\n'), "footer stays single-line: {footer}");
        let body = format!("fact\n\n{}", footer);
        assert_eq!(fact_type(&body), Some(FactType::Gotcha), "type survives sanitizing: {footer}");
        assert_eq!(strip(&body), "fact");
    }

    #[test]
    fn empty_type_defaults_to_note() {
        let footer = compose("", &[], "global", &[], &[], None);
        assert!(footer.starts_with("[memory/note "), "{footer}");
        assert_eq!(fact_type(&format!("x\n\n{}", footer)), None, "note is untyped by design");
    }

    #[test]
    fn expires_roundtrip_and_validation() {
        let footer = compose("note", &["pin".into()], "global", &[], &[], Some("2027-01-15"));
        let body = format!("pin serde to 1.9 until the upstream fix

{}", footer);
        assert_eq!(expires(&body).as_deref(), Some("2027-01-15"));
        // every other parser still reads through the new field
        assert_eq!(project(&body).as_deref(), Some("global"));
        assert_eq!(strip(&body), "pin serde to 1.9 until the upstream fix");
        assert_eq!(expires("no footer here"), None);
        // write-time validation: strict YYYY-MM-DD only
        for good in ["2026-01-01", "2030-12-31"] {
            assert!(valid_expiry(good), "{good}");
        }
        for bad in ["2026-1-1", "morgen", "2026-13-01", "2026-00-10", "2026-01-32", "20260101", ""] {
            assert!(!valid_expiry(bad), "{bad}");
        }
        // today() emits the same shape the validator accepts
        assert!(valid_expiry(&today()), "today() must be a valid ISO date: {}", today());
    }

    #[test]
    fn compose_parse_roundtrip_with_anchors() {
        let footer = compose(
            "gotcha",
            &[],
            "ProjA",
            &["deploy".into()],
            &["deploy/watcher.sh".into(), "docker compose up".into(), "a,b".into()],
            None,
        );
        let body = format!("the rule\n\n{}", footer);
        assert_eq!(
            anchors(&body),
            vec!["deploy/watcher.sh".to_string(), "docker compose up".to_string(), "a b".to_string()],
            "multi-word anchors survive; a comma inside an anchor is folded, never a split"
        );
        // every other parser still reads its own field through the new one
        assert_eq!(fires_when(&body).as_deref(), Some("deploy"));
        assert_eq!(project(&body).as_deref(), Some("ProjA"));
        assert_eq!(strip(&body), "the rule");
        assert!(anchors("no footer here").is_empty());
    }

    /// The dead-anchor repair case edit_footer exists for: fix ONE field of an
    /// imported fact without retyping - type, tags, project and the mimir
    /// marker (the import idempotence key) stay byte-for-byte.
    #[test]
    fn edit_footer_changes_one_field_and_leaves_the_rest_byte_for_byte() {
        let footer = "[memory/gotcha | tags: deploy nas | fires-when: scp | anchors: a b c | \
                      expires: 2027-01-15 | provenance: verified | project: P | mimir:01KEXAMPLE]";
        let edits = FieldEdits {
            anchors: Some(vec!["deploy/watcher.sh".into(), "docker compose up".into()]),
            ..Default::default()
        };
        assert_eq!(
            edit_footer(footer, &edits).unwrap(),
            "[memory/gotcha | tags: deploy nas | fires-when: scp | anchors: deploy/watcher.sh, \
             docker compose up | expires: 2027-01-15 | provenance: verified | project: P | \
             mimir:01KEXAMPLE]"
        );
    }

    #[test]
    fn edit_footer_inserts_missing_fields_at_their_canonical_position() {
        let footer = "[memory/note | tags: | project: global]";
        let edits = FieldEdits {
            triggers: Some(vec!["git push".into()]),
            anchors: Some(vec!["deploy.flag".into()]),
            ..Default::default()
        };
        // Empty tags re-emit in compose's shape ("tags: " + separator), which
        // is why the expectation carries two spaces - same bytes remember writes.
        assert_eq!(
            edit_footer(footer, &edits).unwrap(),
            "[memory/note | tags:  | fires-when: git push | anchors: deploy.flag | project: global]"
        );
    }

    #[test]
    fn edit_footer_clears_fields_and_retypes() {
        let footer = "[memory/note | tags: a b | fires-when: x | anchors: f.rs | \
                      expires: 2027-01-01 | provenance: inferred | project: P]";
        let edits = FieldEdits {
            fact_type: Some("gotcha".into()),
            tags: Some(vec![]),
            triggers: Some(vec![]),
            anchors: Some(vec![]),
            expires: Some(None),
            provenance: Some(None),
        };
        // tags stay present-but-empty (the format always writes them, same as
        // compose); every optional field is gone; project is untouched.
        assert_eq!(edit_footer(footer, &edits).unwrap(), "[memory/gotcha | tags:  | project: P]");
    }

    #[test]
    fn edit_footer_refuses_a_non_memory_line() {
        let edits = FieldEdits { tags: Some(vec![]), ..Default::default() };
        assert_eq!(edit_footer("[repo file | P/src/a.rs | chunk 1/1]", &edits), None);
        assert_eq!(edit_footer("not bracketed at all", &edits), None);
    }

    #[test]
    fn edit_footer_output_reads_back_through_every_parser() {
        let footer = compose_full(
            "decision",
            &["k".into()],
            "ProjA",
            &["ssh".into()],
            &["old.rs".into()],
            Some("2027-05-01"),
            Some("inferred"),
        );
        let edits = FieldEdits {
            anchors: Some(vec!["new.rs".into(), "cmd one".into()]),
            provenance: Some(Some("verified".into())),
            ..Default::default()
        };
        let body = format!("content\n\n{}", edit_footer(&footer, &edits).unwrap());
        assert_eq!(anchors(&body), vec!["new.rs", "cmd one"]);
        assert_eq!(provenance(&body).as_deref(), Some("verified"));
        assert_eq!(fires_when(&body).as_deref(), Some("ssh"), "untouched fields survive");
        assert_eq!(expires(&body).as_deref(), Some("2027-05-01"));
        assert_eq!(project(&body).as_deref(), Some("ProjA"));
        assert_eq!(fact_type(&body), Some(FactType::Decision));
        assert!(write_defect(&body).is_none(), "result must be a valid body");
    }

    #[test]
    fn has_source_ref_only_matches_a_real_trailing_footer() {
        // the import-synced shape
        assert!(has_source_ref("a fact\n\n[memory/note | tags: | project: global | mimir:01KFOOT]"));
        // native compose() footer: no mimir field
        assert!(!has_source_ref("a fact\n\n[memory/gotcha | tags: x | project: P]"));
        // prose that merely QUOTES the footer syntax mid-body must not count
        assert!(!has_source_ref(
            "reminder: an imported footer looks like [memory/note | project: global | mimir:01EX] - quote it exactly"
        ));
        // a quoted footer with real text after it is not a trailing footer
        assert!(!has_source_ref(
            "the line\n\n[memory/note | mimir:01EX]\nwas an example, not a footer"
        ));
        assert!(!has_source_ref("no footer at all"));
    }

    #[test]
    fn project_field_with_and_without_mimir_id() {
        // imported footers carry a trailing mimir field; native ones do not
        assert_eq!(
            project("b\n\n[memory/gotcha | tags: x | project: SomeProj | mimir:01K]").as_deref(),
            Some("SomeProj")
        );
        assert_eq!(project("b\n\n[memory/note | tags: | project: global]").as_deref(), Some("global"));
        assert_eq!(project("no footer here"), None);
    }
}
