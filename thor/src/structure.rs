//! Structure Cards: a composed PROSE serving form for structure-shaped queries.
//!
//! The measured problem (BENCHMARKS.md, 2026-07-21): on "which functions call X"
//! questions THOR retrieves the right file MORE often than the rival (81% vs 69%
//! where the gold names a file) and still loses the judged category (57.6% vs
//! 74.2%), because it serves raw code where the rival serves a document that
//! states the answer in words. Nothing in THOR ever SAID structure; even
//! `where_used` answered with bare chunk-id lists the reader had to chase.
//!
//! A structure card states it: where the symbol is defined, its signature line,
//! who calls it grouped per file, and the stored memory about that symbol - all
//! derived, all verifiable, woven from the ONE store that holds the repo, the
//! symbol sidecar and the hand-written memories together. That composition is
//! the point: a rival whose code graph and recall are separate tools cannot
//! serve it in one answer.
//!
//! Not a ranking change: the card is ADDITIVE (ranked hits follow unchanged
//! below it) and fires only when both gates hold - the query carries structure
//! vocabulary AND names a symbol the sidecar actually resolves. A false fire
//! costs a few lines of accurate derived text. No LLM anywhere: the card is a
//! template over sidecar lookups, so it can never fabricate.

use crate::event_store::EventStore;
use crate::symbols::SymbolStore;
use std::collections::BTreeMap;

/// Structure vocabulary (EN + NL), matched on word boundaries. Deliberately the
/// vocabulary of ASKING ABOUT shape - "calls", "callers", "defined", "impact" -
/// not generic code words: the second gate (sidecar resolution) does the real
/// filtering, this one only keeps the card off plain knowledge questions.
const STRUCTURE_WORDS: &[&str] = &[
    // EN
    "call", "calls", "called", "caller", "callers", "uses", "used", "usage", "define",
    "defines", "defined", "definition", "declared", "declaration", "implemented",
    "implements", "impact", "structure", "where", "blast", "radius",
    // NL
    "aanroept", "aangeroepen", "roept", "gebruikt", "definieert", "gedefinieerd",
    "geimplementeerd", "waar", "structuur",
];

/// Words never tried as a symbol, however identifier-shaped the tokenizer finds
/// them: the structure vocabulary itself plus question glue.
const NOT_A_SYMBOL: &[&str] = &[
    "the", "this", "that", "what", "which", "who", "how", "does", "are", "is", "in", "of",
    "for", "from", "and", "function", "functions", "method", "methods", "symbol", "file",
    "code", "wat", "wie", "hoe", "welke", "functie", "functies", "bestand", "waarom",
];

/// The query names structure AND a symbol the sidecar resolves: return that
/// symbol. Both gates are required - vocabulary alone fires on prose questions,
/// resolution alone fires on every code question.
pub fn detect(query: &str, symbols: &SymbolStore, project: Option<&str>) -> Option<String> {
    let lower = query.to_lowercase();
    let words: Vec<&str> =
        lower.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| !w.is_empty()).collect();
    if !words.iter().any(|w| STRUCTURE_WORDS.contains(w)) {
        return None;
    }
    // Candidate symbols from the ORIGINAL query (casing matters to the sidecar):
    // identifier-shaped tokens first, then any remaining word, capped so a long
    // prompt cannot turn detection into a table scan.
    let mut cands: Vec<String> = Vec::new();
    for raw in query.split_whitespace() {
        let t = raw.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        // `a::b` / `a.b()` forms: try the last path segment too.
        for part in [t, t.rsplit("::").next().unwrap_or(t), t.rsplit('.').next().unwrap_or(t)] {
            let p = part.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
            if p.chars().count() >= 3
                && !NOT_A_SYMBOL.contains(&p.to_lowercase().as_str())
                && !STRUCTURE_WORDS.contains(&p.to_lowercase().as_str())
                && !cands.iter().any(|c| c == p)
            {
                cands.push(p.to_string());
            }
        }
    }
    let ident_shaped = |s: &str| {
        s.contains('_') || s.chars().any(|c| c.is_uppercase()) && s.chars().any(|c| c.is_lowercase())
    };
    cands.sort_by_key(|c| !ident_shaped(c)); // identifier-shaped first, stable
    // Resolution alone is not enough: `let mesh = null;` makes "mesh" a defined
    // symbol, and a card about a local binding with zero callers answers the
    // wrong question (measured: 24 cards on the 200-question battery, 27/27
    // judged ties - the weak-symbol cards added nothing). A card is warranted
    // when the word LOOKS like an identifier, or when something actually CALLS
    // it - either one is evidence the question is about a real symbol.
    cands.into_iter().take(12).find(|c| {
        let has_callers = !symbols.callers_of(c, project).is_empty();
        let defined = !symbols.definers_of(c, project).is_empty();
        (ident_shaped(c) && (defined || has_callers)) || has_callers
    })
}

