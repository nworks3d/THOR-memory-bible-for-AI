//! Which names each indexed chunk DEFINES and which names it CALLS, so
//! "who calls this" and "what does this file define" are answerable without
//! reading every file.
//!
//! Ported from 1.0's `thor/src/symbols.rs`, which measured well enough to be
//! worth keeping, with two deliberate changes:
//!
//!  1. No sidecar. 1.0 kept a separate `thor-symbols.db` next to the store and
//!     needed a `thor symbols` command to rebuild it when the two drifted
//!     apart - a rebuild nobody remembers to run is exactly the kind of
//!     convention CONTRACT R7 forbids. Here the symbol rows live in the same
//!     database as the chunks they came from and are written in the same
//!     transaction, so they cannot lag behind the index they describe.
//!  2. No footer strip. 1.0 had to call `footer::strip` first because a stored
//!     chunk carried a prose metadata footer. 2.0's chunks are plain source
//!     text (FINDINGS-1.0.md F1 is precisely about why that footer had to go),
//!     so the extractor reads the text as it is.
//!
//! What this is NOT: a compiler, a parser, or a resolver. It matches on line
//! shapes, resolves by bare name, and knows nothing about scopes, imports or
//! overloads. Two different functions with the same name in two files are one
//! name here. That is why every answer comes back as file and line for a human
//! to read, never as a claim about what a program does.

use crate::store::{CodeIndexError, Store};
use rusqlite::{params, Connection};
use std::collections::HashSet;

/// Keywords that look like calls (`if (`, `match (`) but never are, plus
/// declaration keywords that must not register as callee names.
const NOT_A_CALL: &[&str] = &[
    "if", "for", "while", "switch", "match", "return", "catch", "fn", "function", "def",
    "new", "await", "typeof", "sizeof", "assert", "loop", "else", "do", "try", "yield",
    "print", "println", "eprintln", "panic", "vec", "write", "writeln", "format", "matches",
    "some", "ok", "err", "none",
];

/// A name is interesting enough to store: the length floor keeps single
/// letters and loop variables out of the tables.
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

/// What kind of definition a line makes. The two are stored under different
/// roles so a file's OUTLINE can show its shape - the things it declares -
/// while `where_used` still sees every binding, including local ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefShape {
    /// Keyword-led: `fn`, `class`, `struct`, `type`, `interface`, ...
    Declaration,
    /// Assignment-led: `const x = ...`, `let x = ...`, destructuring.
    Binding,
}

impl DefShape {
    fn role(self) -> &'static str {
        match self {
            DefShape::Declaration => "def",
            DefShape::Binding => "local",
        }
    }
}

/// Symbol DEFINITIONS on one line, top-level or nested: keyword-led
/// (`fn name(`, `function name(`, `class Name`, `def name(`) and
/// declaration-assignment forms (`const name = ...`), including JS/TS
/// destructuring (`const [a, setA] = useState(...)`, `const { a, b } = props`).
fn defs_on_line(line: &str) -> Vec<(String, DefShape)> {
    // A const at the left margin is part of the file's shape; a let inside a
    // function is not. Indentation is the only signal available without a
    // parser, and it is a good one in every language that indents bodies -
    // which is all of the ones here. A file written without indentation loses
    // the distinction and gets everything in its outline, which is the safe
    // way round.
    let top_level = !line.starts_with([' ', '\t']);
    let assignment_shape =
        if top_level { DefShape::Declaration } else { DefShape::Binding };
    let mut l = line.trim_start();
    // strip common visibility/async prefixes (any order, few passes)
    loop {
        let before = l;
        for p in [
            "pub(crate) ", "pub(super) ", "pub ", "export ", "default ", "async ", "static ",
            "unsafe ", "abstract ", "final ",
        ] {
            l = l.strip_prefix(p).unwrap_or(l);
        }
        if l == before {
            break;
        }
    }
    let mut out = Vec::new();
    for kw in [
        "fn ", "function ", "def ", "class ", "struct ", "enum ", "trait ", "interface ",
        "type ", "macro_rules! ",
    ] {
        if let Some(rest) = l.strip_prefix(kw) {
            if let Some(name) = ident_at(rest.trim_start()) {
                if keep_name(name) {
                    out.push((name.to_string(), DefShape::Declaration));
                }
            }
            return out;
        }
    }
    for kw in ["const ", "let ", "var "] {
        let Some(rest) = l.strip_prefix(kw) else { continue };
        // `let mut f = ...` binds f, not "mut". 1.0 recorded the keyword as a
        // name, so every Rust file's outline was full of "mut".
        let rest = rest.trim_start().strip_prefix("mut ").unwrap_or(rest.trim_start()).trim_start();
        // destructuring: collect every identifier up to the closing bracket
        if let Some(open) = rest.strip_prefix('[').or_else(|| rest.strip_prefix('{')) {
            let close = open.find([']', '}']).unwrap_or(open.len());
            for part in open[..close].split(',') {
                if let Some(name) = ident_at(part.trim().trim_start_matches("...")) {
                    if keep_name(name) {
                        out.push((name.to_string(), assignment_shape));
                    }
                }
            }
            return out;
        }
        if let Some(name) = ident_at(rest) {
            // require an assignment so `const fn`-style keyword uses and bare
            // C declarations do not register
            if rest[name.len()..].contains('=') && keep_name(name) {
                out.push((name.to_string(), assignment_shape));
            }
        }
        return out;
    }
    out
}

