//! Derived symbol sidecar: WHICH names does each code chunk define, and which
//! names does it call. Powers the `where_used`/`impact` MCP tools and the
//! deliberate-path symbol-definition ranking bonus.
//!
//! Design constraints, in order:
//! - DERIVED and rebuildable, stored OUTSIDE the hash-chained event log
//!   (`thor-symbols.db` next to the store, like the vectors sidecar): fsck,
//!   export, log-shipping and the auditors never see it, and deleting it only
//!   degrades ranking/tools, never data.
//! - Extraction is heuristic and dependency-free (same philosophy as the
//!   chunker): linear scans, no parser. codebase-memory-mcp's published tiers
//!   show name-level extraction without LSP is "good enough" for
//!   which-calls-what questions; resolution here is NAME-BASED and
//!   project-scoped, exactly like mimir's. Idea credit: the symbol-graph
//!   concept comes from mimir and codebase-memory-mcp (see SIMILAR-PROJECTS.md
//!   R2); this is an independent heuristic reimplementation.
//! - Extraction reads the STORE's chunk bodies, not the filesystem: the
//!   sidecar can be (re)built on any machine holding the store, and a symbol
//!   maps directly to the chunk entity that defines it - which is what the
//!   ranking bonus needs.

use crate::event_store::{Event, EventKind, EventStore};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Sidecar path next to the store (mirrors the vectors sidecar convention).
pub fn default_symbols_path(db: &Path) -> PathBuf {
    db.with_file_name("thor-symbols.db")
}

/// Keywords that look like calls (`if (`, `match (`) but never are, plus
/// declaration keywords that must not register as callee names.
const NOT_A_CALL: &[&str] = &[
    "if", "for", "while", "switch", "match", "return", "catch", "fn", "function", "def",
    "new", "await", "typeof", "sizeof", "assert", "loop", "else", "do", "try", "yield",
    "print", "println", "eprintln", "panic", "vec", "write", "writeln", "format", "matches",
    "some", "ok", "err", "none",
];

/// A name is interesting enough to store: length floor keeps single letters
/// and loop variables out of the tables.
fn keep_name(name: &str) -> bool {
    name.chars().count() >= 3 && !NOT_A_CALL.contains(&name.to_lowercase().as_str())
}

fn ident_at(s: &str) -> Option<&str> {
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
        .map_or(s.len(), |(i, _)| i);
    let id = &s[..end];
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => Some(id),
        _ => None,
    }
}

/// Symbol DEFINITIONS on one line, top-level or nested: keyword-led
/// (`fn name(`, `function name(`, `class Name`, `def name(`) and
/// declaration-assignment forms (`const name = ...`), including JS/TS
/// DESTRUCTURING (`const [linkJobModal, setLinkJobModal] = useState(...)`,
/// `const { a, b } = props`) - React state/hook names are exactly what
/// structure-shaped questions ask about.
fn defs_on_line(line: &str) -> Vec<String> {
    let mut l = line.trim_start();
    // strip common visibility/async prefixes (any order, few passes)
    loop {
        let before = l;
        for p in ["pub(crate) ", "pub(super) ", "pub ", "export ", "default ", "async ",
                  "static ", "unsafe ", "abstract ", "final "] {
            l = l.strip_prefix(p).unwrap_or(l);
        }
        if l == before {
            break;
        }
    }
    let mut out = Vec::new();
    for kw in ["fn ", "function ", "def ", "class ", "struct ", "enum ", "trait ",
               "interface ", "type ", "macro_rules! "] {
        if let Some(rest) = l.strip_prefix(kw) {
            if let Some(name) = ident_at(rest.trim_start()) {
                if keep_name(name) {
                    out.push(name.to_string());
                }
            }
            return out;
        }
    }
    for kw in ["const ", "let ", "var "] {
        let Some(rest) = l.strip_prefix(kw) else { continue };
        let rest = rest.trim_start();
        // destructuring: collect every identifier up to the closing bracket
        if let Some(open) = rest.strip_prefix('[').or_else(|| rest.strip_prefix('{')) {
            let close = open.find([']', '}']).unwrap_or(open.len());
            for part in open[..close].split(',') {
                if let Some(name) = ident_at(part.trim().trim_start_matches("...")) {
                    if keep_name(name) {
                        out.push(name.to_string());
                    }
                }
            }
            return out;
        }
        if let Some(name) = ident_at(rest) {
            // require an assignment so `const fn`-style keyword uses and bare
            // C declarations do not register
            if rest[name.len()..].contains('=') && keep_name(name) {
                out.push(name.to_string());
            }
        }
        return out;
    }
    out
}