/// The relative file path inside a chunk entity id (`project:rel/path#idx`).
fn chunk_file(entity: &str) -> &str {
    let no_idx = entity.rsplit_once('#').map_or(entity, |(f, _)| f);
    no_idx.split_once(':').map_or(no_idx, |(_, rel)| rel)
}

/// The definition's own line from a defining chunk's body: the first line that
/// carries the symbol next to a declaration keyword. Derived text, or nothing.
fn signature_line(body: &str, symbol: &str) -> Option<String> {
    crate::footer::strip(body)
        .lines()
        .map(str::trim)
        .find(|l| {
            l.contains(symbol)
                && ["fn ", "function ", "def ", "class ", "struct ", "enum ", "trait ",
                    "interface ", "const ", "let ", "var ", "type ", "macro_rules!"]
                    .iter()
                    .any(|k| l.contains(k))
        })
        .map(|l| {
            let mut s: String = l.chars().take(110).collect();
            if l.chars().count() > 110 {
                s.push_str("...");
            }
            s
        })
}

/// Compose the card. None when the sidecar knows nothing about the symbol -
/// the caller then serves exactly what it always served.
pub fn card(
    store: &EventStore,
    symbols: &SymbolStore,
    symbol: &str,
    project: Option<&str>,
) -> Option<String> {
    let defs = symbols.definers_of(symbol, project);
    let callers = symbols.callers_of(symbol, project);
    if defs.is_empty() && callers.is_empty() {
        return None;
    }
    let mut out = format!("[structure] `{symbol}`");

    // Defined where, with the definition's own line when it can be found.
    if defs.is_empty() {
        out.push_str(" - no definition in the sidecar (external or renamed)");
    } else {
        let files: Vec<&str> = {
            let mut f: Vec<&str> = defs.iter().map(|d| chunk_file(d)).collect();
            f.dedup();
            f
        };
        out.push_str(&format!(" - defined in {}", files.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
        if files.len() > 3 {
            out.push_str(&format!(" (+{} more)", files.len() - 3));
        }
        let events = store.get_all_events().ok();
        if let Some(events) = &events {
            if let Some(sig) = defs.iter().find_map(|d| {
                let body = &events.iter().rev().find(|e| &e.entity_id == d)?.body;
                signature_line(body, symbol)
            }) {
                out.push_str(&format!("\n  signature: {sig}"));
            }
        }
    }

    // Callers grouped per file: the sentence the category's questions ask for.
    if callers.is_empty() {
        out.push_str("\n  called from: nothing in the sidecar (entry point, test-only, or dynamic)");
    } else {
        let mut by_file: BTreeMap<&str, usize> = BTreeMap::new();
        for c in &callers {
            *by_file.entry(chunk_file(c)).or_insert(0) += 1;
        }
        let mut parts: Vec<String> = by_file
            .iter()
            .map(|(f, n)| if *n > 1 { format!("{f} ({n})") } else { (*f).to_string() })
            .collect();
        let extra = parts.len().saturating_sub(8);
        parts.truncate(8);
        out.push_str(&format!(
            "\n  called from {} chunk(s) across {} file(s): {}",
            callers.len(),
            by_file.len(),
            parts.join(", ")
        ));
        if extra > 0 {
            out.push_str(&format!(" (+{extra} more files)"));
        }
    }

    // The stored memory about this symbol: the composition no split-tool rival
    // can serve. Live, hand-written heads only; the newest two.
    if let Ok(events) = store.get_all_events() {
        let heads = crate::cas::compute_head_sets(&events);
        let by_rev: std::collections::HashMap<&str, &crate::event_store::Event> =
            events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
        let sym_lower = symbol.to_lowercase();
        let mut related: Vec<(&i64, String)> = Vec::new();
        for (entity, hs) in &heads {
            if crate::repo::is_chunk_id(entity) {
                continue;
            }
            for rev in &hs.heads {
                let Some(head) = by_rev.get(rev.as_str()) else { continue };
                if matches!(head.kind, crate::event_store::EventKind::FactRetracted) {
                    continue;
                }
                if head.body.to_lowercase().contains(&sym_lower) {
                    let ty = crate::footer::fact_type(&head.body)
                        .map(|t| format!("[{}] ", t.as_str()))
                        .unwrap_or_default();
                    let first: String =
                        head.body.lines().next().unwrap_or("").chars().take(90).collect();
                    related.push((&head.seq, format!("{ty}{entity}: {first}")));
                    break;
                }
            }
        }
        related.sort_by(|a, b| b.0.cmp(a.0)); // newest first
        for (_, line) in related.into_iter().take(2) {
            out.push_str(&format!("\n  related memory: {line}"));
        }
    }

    out.push_str(
        "\n  (derived from the symbol sidecar; name-based, not type-resolved - \
         full body: get <chunk id>, blast radius: impact)",
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::{EventKind, EventStore};

    fn seeded() -> (tempfile::TempDir, EventStore, SymbolStore) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        for (eid, body) in [
            ("Proj:src/a.rs#0", "pub fn alpha_thing() {\n    helper();\n}"),
            ("Proj:src/b.rs#0", "fn beta() {\n    alpha_thing();\n}"),
            ("Proj:src/c.rs#0", "fn gamma() {\n    alpha_thing();\n    alpha_thing();\n}"),
            ("Proj:mem-1", "gotcha: alpha_thing must never run twice per session\n\n[memory/gotcha | project: Proj]"),
        ] {
            store.append_event("s", "l", "a", EventKind::FactCreated, eid, None, body).unwrap();
        }
        let mut sy = SymbolStore::open(&dir.path().join("t-symbols.db")).unwrap();
        sy.rebuild(&store).unwrap();
        (dir, store, sy)
    }

    #[test]
    fn detect_needs_both_gates() {
        let (_d, _store, sy) = seeded();
        // vocabulary without a resolving symbol: silent
        assert_eq!(detect("who calls the frontend renderer", &sy, Some("Proj")), None);
        // resolving symbol without structure vocabulary: silent
        assert_eq!(detect("fix the bug in alpha_thing please", &sy, Some("Proj")), None);
        // both: fires, and returns the symbol
        assert_eq!(
            detect("which functions call alpha_thing?", &sy, Some("Proj")),
            Some("alpha_thing".to_string())
        );
    }

    #[test]
    fn detect_refuses_a_weak_symbol_with_no_callers() {
        // `let mesh = null;` defines "mesh" in the sidecar's eyes, but a card
        // about a callerless local binding answers the wrong question - the
        // measured failure of the first battery run. Not identifier-shaped and
        // nothing calls it: no card.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        store
            .append_event(
                "s", "l", "a", EventKind::FactCreated, "Proj:src/view.js#0", None,
                "let mesh = null;\nfunction draw() { paint(); }",
            )
            .unwrap();
        let mut sy = SymbolStore::open(&dir.path().join("t-symbols.db")).unwrap();
        sy.rebuild(&store).unwrap();
        assert_eq!(detect("where is the mesh volume calculated", &sy, Some("Proj")), None);
    }

    #[test]
    fn card_states_structure_in_words_and_weaves_the_memory_in() {
        let (_d, store, sy) = seeded();
        let card = card(&store, &sy, "alpha_thing", Some("Proj")).expect("sidecar resolves it");
        assert!(card.contains("defined in src/a.rs"), "definition file stated: {card}");
        assert!(card.contains("signature: pub fn alpha_thing()"), "signature line: {card}");
        assert!(
            card.contains("called from 2 chunk(s) across 2 file(s)"),
            "caller count stated in words: {card}"
        );
        assert!(card.contains("src/b.rs") && card.contains("src/c.rs"), "files named: {card}");
        assert!(
            card.contains("related memory") && card.contains("never run twice"),
            "the stored gotcha rides along: {card}"
        );
        assert!(card.contains("name-based"), "the resolution caveat travels with it: {card}");
    }

    #[test]
    fn card_declines_on_unknown_symbols() {
        let (_d, store, sy) = seeded();
        assert_eq!(card(&store, &sy, "no_such_symbol", Some("Proj")), None);
    }
}
