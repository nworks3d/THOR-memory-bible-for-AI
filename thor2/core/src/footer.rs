//! MINIMAL PORT of thor/src/footer.rs (1.0 crate). The 1.0 file is the ONE
//! owner of the memory footer format and also carries a large amount of
//! guard/consolidate-only logic (anchor-quality heuristics, report/rule
//! detection, defect scanning and repair, field surgery, ...) that belongs to
//! layers NOT ported here.
//!
//! Storage needs exactly the footer FORMAT core, because event_store.rs's
//! append_mutate_checked calls `carry_over` in production (a content-only
//! REVISE must not silently drop the previous head's footer - type, tags,
//! fires-when, anchors), and its test suite calls `compose` / `strip` /
//! `anchors` / `fact_type` to prove that behavior. Every function below is
//! copied unchanged from the 1.0 source. Left out entirely (not needed by the
//! storage layer, and each is a sizeable, independent piece of the guard/
//! consolidate surface): unstorable_anchor, overbroad_anchor, dead_path_anchor,
//! alpha_extension, GENERIC_STEMS, BARE_TOOLS, TOOL_SUBCOMMANDS,
//! anchor_candidate, FieldEdits, edit_footer, provenance, expires, today,
//! days_from_today, report_shaped, SHIPPED_OPENERS, reads_as_report,
//! WIDE_OPENERS, opens_with, head_names, HEAD_WINDOW_CHARS, rule_shaped,
//! REPORT_EXPIRY_DAYS, valid_expiry, write_defect, Defect, defects, project,
//! has_project_field, fires_when, has_tag, has_source_ref.

use crate::repo::FactType;

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

/// Does this trailing bracketed line actually LOOK like one of THOR's footers?
/// `extract` accepts any single bracketed line after a blank line, which is the
/// right rule for finding a footer that is known to be there and the wrong one
/// for deciding whether the caller MEANT to supply one (found 2026-07-30 by an
/// adversarial verifier). A body ending in an ordinary bracketed remark - a
/// dated status line, a citation, a checkbox - looked like a footer to
/// `carry_over`, which then handed the fact "theirs wins" and dropped the real
/// footer: the type, the tags, the fires-when words and the ANCHORS, gone in
/// silence, on a fact that keeps reading perfectly well in recall. Exactly the
/// failure this module's own doc calls out, reached through the door next to it.
fn looks_like_footer(line: &str) -> bool {
    line.starts_with("[memory/") || line.starts_with("[repo file")
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
    if extract(new_body).is_some_and(looks_like_footer) {
        return None; // the caller supplied a real footer - theirs wins
    }
    let prev_footer = extract(prev_body).filter(|f| looks_like_footer(f))?;
    Some(format!("{}\n\n{}", new_body.trim_end(), prev_footer))
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