/// Call SITES on one line: identifiers directly followed by `(`, minus
/// keywords and the definitions made on that same line. Char-indexed walk so
/// multibyte text (comments, string literals) can never split a codepoint.
fn calls_on_line(line: &str, own_defs: &[(String, DefShape)]) -> Vec<String> {
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
            if name.is_ascii() && keep_name(name) && !own_defs.iter().any(|(d, _)| d == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// One chunk's names, with the line each was seen on. `(defs, refs)`, deduped
/// per LINE - the same name twice on one line is one site, the same name on
/// two lines is two.
///
/// 1.0 deduped per CHUNK instead, keeping only the first line a name appeared
/// on. Measured on this repository that turned 8 real call sites of one
/// function into 4 reported ones, because several sat in the same chunk. For a
/// "what breaks if I change this" answer an undercount is the worst possible
/// error: it reads as a complete list and is not one. Every occurrence is
/// recorded here, and the caller's limit does the trimming visibly.
pub fn extract_chunk(
    text: &str,
    first_line: i64,
) -> (Vec<(String, DefShape, i64)>, Vec<(String, i64)>) {
    let mut defs: Vec<(String, DefShape, i64)> = Vec::new();
    let mut calls: Vec<(String, i64)> = Vec::new();
    let mut defined_here: HashSet<String> = HashSet::new();
    let mut seen_d: HashSet<(String, i64)> = HashSet::new();
    let mut seen_c: HashSet<(String, i64)> = HashSet::new();
    for (offset, line) in text.lines().enumerate() {
        let line_no = first_line + offset as i64;
        let line_defs = defs_on_line(line);
        for c in calls_on_line(line, &line_defs) {
            if seen_c.insert((c.clone(), line_no)) {
                calls.push((c, line_no));
            }
        }
        for (d, shape) in line_defs {
            defined_here.insert(d.clone());
            if seen_d.insert((d.clone(), line_no)) {
                defs.push((d, shape, line_no));
            }
        }
    }
    // a name defined in this chunk is not an outgoing call of it
    calls.retain(|(c, _)| !defined_here.contains(c));
    (defs, calls)
}

/// Create the symbol table. Called from the index's own `init`, so a database
/// that has chunks always has somewhere to put their symbols.
pub(crate) fn init(conn: &Connection) -> Result<(), CodeIndexError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS symbol (
            id INTEGER PRIMARY KEY,
            chunk_id INTEGER NOT NULL REFERENCES chunk(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            role TEXT NOT NULL CHECK (role IN ('def', 'local', 'ref')),
            line INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_symbol_name ON symbol(name, role);
        CREATE INDEX IF NOT EXISTS idx_symbol_chunk ON symbol(chunk_id);
        ",
    )?;
    Ok(())
}

/// Record one chunk's symbols. Called inside the same transaction that wrote
/// the chunk, so the two can never disagree - there is no separate rebuild
/// step to forget.
pub(crate) fn record_chunk(
    tx: &rusqlite::Transaction,
    chunk_id: i64,
    text: &str,
    start_line: i64,
) -> Result<usize, CodeIndexError> {
    let (defs, refs) = extract_chunk(text, start_line);
    let mut stmt =
        tx.prepare_cached("INSERT INTO symbol (chunk_id, name, role, line) VALUES (?, ?, ?, ?)")?;
    for (name, shape, line) in &defs {
        stmt.execute(params![chunk_id, name, shape.role(), line])?;
    }
    for (name, line) in &refs {
        stmt.execute(params![chunk_id, name, "ref", line])?;
    }
    Ok(defs.len() + refs.len())
}

/// One place a name was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub path: String,
    pub line: i64,
}

/// Everything the index knows about one name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub name: String,
    pub defined_at: Vec<Site>,
    pub referenced_at: Vec<Site>,
}

