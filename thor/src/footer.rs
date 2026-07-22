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

/// Today as YYYY-MM-DD (UTC), for the rank-time expiry compare. Civil-date
/// from days-since-epoch (Howard Hinnant's algorithm) - no chrono dependency,
/// and deliberately NOT usable from the fold modules (cas/auditor stay
/// clock-free; test_2_purity_no_time enforces that).
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
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
