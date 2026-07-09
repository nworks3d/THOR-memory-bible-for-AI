//! The ONE owner of the memory footer format:
//! `[memory/<type> | tags: <t1 t2> | project: <key|global> | mimir:<id>]`
//! (the mimir-compatible convention; the trailing mimir field only appears on
//! imported facts). Composing at write time and parsing at read time used to
//! live in four call sites that shared the format by convention only - the MCP
//! writer, the type classifier, the dedup/snippet stripper, and the backfill
//! project parser. A format drift would break them silently and asymmetrically
//! (facts written by one side, unreadable by another), so BOTH sides live here
//! and the old call sites keep thin shims.

use crate::repo::FactType;

/// Compose the footer for a fact written at type-aware write time (MCP
/// remember). Fields are sanitized here so a caller can never corrupt the
/// format: see field_safe. `project_label` is a project key or "global".
pub fn compose(fact_type: &str, tags: &[String], project_label: &str) -> String {
    let ty = {
        let t = field_safe(fact_type).to_lowercase();
        if t.is_empty() { "note".to_string() } else { t }
    };
    let tags = tags
        .iter()
        .map(|t| field_safe(t))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    format!("[memory/{} | tags: {} | project: {}]", ty, tags, project_label)
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
mod tests {
    use super::*;

    #[test]
    fn compose_parse_roundtrip() {
        // The property the module exists for: whatever compose writes, every
        // parser reads back - writer and parsers can no longer drift apart.
        let footer = compose("gotcha", &["db".into(), "wal".into()], "ProjA");
        let body = format!("never open the db over SMB\n\n{}", footer);
        assert_eq!(fact_type(&body), Some(FactType::Gotcha));
        assert_eq!(project(&body).as_deref(), Some("ProjA"));
        assert!(has_project_field(&body));
        assert_eq!(strip(&body), "never open the db over SMB");
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
        let footer = compose("gotcha\nweird", &["a|b".into(), "[x]".into()], "global");
        assert!(!footer.contains('\n'), "footer stays single-line: {footer}");
        let body = format!("fact\n\n{}", footer);
        assert_eq!(fact_type(&body), Some(FactType::Gotcha), "type survives sanitizing: {footer}");
        assert_eq!(strip(&body), "fact");
    }

    #[test]
    fn empty_type_defaults_to_note() {
        let footer = compose("", &[], "global");
        assert!(footer.starts_with("[memory/note "), "{footer}");
        assert_eq!(fact_type(&format!("x\n\n{}", footer)), None, "note is untyped by design");
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