/// Sites for one name under any of `roles`. Roles are internal constants, never
/// caller input, so they are inlined into the SQL rather than bound - there is
/// no user string anywhere near this statement.
fn sites(
    store: &Store,
    name: &str,
    roles: &[&str],
    limit: usize,
) -> Result<Vec<Site>, CodeIndexError> {
    let list = roles.iter().map(|r| format!("'{r}'")).collect::<Vec<_>>().join(", ");
    let mut stmt = store.connection().prepare(&format!(
        "SELECT f.path, s.line
         FROM symbol s JOIN chunk c ON c.id = s.chunk_id JOIN file f ON f.id = c.file_id
         WHERE s.name = ?1 AND s.role IN ({list})
         ORDER BY f.path, s.line
         LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![name, limit as i64], |r| {
            Ok(Site { path: r.get(0)?, line: r.get(1)? })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Where a name is defined and where it is used. Answers both of 1.0's
/// `where_used` and `impact` questions with one query, because they were the
/// same query with two framings - what breaks if you change X is exactly the
/// list of places that mention X.
///
/// Resolution is by bare name and nothing else (see the module docs): two
/// unrelated functions sharing a name come back together, so every site is
/// returned as a file and a line to open, never as a conclusion.
pub fn where_used(store: &Store, name: &str, limit: usize) -> Result<Usage, CodeIndexError> {
    let limit = limit.max(1);
    Ok(Usage {
        name: name.to_string(),
        // Both shapes count as "where this name comes from": a local binding
        // that shadows a function is exactly the kind of thing you want to see
        // before you change either.
        defined_at: sites(store, name, &["def", "local"], limit)?,
        referenced_at: sites(store, name, &["ref"], limit)?,
    })
}

/// Every name one file DECLARES, in line order - the file's shape without
/// reading it. Local bindings are deliberately left out: a Rust file's outline
/// full of `out`, `now` and `clean` is not a shape, it is noise. They are
/// still recorded and still reachable through `where_used`.
///
/// An unknown or unindexed path comes back empty, which the caller must report
/// as "not indexed", never as "defines nothing".
pub fn outline(store: &Store, path: &str) -> Result<Vec<(String, i64)>, CodeIndexError> {
    let mut stmt = store.connection().prepare(
        "SELECT s.name, s.line
         FROM symbol s JOIN chunk c ON c.id = s.chunk_id JOIN file f ON f.id = c.file_id
         WHERE f.path = ?1 AND s.role = 'def'
         ORDER BY s.line",
    )?;
    let rows = stmt
        .query_map(params![path], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// True iff this index has a given path at all - so "no symbols" and "never
/// indexed" are answerable as different things.
pub fn is_indexed(store: &Store, path: &str) -> Result<bool, CodeIndexError> {
    let n: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM file WHERE path = ?1",
        params![path],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rust_definition_and_its_call_are_told_apart() {
        let text = "pub fn resolve_ref(x: u32) -> u32 {\n    helper(x)\n}\n";
        let (defs, refs) = extract_chunk(text, 10);
        assert_eq!(defs, vec![("resolve_ref".to_string(), DefShape::Declaration, 10)]);
        assert_eq!(refs, vec![("helper".to_string(), 11)]);
    }

    /// The defect this guards against: reporting a function as calling itself
    /// because its own definition line mentions its name.
    #[test]
    fn a_definition_is_never_also_a_call_of_itself() {
        let text = "function render(props) {\n  return render_inner(props);\n}\n";
        let (defs, refs) = extract_chunk(text, 1);
        assert!(defs.iter().any(|(n, _, _)| n == "render"));
        assert!(!refs.iter().any(|(n, _)| n == "render"), "got {refs:?}");
    }

    #[test]
    fn js_destructuring_registers_every_name_it_binds() {
        let text = "const [linkJobModal, setLinkJobModal] = useState(null);\n";
        let (defs, _) = extract_chunk(text, 1);
        let names: Vec<&str> = defs.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"linkJobModal"), "got {names:?}");
        assert!(names.contains(&"setLinkJobModal"), "got {names:?}");
    }

    /// The defect this guards against: `if (`, `while (` and friends counted
    /// as calls, which would make every control-flow keyword the most-called
    /// symbol in the whole index.
    #[test]
    fn control_flow_keywords_are_never_calls() {
        let text = "if (ready) {\n  while (more()) {}\n}\n";
        let (_, refs) = extract_chunk(text, 1);
        let names: Vec<&str> = refs.iter().map(|(n, _)| n.as_str()).collect();
        assert!(!names.contains(&"if"), "got {names:?}");
        assert!(!names.contains(&"while"), "got {names:?}");
        assert!(names.contains(&"more"), "a real call must still register: {names:?}");
    }

    /// Multibyte text in a comment or a string must never split a codepoint.
    /// The defect this guards against, and it is the one that matters most
    /// here: an UNDERCOUNT. 1.0 kept only the first line a name appeared on
    /// per chunk, so a function called three times in one chunk was reported
    /// as called once - a list that looks complete and is not.
    #[test]
    fn every_call_site_is_recorded_not_just_the_first_in_a_chunk() {
        let text = "let a = helper(1);\nlet b = helper(2);\nlet c = helper(3);\n";
        let (_, refs) = extract_chunk(text, 100);
        let lines: Vec<i64> = refs.iter().filter(|(n, _)| n == "helper").map(|(_, l)| *l).collect();
        assert_eq!(lines, vec![100, 101, 102], "all three sites, not just the first");
    }

    /// The two defects this guards against, both seen on a real file: `let mut
    /// f = ...` recorded as a symbol called "mut", and a module-level `const`
    /// missing from the file's shape while every local `let` was in it.
    #[test]
    fn a_top_level_const_is_shape_and_an_indented_let_is_not() {
        let text = "const DEBOUNCE_HOURS: u64 = 20;\nfn go() {\n    let mut file = open();\n}\n";
        let (defs, _) = extract_chunk(text, 1);
        let by_name = |n: &str| defs.iter().find(|(name, _, _)| name == n).map(|(_, s, _)| *s);
        assert_eq!(by_name("DEBOUNCE_HOURS"), Some(DefShape::Declaration));
        assert_eq!(by_name("file"), Some(DefShape::Binding));
        assert_eq!(by_name("mut"), None, "the mut keyword is not a name");
    }

    #[test]
    fn the_same_name_twice_on_one_line_is_one_site() {
        let text = "let x = helper(helper(1));\n";
        let (_, refs) = extract_chunk(text, 1);
        let n = refs.iter().filter(|(name, _)| name == "helper").count();
        assert_eq!(n, 1, "a line is the unit of a site, so one line is one row");
    }

    #[test]
    fn a_line_with_multibyte_text_does_not_panic() {
        let text = "// stuurt op 30 graden, meet in Ohm\nlet total = compute(waarde);\n";
        let (_, refs) = extract_chunk(text, 1);
        assert!(refs.iter().any(|(n, _)| n == "compute"), "got {refs:?}");
    }
}
