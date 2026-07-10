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
) -> String {
    let ty = {
        let t = field_safe(fact_type).to_lowercase();
        if t.is_empty() { "note".to_string() } else { t }
    };
    let clean = |xs: &[String]| -> Vec<String> {
        xs.iter().map(|t| field_safe(t)).filter(|t| !t.is_empty()).collect()
    };
    let tags = clean(tags).join(" ");
    let fires = clean(triggers).join(" ");
    // an anchor may contain spaces (a command phrase), so entries are
    // comma-separated; commas inside an anchor would split it - strip them
    let anchors = clean(anchors)
        .into_iter()
        .map(|a| a.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = format!("[memory/{} | tags: {}", ty, tags);
    if !fires.is_empty() {
        out.push_str(&format!(" | fires-when: {}", fires));
    }
    if !anchors.is_empty() {
        out.push_str(&format!(" | anchors: {}", anchors));
    }
    out.push_str(&format!(" | project: {}]", project_label));
    out
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

/// Parse the footer's `| fires-when: <words> |` field: the author-declared
/// firing vocabulary that recall's trigger bonus reads. None when absent.
pub fn fires_when(body: &str) -> Option<String> {
    let idx = body.find("| fires-when: ")?;
    let rest = &body[idx + "| fires-when: ".len()..];
    let words = rest.split(" |").next()?.trim().trim_end_matches(']').trim();
    (!words.is_empty()).then(|| words.to_string())
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
mod tests {
    use super::*;

    #[test]
    fn compose_parse_roundtrip() {
        // The property the module exists for: whatever compose writes, every
        // parser reads back - writer and parsers can no longer drift apart.
        let footer = compose("gotcha", &["db".into(), "wal".into()], "ProjA", &[], &[]);
        let body = format!("never open the db over SMB\n\n{}", footer);
        assert_eq!(fact_type(&body), Some(FactType::Gotcha));
        assert_eq!(project(&body).as_deref(), Some("ProjA"));
        assert!(has_project_field(&body));
        assert_eq!(fires_when(&body), None, "no triggers = no field");
        assert_eq!(strip(&body), "never open the db over SMB");
    }

    #[test]
    fn compose_parse_roundtrip_with_triggers() {
        let footer = compose(
            "gotcha",
            &["deploy".into()],
            "ProjA",
            &["docker compose".into(), "deploy.flag".into()],
            &[],
        );
        let body = format!("the deploy rule\n\n{}", footer);
        assert_eq!(fires_when(&body).as_deref(), Some("docker compose deploy.flag"));
        // every other parser still reads its own field through the new one
        assert_eq!(fact_type(&body), Some(FactType::Gotcha));
        assert_eq!(project(&body).as_deref(), Some("ProjA"));
        assert_eq!(strip(&body), "the deploy rule");
        // hostile trigger content cannot corrupt the footer structure
        let hostile = compose("note", &[], "global", &["a|b\n[x]".into()], &[]);
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
        let footer = compose("gotcha\nweird", &["a|b".into(), "[x]".into()], "global", &[], &[]);
        assert!(!footer.contains('\n'), "footer stays single-line: {footer}");
        let body = format!("fact\n\n{}", footer);
        assert_eq!(fact_type(&body), Some(FactType::Gotcha), "type survives sanitizing: {footer}");
        assert_eq!(strip(&body), "fact");
    }

    #[test]
    fn empty_type_defaults_to_note() {
        let footer = compose("", &[], "global", &[], &[]);
        assert!(footer.starts_with("[memory/note "), "{footer}");
        assert_eq!(fact_type(&format!("x\n\n{}", footer)), None, "note is untyped by design");
    }

    #[test]
    fn compose_parse_roundtrip_with_anchors() {
        let footer = compose(
            "gotcha",
            &[],
            "ProjA",
            &["deploy".into()],
            &["deploy/watcher.sh".into(), "docker compose up".into(), "a,b".into()],
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