/// Call SITES on one line: identifiers directly followed by `(`, minus
/// keywords and the definitions made on that same line. Char-indexed walk so
/// multibyte text (comments, string literals) can never split a codepoint.
fn calls_on_line(line: &str, own_defs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = line.char_indices().peekable();
    while let Some((start, c)) = it.next() {
        if !(c.is_alphabetic() || c == '_') {
            continue;
        }
        let mut end = start + c.len_utf8();
        while let Some(&(i, n)) = it.peek() {
            if n.is_alphanumeric() || n == '_' {
                it.next();
                end = i + n.len_utf8();
            } else {
                break;
            }
        }
        if it.peek().is_some_and(|&(_, n)| n == '(') {
            let name = &line[start..end];
            // method calls resolve by bare name (name-based resolution)
            if name.is_ascii() && keep_name(name) && !own_defs.iter().any(|d| d == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Per-chunk extraction: (defined names, called names), deduped.
pub fn extract_chunk(body: &str) -> (Vec<String>, Vec<String>) {
    let content = crate::footer::strip(body);
    let mut defs: Vec<String> = Vec::new();
    let mut calls: Vec<String> = Vec::new();
    let mut seen_d = HashSet::new();
    let mut seen_c = HashSet::new();
    for line in content.lines() {
        let line_defs = defs_on_line(line);
        for c in calls_on_line(line, &line_defs) {
            if seen_c.insert(c.clone()) {
                calls.push(c);
            }
        }
        for d in line_defs {
            if seen_d.insert(d.clone()) {
                defs.push(d);
            }
        }
    }
    // a name defined in this chunk is not an outgoing call of it
    calls.retain(|c| !seen_d.contains(c));
    (defs, calls)
}

pub struct SymbolStore {
    conn: Connection,
}

impl SymbolStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS symbol(
                project TEXT NOT NULL,
                name TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                PRIMARY KEY(project, name, entity_id)
            );
            CREATE TABLE IF NOT EXISTS call_edge(
                project TEXT NOT NULL,
                caller_entity TEXT NOT NULL,
                callee_name TEXT NOT NULL,
                PRIMARY KEY(project, caller_entity, callee_name)
            );
            CREATE INDEX IF NOT EXISTS idx_symbol_entity ON symbol(entity_id);
            CREATE INDEX IF NOT EXISTS idx_edge_callee ON call_edge(project, callee_name);
            ",
        )?;
        Ok(SymbolStore { conn })
    }

    pub fn open_default(db: &Path) -> rusqlite::Result<Self> {
        Self::open(&default_symbols_path(db))
    }

    /// Rebuild the tables for the projects present in the store's live chunk
    /// heads. Whole-project replace inside one transaction: a rebuild is
    /// idempotent and a crash leaves the previous consistent state.
    pub fn rebuild(&mut self, store: &EventStore) -> anyhow::Result<RebuildStats> {
        let events = store.get_all_events()?;
        let heads = crate::cas::compute_head_sets(&events);
        let by_rev: HashMap<&str, &Event> =
            events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
        let mut stats = RebuildStats::default();
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM symbol", [])?;
        tx.execute("DELETE FROM call_edge", [])?;
        for (eid, hs) in &heads {
            if !crate::repo::is_chunk_id(eid) || hs.heads.len() != 1 {
                continue;
            }
            let Some((project, rest)) = eid.split_once(':') else { continue };
            if !crate::repo::is_source_file(rest.rsplit_once('#').map_or(rest, |(r, _)| r)) {
                continue;
            }
            let rev = hs.heads.iter().next().unwrap();
            let Some(ev) = by_rev.get(rev.as_str()) else { continue };
            if matches!(ev.kind, EventKind::FactRetracted) {
                continue;
            }
            let (defs, calls) = extract_chunk(&ev.body);
            for d in defs {
                tx.execute(
                    "INSERT OR IGNORE INTO symbol(project, name, entity_id) VALUES (?, ?, ?)",
                    params![project, d, eid],
                )?;
                stats.symbols += 1;
            }
            for c in calls {
                tx.execute(
                    "INSERT OR IGNORE INTO call_edge(project, caller_entity, callee_name) VALUES (?, ?, ?)",
                    params![project, eid, c],
                )?;
                stats.edges += 1;
            }
            stats.chunks += 1;
        }
        tx.commit()?;
        Ok(stats)
    }

    /// Names defined per chunk, for the given candidate entity ids - the
    /// ranking bonus's lookup. ORIGINAL casing (the bonus needs the camelCase
    /// shape for its specificity gate); compare lowercased at the call site.
    pub fn defs_for(&self, entity_ids: &[&str]) -> HashMap<String, Vec<String>> {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        let Ok(mut stmt) =
            self.conn.prepare_cached("SELECT name FROM symbol WHERE entity_id = ?")
        else {
            return out;
        };
        for eid in entity_ids {
            let rows = stmt
                .query_map(params![eid], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect::<Vec<_>>())
                .unwrap_or_default();
            if !rows.is_empty() {
                out.insert((*eid).to_string(), rows);
            }
        }
        out
    }

    /// Chunks that CALL `name` (name-based, project-scoped; project None =
    /// all projects). Returns caller chunk entity ids.
    pub fn callers_of(&self, name: &str, project: Option<&str>) -> Vec<String> {
        let mut sql = String::from("SELECT caller_entity FROM call_edge WHERE callee_name = ?");
        if project.is_some() {
            sql.push_str(" AND project = ?");
        }
        sql.push_str(" ORDER BY caller_entity");
        let Ok(mut stmt) = self.conn.prepare(&sql) else { return Vec::new() };
        match project {
            Some(p) => stmt
                .query_map(params![name, p], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default(),
            None => stmt
                .query_map(params![name], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default(),
        }
    }

    /// Chunks DEFINING `name` (exact), project-scoped like callers_of.
    pub fn definers_of(&self, name: &str, project: Option<&str>) -> Vec<String> {
        let mut sql = String::from("SELECT entity_id FROM symbol WHERE name = ?");
        if project.is_some() {
            sql.push_str(" AND project = ?");
        }
        sql.push_str(" ORDER BY entity_id");
        let Ok(mut stmt) = self.conn.prepare(&sql) else { return Vec::new() };
        match project {
            Some(p) => stmt
                .query_map(params![name, p], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default(),
            None => stmt
                .query_map(params![name], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default(),
        }
    }

    /// Names DEFINED in a chunk (original casing) - `impact` walks these.
    pub fn defined_in(&self, entity_id: &str) -> Vec<String> {
        let Ok(mut stmt) =
            self.conn.prepare_cached("SELECT name FROM symbol WHERE entity_id = ?")
        else {
            return Vec::new();
        };
        stmt.query_map(params![entity_id], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }
}

#[derive(Default, Debug)]
pub struct RebuildStats {
    pub chunks: usize,
    pub symbols: usize,
    pub edges: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_keyword_and_declaration_defs_with_destructuring() {
        let body = "\
pub fn resolve_ref(x: u32) -> u32 { helper(x) }
const [linkJobModal, setLinkJobModal] = useState(null);
  const awaitingUpload = printers.filter(p => p.held);
function toggleSelect(id) { return dispatch(id); }
type Props = { a: number };

[repo file | P/src/a.jsx | chunk 1/1]";
        let (defs, calls) = extract_chunk(body);
        for d in ["resolve_ref", "linkJobModal", "setLinkJobModal", "awaitingUpload",
                  "toggleSelect", "Props"] {
            assert!(defs.iter().any(|x| x == d), "def {d} missing: {defs:?}");
        }
        for c in ["helper", "useState", "filter", "dispatch"] {
            assert!(calls.iter().any(|x| x == c), "call {c} missing: {calls:?}");
        }
        assert!(!calls.iter().any(|x| x == "toggleSelect"), "own def never an outgoing call");
        assert!(!defs.iter().any(|x| x == "repo"), "footer never parsed: {defs:?}");
    }

    #[test]
    fn keywords_and_short_names_never_register() {
        let (defs, calls) = extract_chunk("if (ready) { go(x); }\nfor (i = 0; i < n; i++) {}");
        assert!(defs.is_empty(), "{defs:?}");
        assert!(!calls.iter().any(|c| c == "if" || c == "for" || c == "go"), "{calls:?}");
    }

    #[test]
    fn sidecar_roundtrip_and_name_lookups() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::new(&dir.path().join("s.db")).unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "P:src/lib.rs#0", None,
                "pub fn pack_blocks(x: u32) {}\n\n[repo file | P/src/lib.rs | chunk 1/2]")
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "P:src/main.rs#0", None,
                "fn main() { pack_blocks(7); }\n\n[repo file | P/src/main.rs | chunk 1/1]")
            .unwrap();
        // a doc chunk must be ignored entirely
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "P:docs/x.md#0", None,
                "call pack_blocks() often\n\n[repo file | P/docs/x.md | chunk 1/1]")
            .unwrap();
        let mut sy = SymbolStore::open(&dir.path().join("thor-symbols.db")).unwrap();
        let stats = sy.rebuild(&store).unwrap();
        assert_eq!(stats.chunks, 2, "only source chunks scanned");
        assert_eq!(sy.definers_of("pack_blocks", Some("P")), vec!["P:src/lib.rs#0"]);
        assert_eq!(sy.callers_of("pack_blocks", Some("P")), vec!["P:src/main.rs#0"]);
        assert!(sy.callers_of("pack_blocks", Some("Q")).is_empty(), "project-scoped");
        let defs = sy.defs_for(&["P:src/lib.rs#0"]);
        assert_eq!(defs["P:src/lib.rs#0"], vec!["pack_blocks"]);
        // rebuild is idempotent (whole-replace)
        let stats2 = sy.rebuild(&store).unwrap();
        assert_eq!(stats2.chunks, 2);
        assert_eq!(sy.definers_of("pack_blocks", Some("P")).len(), 1);
    }
}
