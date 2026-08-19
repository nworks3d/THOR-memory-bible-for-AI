//! Surface 4: OPZOEKEN (explicit lookup). NOT an injection channel: this is
//! never called on a session start, a moment of action, or a prompt - only
//! on an explicit search/key request. Two consequences follow directly from
//! that, per CONTRACT.md:
//!
//! - **No project scope-default.** Every live item, from every project,
//!   participates - unlike `session_start`, which deliberately DOES scope by
//!   project. There is no "current project" concept here at all.
//! - **Archive kinds participate.** `Report` and `Chunk` are "archive exactly
//!   like Report" (see `model::item::Kind`'s own doc comment: "found only by
//!   searching, never served at a gate" / "fully searchable") - `search`
//!   below does NOT filter on `Kind::can_fire()` the way every injection
//!   surface does; that gate belongs to the moment/prompt/session-start
//!   doors, not this one.
//!
//! `Lookup` is the one kind excluded from `search`: per its own doc comment
//! it "is served only on an explicit request for its key" - a different
//! door, `by_key` below, never found by scanning text.

use crate::live::live_items;
use model::item::{Item, Kind};
#[cfg(feature = "semantic")]
use std::collections::{HashMap, HashSet};
use std::path::Path;
use thor_core::event_store::EventStore;

pub struct LookupHit {
    pub id: String,
    pub item: Item,
}

/// Every live item whose kind is NOT `Lookup`, from any project, whose text
/// or tags contain `query` (case-insensitive substring) - archive kinds
/// (`Report`, `Chunk`) included on purpose. Sorted by id for a stable,
/// diffable result, same convention as `audit::audit_rows`.
pub fn search(store: &EventStore, query: &str) -> Vec<LookupHit> {
    search_with_expired(store, query).0
}

/// Today, as the `YYYY-MM-DD` an `expires` value is compared against.
fn today() -> String {
    crate::time::now_iso8601().chars().take(10).collect()
}

/// Has this item's own declared expiry passed? A malformed date expires
/// nothing - guessing what a bad date meant is worse than ignoring it.
fn is_expired(item: &Item, today: &str) -> bool {
    match item.expires.as_deref() {
        Some(date) if date.len() == 10 => date < today,
        _ => false,
    }
}

/// `search`, plus how many matches its own expiry rule held back.
///
/// THE DEFECT THIS CLOSES, found by auditing the migration (2026-08-03).
/// `expires` was written, validated at the write gate, and read by NOTHING on
/// any read path - the same stored-and-ignored shape as the project field.
/// The live store holds 169 items carrying an expiry, 77 of them already
/// past it, so a search returned settled and superseded answers ranked
/// exactly like current ones. "Old facts coming back" is one of the two
/// complaints this whole rebuild exists to answer, and the field meant to
/// prevent it was inert.
///
/// Held back, never deleted and never hidden: `get` still shows an expired
/// item whole, `history` still walks it, and the count comes back with every
/// search so a caller can say how many were withheld. An expiry that removed
/// things silently would only be a different silent failure.
pub fn search_with_expired(store: &EventStore, query: &str) -> (Vec<LookupHit>, usize) {
    let q = query.to_lowercase();
    let today = today();
    let words: Vec<String> = q.split_whitespace().map(str::to_string).collect();

    let candidates: Vec<LookupHit> = live_items(store)
        .into_iter()
        .filter(|li| li.item.kind != Kind::Lookup)
        .map(|li| LookupHit { id: li.id, item: li.item })
        .collect();

    let phrase_hit: Vec<bool> = candidates.iter().map(|h| haystack_contains(h, &q)).collect();
    let any_phrase = phrase_hit.iter().any(|hit| *hit);

    // THE DEFECT THIS CLOSES, reported from a real session and reproduced
    // here: the whole query was matched as ONE substring, so a natural
    // multi-word search found nothing at all. "release checklist launch"
    // returned zero while every word appeared in the store, and the only
    // queries that worked were single distinctive words - which then returned
    // hundreds. Search was all or nothing.
    //
    // The fallback requires EVERY word the caller typed, in any order. That is
    // an AND, not an OR: it can never return an item missing one of their
    // words, and it only runs when the phrase match already found nothing, so
    // it can never change or reorder a result that already worked.
    let use_all_words = !any_phrase && words.len() > 1;
    let matched: Vec<LookupHit> = candidates
        .into_iter()
        .zip(phrase_hit)
        .filter(|(h, on_phrase)| {
            if use_all_words {
                words.iter().all(|w| haystack_contains(h, w))
            } else {
                *on_phrase
            }
        })
        .map(|(h, _)| h)
        .collect();

    let total = matched.len();
    let mut hits: Vec<LookupHit> = matched.into_iter().filter(|h| !is_expired(&h.item, &today)).collect();
    let withheld = total - hits.len();
    hits.sort_by(|a, b| a.id.cmp(&b.id));
    (hits, withheld)
}

// ------------------------------------------------------- surface 4: catalogue

/// One named collection: a `Lookup` item, whole and never ranked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    pub key: String,
    pub id: String,
    /// Non-blank lines. A register's whole point is that this number is exact.
    pub rows: usize,
    /// First and last `YYYY-MM-DD` seen at the start of a line, if any. String
    /// order is date order, so no date type is needed to say "when did this
    /// last grow".
    pub first_date: Option<String>,
    pub last_date: Option<String>,
}

/// What lives under one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeEntry {
    pub scope: String,
    pub registers: Vec<Register>,
    /// Live `Report` items carrying this scope. Counted, never listed here:
    /// the list is derived on request, so there is no second truth to drift.
    pub documents: usize,
    /// Live items carrying this scope that can still FIRE - rules and
    /// orientations, the code lane rather than the library.
    ///
    /// Counted separately but never left out of the total, because the
    /// catalogue and `in_scope` have to agree about how big a scope is.
    /// Measured on the owner's store 2026-08-17, before this field existed:
    /// the catalogue announced 42 for a scope that opened to 54 items, and a
    /// count that disagrees with the thing it counts is worse than no count.
    pub fireable: usize,
}

/// Every address the memory can be asked for by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub scopes: Vec<ScopeEntry>,
    /// Live items carrying no scope at all. THE LEAK COUNTER: if this climbs
    /// while the scoped counts stand still, the writing side is leaking, and
    /// nothing else in this system would say so.
    pub unscoped: usize,
}

/// THE DOOR THAT WAS MISSING. Measured on the owner's store 2026-08-16: 2
/// `Lookup` items out of 3523, and neither had ever been served. Not because
/// they were wrong - because `search` filters `Lookup` out by design (it
/// answers only to its own exact key) and nothing could ask which keys exist.
/// A holder nobody can discover is a write-only holder.
///
/// Read-only, and complete by construction: one pass over the live items, the
/// same pass `search` already makes, with no ranking, no floor and no cap. Its
/// size is bounded by the number of registers, never by the number of rows.
pub fn catalog(store: &EventStore) -> Catalog {
    let mut by_scope: std::collections::BTreeMap<String, ScopeEntry> = Default::default();
    let mut unscoped = 0usize;

    for li in live_items(store) {
        match li.item.kind {
            // A register announces its own scope: the first word of its key.
            // That is what makes the lane self-declaring - the thing that
            // opens a scope is the thing that carries it, so there is no table
            // to keep in step and nothing that can fall behind.
            Kind::Lookup => {
                let Some(key) = li.item.key.clone().filter(|k| !k.trim().is_empty()) else { continue };
                let scope = key.split_whitespace().next().unwrap_or(&key).to_string();
                let rows: Vec<&str> = li.item.text.lines().filter(|l| !l.trim().is_empty()).collect();
                let dates: Vec<String> = rows.iter().filter_map(|l| leading_date(l)).collect();
                by_scope.entry(scope.clone()).or_insert_with(|| ScopeEntry {
                    scope,
                    registers: Vec::new(),
                    documents: 0,
                    fireable: 0,
                });
                let entry = by_scope.get_mut(key.split_whitespace().next().unwrap_or(&key)).expect("just inserted");
                entry.registers.push(Register {
                    key,
                    id: li.id.clone(),
                    rows: rows.len(),
                    first_date: dates.first().cloned(),
                    last_date: dates.last().cloned(),
                });
            }
            Kind::Report | Kind::Chunk => match li.item.project.as_deref() {
                Some(p) if !p.trim().is_empty() => {
                    by_scope
                        .entry(p.to_string())
                        .or_insert_with(|| ScopeEntry {
                            scope: p.to_string(),
                            registers: Vec::new(),
                            documents: 0,
                            fireable: 0,
                        })
                        .documents += 1;
                }
                _ => unscoped += 1,
            },
            // Rules and orientations: the code lane. They still belong to
            // their scope's total - `in_scope` returns them - so a scope that
            // holds them must say so rather than quietly leave them out.
            _ => match li.item.project.as_deref() {
                Some(p) if !p.trim().is_empty() => {
                    by_scope
                        .entry(p.to_string())
                        .or_insert_with(|| ScopeEntry {
                            scope: p.to_string(),
                            registers: Vec::new(),
                            documents: 0,
                            fireable: 0,
                        })
                        .fireable += 1;
                }
                _ => unscoped += 1,
            },
        }
    }

    let mut scopes: Vec<ScopeEntry> = by_scope.into_values().collect();
    for s in &mut scopes {
        s.registers.sort_by(|a, b| a.key.cmp(&b.key));
    }
    Catalog { scopes, unscoped }
}

/// The `YYYY-MM-DD` a register row starts with, if it starts with one. Kept
/// deliberately dumb: ten characters, four-two-two with hyphens, all digits.
/// A row without one is not an error, it simply carries no date.
fn leading_date(line: &str) -> Option<String> {
    let s: String = line.trim_start().chars().take(10).collect();
    let b = s.as_bytes();
    if b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        Some(s)
    } else {
        None
    }
}

/// The catalogue as a person reads it. One place, so both doors say the same.
pub fn render_catalog(cat: &Catalog) -> String {
    if cat.scopes.is_empty() && cat.unscoped == 0 {
        return "this memory holds nothing yet".to_string();
    }
    let mut out = String::new();
    for s in &cat.scopes {
        // The total comes FIRST and is the sum of the three, so this number
        // and the one `in_scope` reports can never drift apart.
        out.push_str(&format!(
            "scope {:<20} {:>4} item(s) = {} register(s), {} document(s), {} rule(s)\n",
            s.scope,
            s.registers.len() + s.documents + s.fireable,
            s.registers.len(),
            s.documents,
            s.fireable
        ));
        for r in &s.registers {
            let span = match (&r.first_date, &r.last_date) {
                (Some(a), Some(b)) => format!("   {a} .. {b}"),
                _ => String::new(),
            };
            out.push_str(&format!("  {:<26} id={:<22} {:>4} rows{}\n", r.key, r.id, r.rows, span));
        }
    }
    if cat.unscoped > 0 {
        out.push_str(&format!("({} live item(s) carry no scope at all.)\n", cat.unscoped));
    }
    out.push_str(
        "Open a scope by name to see what it holds, one line per item. Ask for a register by key \
         to get it whole - never ranked, never capped, never expiring.\n",
    );
    out
}

/// The scopes that exist, as one line to choose from - names and sizes only,
/// never contents.
///
/// FOR A REFUSAL, which is why it is this small. A write that names no scope
/// is refused by the gate, and the gate can only name the rule: it is handed
/// an item and never the store. Appending this turns "look up which scopes
/// exist, then write again" into "write again", which is the difference
/// between three calls and two. Sorted by size, biggest first: the scope an
/// everyday fact belongs to is almost never the emptiest one.
pub fn scope_menu(store: &EventStore) -> String {
    let cat = catalog(store);
    let mut sized: Vec<(usize, String)> = cat
        .scopes
        .iter()
        .map(|s| (s.registers.len() + s.documents + s.fireable, s.scope.clone()))
        .collect();
    sized.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if sized.is_empty() {
        return "there are no scopes yet, so this one would be the first - ask the owner what to call it".to_string();
    }
    // Bounded on purpose: a refusal that grows with the store stops being
    // readable exactly when the store is worth reading.
    const MAX_SHOWN: usize = 25;
    let shown: Vec<String> = sized.iter().take(MAX_SHOWN).map(|(n, s)| format!("{s} ({n})")).collect();
    let tail = if sized.len() > MAX_SHOWN {
        format!(", and {} more - call lookup with no arguments for all of them", sized.len() - MAX_SHOWN)
    } else {
        String::new()
    };
    format!("scopes that already exist: {}{tail}", shown.join(", "))
}

// ------------------------------------------------------ surface 4: one scope

/// A folder listing shows this many items before it says how many it did not.
pub const MAX_SCOPE_ROWS: usize = 200;

/// Everything filed under one scope, as a list of one-line headings.
///
/// THE SHAPE THIS REPLACES. A growing collection used to have to live inside
/// ONE `Lookup` item - every book on its own line of a single page - because
/// that was the only holder whose answer could not be ranked away or capped.
/// That page is unreadable at a hundred rows, cannot be filtered, has no room
/// for a book's own notes, and grows only by appending to a blob of text.
///
/// Here a collection is simply the items carrying the same scope: each one an
/// ordinary item, written the ordinary way, added by writing another one. The
/// collection is what you get by naming the scope, and this listing is the
/// index into it. Complete by construction - one pass over the live items, no
/// ranking, no similarity floor, no expiry filter - so the count at the top is
/// the true size even when the rendering below it is cut.
pub fn in_scope(store: &EventStore, scope: &str) -> Vec<LookupHit> {
    let want = scope.trim().to_lowercase();
    let mut hits: Vec<LookupHit> = live_items(store)
        .into_iter()
        .filter(|li| match li.item.kind {
            // A register announces its own scope through the first word of its
            // key, exactly as `catalog` reads it, so a scope holds its own
            // registers too and the two doors can never disagree about where
            // something lives.
            Kind::Lookup => li
                .item
                .key
                .as_deref()
                .and_then(|k| k.split_whitespace().next())
                .is_some_and(|first| first.to_lowercase() == want),
            _ => li
                .item
                .project
                .as_deref()
                .is_some_and(|p| p.trim().to_lowercase() == want),
        })
        .map(|li| LookupHit { id: li.id, item: li.item })
        .collect();

    // Dated entries in date order, undated after them by id. A reading list, a
    // diary and a ledger are all written in time order, and a listing that
    // ignores that is a bag of rows rather than a record. Undated items are not
    // an error - they simply have no place on the timeline, so they follow it.
    hits.sort_by(|a, b| match (opening_date(&a.item.text), opening_date(&b.item.text)) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.id.cmp(&b.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.id.cmp(&b.id),
    });
    hits
}

/// Keep only the hits filed under `scope`. Applied AFTER a search, so the order
/// a query earned is untouched: this narrows, it never reorders.
pub fn only_scope(hits: Vec<LookupHit>, scope: &str) -> Vec<LookupHit> {
    let want = scope.trim().to_lowercase();
    hits.into_iter()
        .filter(|h| h.item.project.as_deref().is_some_and(|p| p.trim().to_lowercase() == want))
        .collect()
}

/// The date a row belongs on: the `YYYY-MM-DD` its first non-blank line starts
/// with, if it starts with one.
fn opening_date(text: &str) -> Option<String> {
    text.lines().find(|l| !l.trim().is_empty()).and_then(leading_date)
}

/// The first non-blank line, shortened to something a list can hold.
fn headline(text: &str) -> String {
    const MAX: usize = 100;
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX).collect();
    match cut.rfind(char::is_whitespace).filter(|i| *i > MAX / 2) {
        Some(i) => format!("{}...", &cut[..i]),
        None => format!("{cut}..."),
    }
}

/// One scope as a person reads it: how many, then one line each.
pub fn render_scope(scope: &str, hits: &[LookupHit]) -> String {
    if hits.is_empty() {
        return format!(
            "scope '{scope}' holds nothing. Call lookup with no arguments for the scopes that do exist.\n"
        );
    }
    let mut out = format!("scope {scope}: {} item(s)\n", hits.len());
    for hit in hits.iter().take(MAX_SCOPE_ROWS) {
        out.push_str(&format!("  {:<22} {}\n", hit.id, headline(&hit.item.text)));
    }
    // A CAP, AND IT SAYS SO - the same rule the query door follows. The count
    // on the first line is the whole truth about the size either way.
    if hits.len() > MAX_SCOPE_ROWS {
        out.push_str(&format!(
            "({} more not shown. Add 'query' to search inside this scope.)\n",
            hits.len() - MAX_SCOPE_ROWS
        ));
    }
    out.push_str(
        "One line per item, not its whole text: ask for one by id with get, or add 'query' to \
         search inside this scope. To add to this scope, remember a new item carrying it - never \
         append to an existing one.\n",
    );
    out
}

/// Does this item's text or any of its tags contain `needle`, already
/// lowercased by the caller? The one place the haystack is defined, so the
/// phrase pass and the all-words fallback can never search different fields.
fn haystack_contains(hit: &LookupHit, needle: &str) -> bool {
    hit.item.text.to_lowercase().contains(needle)
        || hit.item.tags.iter().any(|t| t.to_lowercase().contains(needle))
}

/// The other door: an explicit request for exactly one `Lookup`'s key, from
/// any project - never found by `search` above, only by naming the key
/// exactly (see `model::item::Kind`'s own doc comment and
/// `rank::a_lookup_answers_only_its_own_key_never_a_moment_or_target`, the
/// injection-side half of this same guarantee).
pub fn by_key(store: &EventStore, key: &str) -> Option<LookupHit> {
    let items = live_items(store);
    if let Some(li) =
        items.iter().find(|li| li.item.kind == Kind::Lookup && li.item.key.as_deref() == Some(key))
    {
        return Some(LookupHit { id: li.id.clone(), item: li.item.clone() });
    }
    // THE ASYMMETRY THIS CLOSES, reported from a real session 2026-08-19: the
    // write gate refuses a second register whose key differs "give or take
    // case and punctuation" - it says so in the refusal - while this door
    // compared the two spellings byte for byte. So a key the store itself
    // treats as taken could not be asked for under the spelling someone
    // remembered, and the answer was a flat "not found" about a register that
    // is right there. Both sides now read a key the same way; an exact
    // spelling still wins above, so nothing that resolved before resolves
    // differently.
    let wanted = model::store::normalize_for_comparison(key);
    items
        .into_iter()
        .find(|li| {
            li.item.kind == Kind::Lookup
                && li
                    .item
                    .key
                    .as_deref()
                    .is_some_and(|k| model::store::normalize_for_comparison(k) == wanted)
        })
        .map(|li| LookupHit { id: li.id, item: li.item })
}

// ---------------------------------------------------- surface 4: meaning
//
// `search_best_effort` returns `search`'s own literal hits PLUS the items a
// text match would have missed. When the semantic path is live it ALSO
// orders the literal hits by a fused lexical+meaning score (BM25 + cosine to
// the query), so the first result is the most relevant literal match, not
// the one with the smallest database id - the recall@1 fix (2026-08-03,
// cosine-only at first). It never DROPS a literal hit, only reorders it.
// The plain `search()` fallback (no model, or no readable sidecar) still
// returns id order, unchanged.
//
// A BM25 lexical-fusion leg was tried TWICE. First attempt (A2/A3,
// 2026-08-05: `fused = bm_norm + lambda * max(cos, 0)`, mirroring 1.0's
// `recall_fused`) was REVERTED: the falsifiable prediction was that recall@1
// and the "report" category's recall@5 would move up together on the
// 2.0-native recall harness's 199 natural-language QUESTIONS
// (`serve/examples/recall_native.rs`), and the measured result was ZERO
// movement at every lambda tried - a root cause found on the same run: 0 of
// those 199 questions produce even one literal (substring) hit against the
// fact corpus at all, so the code path A2/A3 changed never once engaged.
// That was an INCONCLUSIVE test of the diagnosis, not a refutation: a
// question is essentially never a verbatim substring of a fact's own
// declarative text, so a natural-language battery cannot exercise
// literal-hit RANKING at all, only the short/identifier-style queries
// `lookup` is actually built for (a function name, a path, an error code)
// can.
//
// Second attempt, on the right instrument (2026-08-05, see
// LANE-A-IDENTIFIER-BATTERY.md): a 112-query battery of SHORT
// identifier-style queries (one to four tokens - symbol/function names,
// file paths, command names, config keys, error strings, flag names), every
// one verified to produce >=1 literal hit, gold ids established by reading
// the corpus (never by the mechanism under test), split into a 57-query
// TUNE half and a 55-query HOLD-OUT half touched exactly once. BM25 fusion
// (lambda swept 1.0/1.5/2.5 on tune) beat the prior pure-cosine reorder on
// BOTH halves: hold-out recall@1 83.6% -> 92.7% (+9.1pp), recall@3 98.2% ->
// 100.0%. SHIPPED at lambda=2.5. See `rank_literal_and_extras`'s own doc
// comment and LANE-A-IDENTIFIER-BATTERY.md for the full numbers, all three
// arms measured (including a BM25-only arm that isolated whether cosine
// reordering was actively harmful - it tied fusion on recall@1 but lost on
// recall@3, so fusion, not lexical-only, is what shipped).
//
// MIN_SIMILARITY/MAX_SEMANTIC_EXTRA (the semantic-only EXTRAS door, never
// touched by the BM25 leg above - extras have no lexical signal to fuse
// with) are documented engineering defaults, not measured or tuned against
// any labeled evaluation set at the time they were chosen - see the
// 2.0-native recall harness for the eval pass that later confirmed/adjusted
// them.

/// Minimum cosine similarity for a semantic-only hit (one `search` did not
/// already find) to be included at all. Raised from 0.45 (A4, 2026-08-05):
/// measured on the 2.0-native recall harness (`serve/examples/
/// recall_native.rs`), calling the SAME shared ranking function live uses
/// (`rank_literal_and_extras`), at a true uniform floor (every candidate
/// individually filtered, not just the top one - see that harness's own
/// note on why its exploratory "silence-gate grid" is NOT this number). At
/// 0.50: mean no-answer padding drops from 2.54 to 0.82 results per
/// no-answer query (~68%, not the ~29% an earlier, imprecisely-gated
/// measurement reported), for a SMALL but non-zero recall cost - 1 fewer
/// hit (out of 199) at both recall@3 (136->135) and recall@5 (151->150
/// expanded), all of it in the "report" category (recall@5 47%->44%,
/// 21/45->20/45); recall@1 is unaffected (52%). Kept anyway: a real recall
/// cost of ~0.5 percentage point against a much larger padding cut is still
/// a favorable trade for this floor - but "zero recall loss" (an earlier
/// claim, measured through a differently-shaped gate) does NOT hold up
/// under this direct measurement. See `LANE-A-RESULTS.md` for the full
/// numbers.
#[cfg(feature = "semantic")]
const MIN_SIMILARITY: f32 = 0.50;

/// How many semantic-only hits `search_best_effort` adds at most, beyond
/// whatever `search` already found - a rendering cap, not a claim that this
/// many are always relevant.
#[cfg(feature = "semantic")]
const MAX_SEMANTIC_EXTRA: usize = 10;

/// Lowercase alphanumeric tokens - the same simple split THOR 1.0's own
/// `tokens()` used (`thor/src/recall.rs`), minus the FTS5-escaping this
/// in-memory scorer never needs (it never builds a MATCH query).
#[cfg(feature = "semantic")]
fn bm25_tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_lowercase()).collect()
}

/// Standard BM25 (Robertson/Sparck-Jones), `BM25_K1`/`BM25_B`, smoothed idf
/// (the "+1" form: always positive, never lets a term present in every pool
/// document swing the score negative). Computed over `pool_docs` - the
/// query's OWN literal-hit pool, the only lexical index cheaply available
/// here (see this function's caller's own doc comment on why `event_fts`
/// was rejected as a lexical source).
#[cfg(feature = "semantic")]
fn bm25_raw_scores(pool_docs: &[Vec<String>], query_tokens: &[String]) -> Vec<f64> {
    let n = pool_docs.len();
    if n == 0 {
        return Vec::new();
    }
    let doc_len: Vec<usize> = pool_docs.iter().map(|d| d.len()).collect();
    let avgdl: f64 = doc_len.iter().sum::<usize>() as f64 / n as f64;

    let distinct_terms: Vec<&str> = {
        let mut seen = HashSet::new();
        query_tokens.iter().map(String::as_str).filter(|t| seen.insert(*t)).collect()
    };
    let idf: HashMap<&str, f64> = distinct_terms
        .iter()
        .map(|&t| {
            let df = pool_docs.iter().filter(|d| d.iter().any(|w| w == t)).count() as f64;
            (t, (((n as f64 - df + 0.5) / (df + 0.5)) + 1.0).ln())
        })
        .collect();

    pool_docs
        .iter()
        .enumerate()
        .map(|(i, doc)| {
            let dl = doc_len[i] as f64;
            distinct_terms
                .iter()
                .map(|&t| {
                    let f = doc.iter().filter(|w| w.as_str() == t).count() as f64;
                    if f == 0.0 {
                        return 0.0;
                    }
                    idf[t] * (f * (BM25_K1 + 1.0)) / (f + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avgdl))
                })
                .sum()
        })
        .collect()
}

/// Min-max normalize to `[0,1]` over the pool; an all-equal pool (max==min,
/// including the empty/all-zero case) carries no discriminating BM25
/// signal, so every score is set to 0.0 (fused ranking then falls back to
/// cosine alone) rather than an arbitrary constant.
#[cfg(feature = "semantic")]
fn bm25_min_max_normalize(scores: &[f64]) -> Vec<f64> {
    let max = scores.iter().cloned().fold(f64::MIN, f64::max);
    let min = scores.iter().cloned().fold(f64::MAX, f64::min);
    if !(max > min) {
        return vec![0.0; scores.len()];
    }
    scores.iter().map(|s| (s - min) / (max - min)).collect()
}

/// Standard BM25 defaults. `LAMBDA` is the fusion weight on cosine: `fused =
/// bm_norm + LAMBDA * max(cos, 0)`, mirroring THOR 1.0's `recall_fused`
/// (`bm_norm + lambda*max(cos,0)`, see `thor/src/recall.rs`).
#[cfg(feature = "semantic")]
const BM25_K1: f64 = 1.2;
#[cfg(feature = "semantic")]
const BM25_B: f64 = 0.75;
/// Swept 1.0/1.5/2.5 on a 57-query TUNE half of a 112-query identifier
/// battery (2026-08-05, see LANE-A-IDENTIFIER-BATTERY.md): 2.5 was never
/// worse and sometimes strictly better than 1.0/1.5 on recall@3 in both
/// halves. Verified ONCE on the 55-query HOLD-OUT half, untouched until
/// then: recall@1 83.6% -> 92.7% (+9.1pp), recall@3 98.2% -> 100.0%,
/// recall@5 98.2% -> 100.0%, all vs the prior pure-cosine reorder (Arm A).
/// A pure-BM25 arm (lambda=0, isolating whether cosine reordering was
/// ACTIVELY harmful) tied fusion on hold-out recall@1 and lost on recall@3
/// - fusion (not lexical-only) is what ships, since it never underperforms
/// lexical-only and keeps a semantic fallback for query shapes outside this
/// battery.
#[cfg(feature = "semantic")]
const BM25_LAMBDA: f64 = 2.5;

/// THE shared ranking core (A1, 2026-08-05; BM25-fused literal ranking
/// shipped 2026-08-05, see LANE-A-IDENTIFIER-BATTERY.md): both
/// `search_best_effort_cached` (live, below) and
/// `serve/examples/recall_native.rs` (the 2.0-native eval harness) call this
/// ONE function for literal-hit reordering plus the semantic-only extras, so
/// the two can never silently diverge on what "best" means - the exact trap
/// flagged when the harness carried its own hand-mirrored copy of this
/// logic.
///
/// Reorders `literal_ids` (already `search`'s own hits - never dropped,
/// never added to, order otherwise arbitrary) by a FUSED score:
/// `bm_norm + BM25_LAMBDA * max(cos, 0)`, where `bm_norm` is an in-memory
/// BM25 score (over `texts`, min-max normalized across `literal_ids`'
/// own pool) and `cos` is cosine similarity to the query. This replaced a
/// pure-cosine reorder (the 2026-08-03 recall@1 fix) after the BM25 leg was
/// first tried and reverted against a natural-language battery that turned
/// out to produce zero literal hits (A2/A3, see this file's own "surface 4:
/// meaning" section comment), then re-tried and CONFIRMED on a battery of
/// short identifier-style queries - the shape this door is actually built
/// for.
///
/// A literal hit whose vector or text is missing is never dropped and never
/// forced to a fixed last place any more: missing text means an empty BM25
/// document (contributes 0 to `bm_norm`), missing cosine contributes 0 to
/// the fused score - the hit still competes on whatever signal IS
/// available, rather than always sinking below every scored hit regardless
/// of how relevant it actually is.
///
/// Then picks the semantic-only extras (every `all_candidate_ids` id NOT
/// already in `literal_ids`) by cosine alone, above `min_similarity`, best
/// first, capped at `max_extra` - UNCHANGED by the BM25 leg: extras are a
/// semantic-only door, cosine is the only signal that applies to them.
#[cfg(feature = "semantic")]
pub fn rank_literal_and_extras(
    query: &str,
    query_vec: &[f32],
    literal_ids: &[String],
    all_candidate_ids: &[String],
    vectors: &HashMap<String, Vec<f32>>,
    texts: &HashMap<String, String>,
    min_similarity: f32,
    max_extra: usize,
) -> (Vec<String>, Vec<String>) {
    let cosine = |id: &str| vectors.get(id).map(|v| fastembed::similarity::cosine_similarity(query_vec, v));

    let query_tokens = bm25_tokenize(query);
    let pool_docs: Vec<Vec<String>> =
        literal_ids.iter().map(|id| bm25_tokenize(texts.get(id).map(String::as_str).unwrap_or(""))).collect();
    let raw = bm25_raw_scores(&pool_docs, &query_tokens);
    let bm_norm = bm25_min_max_normalize(&raw);

    let mut lit: Vec<(f64, String)> = literal_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let cos = cosine(id).map(|c| c as f64).unwrap_or(0.0).max(0.0);
            (bm_norm[i] + BM25_LAMBDA * cos, id.clone())
        })
        .collect();
    lit.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let lit: Vec<String> = lit.into_iter().map(|(_, id)| id).collect();

    let literal_set: HashSet<&str> = literal_ids.iter().map(String::as_str).collect();
    let mut extras: Vec<(f32, String)> = all_candidate_ids
        .iter()
        .filter(|id| !literal_set.contains(id.as_str()))
        .filter_map(|id| cosine(id).map(|s| (s, id.clone())))
        .filter(|(s, _)| *s >= min_similarity)
        .collect();
    extras.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    extras.truncate(max_extra);

    (lit, extras.into_iter().map(|(_, id)| id).collect())
}

/// Surface 4 with meaning added on top of `search`'s plain text match
/// (feature `semantic`). `vectors_path` is the sidecar file (see
/// `semantic_paths::default_vectors_path`); `model_dir` overrides
/// `semantic_paths::default_model_dir` when given.
///
/// Every failure mode here degrades SILENTLY to exactly `search`'s own
/// result, never an error and never a panic - CONTRACT: "een ontbrekend
/// model is geen fout maar een stille terugval". That covers: no per-user
/// model directory resolves at all; the directory exists but is missing a
/// model file; the vector sidecar cannot be opened; the sidecar's own
/// `model_id` does not match `semantic_paths::MODEL_ID` (a stale or foreign
/// sidecar - checked BEFORE the embedder is ever loaded, so this is cheap
/// and, crucially, never trusts vectors from a different embedding space
/// for scoring); or the embedder itself fails to load or to embed the
/// query. `status`/`doctor` are what states which of these applies, never
/// this function - see `semantic_paths::mode_line`.
///
/// This is the throwaway-cache convenience form: a one-shot caller (the CLI)
/// pays the ONNX load cost every call regardless, so it has nothing to gain
/// from keeping a cache alive between calls. A long-lived caller (the MCP
/// server) should call `search_best_effort_cached` instead and keep its own
/// `Option<Embedder>` alive across calls - see that function's own doc
/// comment for the defect this split exists to close.
#[cfg(feature = "semantic")]
pub fn search_best_effort(
    store: &EventStore,
    vectors_path: &Path,
    model_dir: Option<&Path>,
    query: &str,
) -> Vec<LookupHit> {
    let mut throwaway_cache = None;
    search_best_effort_cached(store, vectors_path, model_dir, query, &mut throwaway_cache)
}

/// Same as `search_best_effort`, except the caller supplies `embedder_cache`
/// and keeps it alive across repeated calls instead of a fresh one going out
/// of scope every time.
///
/// THE DEFECT THIS CLOSES (GAP 1, found auditing the MCP server, 2026-08-03):
/// `search_best_effort` reloads the ONNX model from disk on every single
/// call. That cost is unavoidable for a one-shot process (the CLI: one
/// call, then exit), but the MCP server (crate `mcp`, `ThorMcpServer`) is a
/// long-lived process that answers many `lookup` calls without ever
/// restarting - it was paying the full ONNX session init (roughly a second,
/// see `embed::Embedder`'s own doc comment) on every single one instead of
/// once for the life of the process.
///
/// `embedder_cache` starts `None`. The first call that reaches this point
/// loads the model and leaves it in the cache; a later call sharing the SAME
/// cache reuses it instead of loading again. A failed load leaves the cache
/// `None`, so a later call may retry it rather than being stuck failing
/// forever - the degrade-to-text-search fallback documented on
/// `search_best_effort` still applies exactly as before, call by call,
/// whichever way the embedder was obtained.
///
/// Assumes `model_dir` does not change across calls that share one cache -
/// true of the MCP server, whose model directory is resolved once and never
/// changes for the life of the process. A caller that might switch model
/// directories between calls must start a fresh cache for the new directory
/// rather than share the old one across the switch.
#[cfg(feature = "semantic")]
pub fn search_best_effort_cached(
    store: &EventStore,
    vectors_path: &Path,
    model_dir: Option<&Path>,
    query: &str,
    embedder_cache: &mut Option<crate::embed::Embedder>,
) -> Vec<LookupHit> {
    let text_hits = search(store, query);

    let Some(model_dir) = model_dir.map(Path::to_path_buf).or_else(crate::semantic_paths::default_model_dir) else {
        return text_hits;
    };
    if !crate::semantic_paths::model_present(&model_dir) {
        return text_hits;
    }
    let Ok(vs) = crate::vectors::VectorStore::open(vectors_path) else {
        return text_hits;
    };
    if vs.model_id().as_deref() != Some(crate::semantic_paths::MODEL_ID) {
        // A stale or foreign sidecar: its numbers live in some other
        // embedding space entirely. Never trusted for scoring, no matter how
        // plausible a cosine value it might produce.
        return text_hits;
    }
    if embedder_cache.is_none() {
        match crate::embed::Embedder::load(&model_dir) {
            Ok(loaded) => *embedder_cache = Some(loaded),
            Err(_) => return text_hits,
        }
    }
    let Some(embedder) = embedder_cache.as_mut() else {
        // Unreachable (the branch above just filled it or returned), kept as
        // a named fallback rather than an `expect` so a future refactor of
        // this function can never turn this into a panic.
        return text_hits;
    };
    let Ok(query_vec) = embedder.embed_one(query) else {
        return text_hits;
    };

    let already: HashSet<&str> = text_hits.iter().map(|h| h.id.as_str()).collect();
    // The same expiry rule the text side applies. Without this an item that
    // text search correctly held back could walk straight back in through the
    // meaning door - a filter with a second entrance is not a filter.
    let today = today();
    let sem_candidates: Vec<_> = live_items(store)
        .into_iter()
        .filter(|li| li.item.kind != Kind::Lookup && !already.contains(li.id.as_str()))
        .filter(|li| !is_expired(&li.item, &today))
        .collect();

    // One vector fetch covering BOTH the literal hits (to reorder them by
    // cosine) and the semantic-only candidates (the extras). If the sidecar
    // cannot be read right now, keep `search`'s own id order rather than
    // guess.
    let mut want_ids: Vec<String> = text_hits.iter().map(|h| h.id.clone()).collect();
    want_ids.extend(sem_candidates.iter().map(|li| li.id.clone()));
    // Best PART per item, not the item's single average: a long item is stored
    // as several vectors and answers with whichever piece the question is
    // actually about. A short item has exactly one part, so this is identical
    // to the old fetch for everything the current ranking was tuned on.
    let Ok(vectors) = vs.get_many_best(&want_ids, &query_vec) else {
        return text_hits;
    };

    // `all_candidate_ids`: every candidate this query may rank over - the
    // literal hits plus the semantic-only candidates. `by_id` lets the
    // ranking core work in plain ids and hand `LookupHit`s back at the end.
    let literal_ids: Vec<String> = text_hits.iter().map(|h| h.id.clone()).collect();
    let mut by_id: HashMap<String, LookupHit> = HashMap::with_capacity(want_ids.len());
    for h in text_hits {
        by_id.insert(h.id.clone(), h);
    }
    for li in sem_candidates {
        by_id.insert(li.id.clone(), LookupHit { id: li.id, item: li.item });
    }

    // Text for the BM25 leg of the literal-hit reorder - item TEXT ONLY, not
    // tags, matching exactly the configuration validated in
    // LANE-A-IDENTIFIER-BATTERY.md (an earlier draft of this line also
    // joined in tags, which measurably changed the ranking versus what was
    // tuned/verified: -3.6pp hold-out recall@1, caught by a same-run
    // cross-check against that measurement's own reimplementation before
    // shipping - see that doc's "what remains unverified" section). A
    // literal hit that matched only via a TAG gets an empty BM25 document
    // (contributes 0, not dropped - it still ranks on cosine); only the
    // literal hits need this at all, extras stay cosine-only.
    let texts: HashMap<String, String> =
        literal_ids.iter().filter_map(|id| by_id.get(id).map(|h| (id.clone(), h.item.text.clone()))).collect();

    // THE recall@1 fix (2026-08-03, cosine-only at first; BM25-fused
    // 2026-08-05): the first result must be the most relevant literal
    // match, not the one with the smallest database id. A literal match is
    // already a strong signal, so NONE are dropped here - they are only
    // reordered.
    let (lit_order, extra_order) = rank_literal_and_extras(
        query,
        &query_vec,
        &literal_ids,
        &want_ids,
        &vectors,
        &texts,
        MIN_SIMILARITY,
        MAX_SEMANTIC_EXTRA,
    );

    let mut out = Vec::with_capacity(lit_order.len() + extra_order.len());
    for id in lit_order.into_iter().chain(extra_order) {
        if let Some(hit) = by_id.remove(&id) {
            out.push(hit);
        }
    }
    out
}

/// Without the `semantic` feature compiled in, this is exactly `search` -
/// same name and signature as the feature-on version above, so every caller
/// (the CLI, in particular) compiles unchanged regardless of which build it
/// is.
#[cfg(not(feature = "semantic"))]
pub fn search_best_effort(store: &EventStore, _vectors_path: &Path, _model_dir: Option<&Path>, query: &str) -> Vec<LookupHit> {
    search(store, query)
}

// ------------------------------------------------------- surface 4: code
//
// Code is archive, exactly like Report/Chunk (see the crate-level doc
// comment on the code index this wraps) - fully searchable, never served at
// a gate. This is the ONLY place in the `serve` crate allowed to name that
// crate at all: `serve/tests/codeindex_is_lookup_only.rs` greps every other
// file under `serve/src` for that literal name and fails if it appears
// anywhere but here, so session_start/serve()/serve_prompt()/the hook
// channel can never quietly grow a path into it. Every type below is owned
// by THIS crate (never the code index's own struct re-exported), so nothing
// outside this file ever needs to name that crate either - not even for a
// type annotation.

/// One code hit, translated into a type this crate owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeHit {
    pub path: String,
    pub commit_id: String,
    pub start_line: i64,
    pub end_line: i64,
    pub text: String,
}

/// Where a code answer came from, and whether the world has moved on since -
/// same fields as the code index's own provenance type, owned here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeProvenance {
    pub indexed_commit: String,
    /// `Err(reason)` when the repository could not be reached just now -
    /// never silently treated as "no drift".
    pub current_commit: Result<String, String>,
    pub files_differ: Option<usize>,
    pub uncommitted_changed: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSearchAnswer {
    pub hits: Vec<CodeHit>,
    pub provenance: CodeProvenance,
}

fn owned_provenance(p: codeindex::Provenance) -> CodeProvenance {
    CodeProvenance {
        indexed_commit: p.indexed_commit,
        current_commit: p.current_commit,
        files_differ: p.files_differ,
        uncommitted_changed: p.uncommitted_changed,
    }
}

/// Search the code index at `index_db_path` for `query`, provenance against
/// the repository at `repo_path` on every answer. This is a human-facing
/// lookup, not an injection surface, so R5's "read and inject never speaks"
/// does not apply - a broken index or an unreachable repository is returned
/// as a plain `Err`, never swallowed.
pub fn search_code(
    index_db_path: &std::path::Path,
    repo_path: &std::path::Path,
    query: &str,
    limit: usize,
) -> Result<CodeSearchAnswer, String> {
    let store = codeindex::Store::open(index_db_path).map_err(|e| e.to_string())?;
    let answer = codeindex::search(&store, repo_path, query, limit).map_err(|e| e.to_string())?;
    Ok(CodeSearchAnswer {
        hits: answer
            .hits
            .into_iter()
            .map(|h| CodeHit {
                path: h.path,
                commit_id: h.commit_id,
                start_line: h.start_line,
                end_line: h.end_line,
                text: h.text,
            })
            .collect(),
        provenance: owned_provenance(answer.provenance),
    })
}

/// One place a name was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSite {
    pub path: String,
    pub line: i64,
}

/// Where a name is defined and where it is used, plus how stale the index is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeUsage {
    pub name: String,
    pub defined_at: Vec<CodeSite>,
    pub referenced_at: Vec<CodeSite>,
    pub provenance: CodeProvenance,
}

fn owned_sites(sites: Vec<codeindex::Site>) -> Vec<CodeSite> {
    sites.into_iter().map(|s| CodeSite { path: s.path, line: s.line }).collect()
}

/// Who defines and who uses one bare name. Provenance rides along for the same
/// reason it does on `search_code`: a line number from an index three commits
/// behind is a wrong line number, and the caller has to be able to SEE that
/// rather than infer it.
pub fn where_used(
    index_db_path: &std::path::Path,
    repo_path: &std::path::Path,
    name: &str,
    limit: usize,
) -> Result<CodeUsage, String> {
    let store = codeindex::Store::open(index_db_path).map_err(|e| e.to_string())?;
    let usage = codeindex::where_used(&store, name, limit).map_err(|e| e.to_string())?;
    let provenance = codeindex::provenance(&store, repo_path).map_err(|e| e.to_string())?;
    Ok(CodeUsage {
        name: usage.name,
        defined_at: owned_sites(usage.defined_at),
        referenced_at: owned_sites(usage.referenced_at),
        provenance: owned_provenance(provenance),
    })
}

/// What one file defines, in line order. `Ok(None)` means the index has never
/// seen that path at all, which a caller must report as "not indexed" - a
/// different answer from "this file defines nothing".
pub fn outline(
    index_db_path: &std::path::Path,
    path: &str,
) -> Result<Option<Vec<(String, i64)>>, String> {
    let store = codeindex::Store::open(index_db_path).map_err(|e| e.to_string())?;
    if !codeindex::is_indexed(&store, path).map_err(|e| e.to_string())? {
        return Ok(None);
    }
    codeindex::outline(&store, path).map(Some).map_err(|e| e.to_string())
}

/// The code index's own status: which commit it is at, and how far the
/// working copy has drifted since - the code-index half of the `status`
/// command (`crate::status` folds the fact-store half).
pub fn code_index_status(index_db_path: &std::path::Path, repo_path: &std::path::Path) -> Result<CodeProvenance, String> {
    let store = codeindex::Store::open(index_db_path).map_err(|e| e.to_string())?;
    let provenance = codeindex::provenance(&store, repo_path).map_err(|e| e.to_string())?;
    Ok(owned_provenance(provenance))
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Severity, TargetKind};
    use model::store;

    fn item(id: &str, kind: Kind, text: &str, project: Option<&str>) -> Item {
        Item {
            id: id.to_string(),
            kind,
            text: text.to_string(),
            bindings: vec![],
            severity: None,
            // A scope by default for archive material, because ground 21
            // refuses it without one - otherwise every fixture here would be
            // exercising that ground instead of the search behaviour it is
            // named after. An explicit scope always wins.
            project: project
                .map(str::to_string)
                .or_else(|| (!kind.can_fire()).then(|| "test-project".to_string())),
            tags: vec![],
            expires: if kind == Kind::Report { Some("2027-01-01".to_string()) } else { None },
            key: None,
            falsifier: if kind.can_fire() {
                Some("this synthetic fixture item turns out to be wrong".to_string())
            } else {
                None
            },
            check: None,
        }
    }

    /// A collection is a SCOPE holding one item per entry, so opening it must
    /// return every entry - and only that scope's entries. This is the whole
    /// point of the shape: a book is added by writing another item, never by
    /// appending a line to a page that already exists.
    #[test]
    fn a_scope_opens_to_every_item_filed_under_it_and_nothing_else() {
        let mut store = EventStore::in_memory().unwrap();
        for (id, text, scope) in [
            ("dune", "2026-08-11 Dune - Frank Herbert, uitgelezen", Some("boeken")),
            ("piranesi", "2026-08-16 Piranesi - Susanna Clarke, halverwege", Some("boeken")),
            ("pizza", "2026-08-01 Napolitaans deeg, 65% hydratatie", Some("eten")),
            ("elders", "2026-08-02 een feit in een derde scope", Some("investments")),
        ] {
            store::declare(&mut store, "s", "l", "t", &item(id, Kind::Report, text, scope)).unwrap();
        }

        let hits = in_scope(&store, "boeken");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["dune", "piranesi"], "only this scope, and all of it");
    }

    /// THE DEFECT THIS CLOSES, measured on the owner's own store 2026-08-17:
    /// the catalogue announced 42 for a scope that opened to 54 items, because
    /// it counted documents and registers but silently skipped the rules filed
    /// there. A count that disagrees with the thing it counts is worse than no
    /// count at all - so the two doors are tied together here.
    #[test]
    fn the_catalogues_total_for_a_scope_equals_what_opening_that_scope_returns() {
        let mut store = EventStore::in_memory().unwrap();
        let mut doc = item("doc", Kind::Report, "een document in deze scope", Some("gemengd"));
        doc.project = Some("gemengd".to_string());
        store::declare(&mut store, "s", "l", "t", &doc).unwrap();

        let mut regel = item("regel", Kind::Rule, "een vuurbare regel in dezelfde scope", Some("gemengd"));
        regel.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/lib.rs".to_string() }];
        regel.severity = Some(Severity::HouseStyle);
        store::declare(&mut store, "s", "l", "t", &regel).unwrap();

        let mut reg = item("reg", Kind::Lookup, "2026-08-01 een rij", None);
        reg.key = Some("gemengd lijst".to_string());
        store::declare(&mut store, "s", "l", "t", &reg).unwrap();

        let cat = catalog(&store);
        let entry = cat.scopes.iter().find(|s| s.scope == "gemengd").expect("the scope must be listed");
        let announced = entry.registers.len() + entry.documents + entry.fireable;
        assert_eq!(announced, 3, "one of each kind: {entry:?}");
        assert_eq!(announced, in_scope(&store, "gemengd").len(), "the catalogue must not undercount its own scope");
    }

    /// A reading list, a diary and a ledger are written in time order, so the
    /// listing follows the dates the rows carry rather than the ids they were
    /// minted with - ids are hashes and carry no order at all.
    #[test]
    fn a_scope_lists_dated_entries_in_date_order_and_undated_ones_after_them() {
        let mut store = EventStore::in_memory().unwrap();
        for (id, text) in [
            ("zzz-oldest", "2026-01-02 het eerste boek"),
            ("aaa-newest", "2026-09-30 het laatste boek"),
            ("mmm-undated", "een boek zonder datum"),
        ] {
            store::declare(&mut store, "s", "l", "t", &item(id, Kind::Report, text, Some("boeken"))).unwrap();
        }

        let ids: Vec<String> = in_scope(&store, "boeken").into_iter().map(|h| h.id).collect();
        assert_eq!(ids, vec!["zzz-oldest", "aaa-newest", "mmm-undated"], "date order first, undated last");
    }

    /// A register announces its own scope through its key, and `catalog` reads
    /// it that way, so opening the scope has to find it too - otherwise the
    /// catalogue would name a scope whose contents are one item short.
    #[test]
    fn a_scope_holds_its_own_registers_as_well_as_its_documents() {
        let mut store = EventStore::in_memory().unwrap();
        let mut reg = item("uitgaven-2026", Kind::Lookup, "2026-08-01 huur 900", None);
        reg.key = Some("uitgaven 2026".to_string());
        store::declare(&mut store, "s", "l", "t", &reg).unwrap();

        let ids: Vec<String> = in_scope(&store, "uitgaven").into_iter().map(|h| h.id).collect();
        assert_eq!(ids, vec!["uitgaven-2026"], "the first word of the key is the scope");
    }

    /// The listing is an index, not a dump: one line per item, so a scope with
    /// a hundred entries stays readable. That is the defect the whole shape
    /// exists to avoid - a page of a thousand lines nobody can use.
    #[test]
    fn a_scope_listing_shows_one_shortened_line_per_item_never_its_whole_text() {
        let mut store = EventStore::in_memory().unwrap();
        let long = format!("2026-08-11 Dune - {}", "een heel lange notitie ".repeat(30));
        store::declare(&mut store, "s", "l", "t", &item("dune", Kind::Report, &long, Some("boeken"))).unwrap();

        let rendered = render_scope("boeken", &in_scope(&store, "boeken"));
        let body: Vec<&str> = rendered.lines().filter(|l| l.starts_with("  dune")).collect();
        assert_eq!(body.len(), 1, "exactly one line for the item: {rendered}");
        assert!(body[0].contains("Dune"), "the heading survives: {}", body[0]);
        assert!(body[0].ends_with("..."), "and the rest is cut, visibly: {}", body[0]);
        assert!(!body[0].contains(long.trim_end()), "the whole text is never printed");
    }

    /// Narrowing a search to one scope may only ever REMOVE hits. If it could
    /// add one, a scoped search would be a different search rather than the
    /// same one seen through a window.
    #[test]
    fn narrowing_a_search_to_a_scope_only_ever_removes_hits_never_reorders_them() {
        let mut store = EventStore::in_memory().unwrap();
        for (id, text, scope) in [
            ("b1", "deeg en boeken staan samen op dezelfde plank", Some("boeken")),
            ("e1", "een kilo deeg, gerezen, plus boeken erover geleend", Some("eten")),
            ("b2", "boeken over deeg, van gist tot desem uitgelegd", Some("boeken")),
        ] {
            store::declare(&mut store, "s", "l", "t", &item(id, Kind::Report, text, scope)).unwrap();
        }

        let wide = search(&store, "deeg boeken");
        let wide_order: Vec<String> = wide.iter().map(|h| h.id.clone()).collect();
        let narrow: Vec<String> = only_scope(wide, "boeken").into_iter().map(|h| h.id).collect();

        assert_eq!(narrow, vec!["b1", "b2"]);
        let kept: Vec<&String> = wide_order.iter().filter(|id| narrow.contains(id)).collect();
        assert_eq!(kept, narrow.iter().collect::<Vec<_>>(), "the surviving hits keep the order they had");
    }

    /// THE DEFECT THIS CLOSES, reported from a real session and reproduced:
    /// the whole query was matched as one substring, so a natural multi-word
    /// search found nothing while every word appeared in the store. Search was
    /// all or nothing - a phrase gave zero, a single common word gave hundreds.
    #[test]
    fn a_multi_word_query_finds_an_item_carrying_all_the_words() {
        let mut store = EventStore::in_memory().unwrap();
        let item = item("release-note", Kind::Report, "Werk de checklist bij voor je een release doet, en pas dan de launch", None);
        store::declare(&mut store, "s", "l", "t", &item).unwrap();

        // The words are all there, in a different order, never as this phrase.
        let hits = search(&store, "release checklist launch");
        assert_eq!(hits.len(), 1, "every word is present, so it must be found");
        assert_eq!(hits[0].id, "release-note");
    }

    /// It is an AND, never an OR: an item missing one of the caller's words is
    /// not a match. Widening to "any word" would turn every search into a
    /// dump, which is the other half of the defect above.
    #[test]
    fn a_multi_word_query_never_matches_an_item_missing_one_of_the_words() {
        let mut store = EventStore::in_memory().unwrap();
        let item = item("partial", Kind::Report, "Werk de checklist bij voor je een release doet", None);
        store::declare(&mut store, "s", "l", "t", &item).unwrap();

        assert!(search(&store, "release checklist launch").is_empty(), "launch is missing, so this is not a match");
    }

    /// The fallback runs ONLY when the phrase found nothing, so it can never
    /// change or widen a search that already worked.
    #[test]
    fn an_exact_phrase_match_is_never_widened_by_the_fallback() {
        let mut store = EventStore::in_memory().unwrap();
        let exact = item("exact", Kind::Report, "de release checklist hangt aan de launch", None);
        store::declare(&mut store, "s", "l", "t", &exact).unwrap();
        let scattered = item("scattered", Kind::Report, "checklist eerst, launch later, release ergens", None);
        store::declare(&mut store, "s", "l", "t", &scattered).unwrap();

        let hits = search(&store, "release checklist");
        assert_eq!(hits.len(), 1, "the phrase matched, so the all-words pass must not run");
        assert_eq!(hits[0].id, "exact");
    }

    #[test]
    fn search_finds_a_report_by_text() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("r1", Kind::Report, "the bbq recipe lives here", None)).unwrap();
        let hits = search(&db, "bbq");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "r1");
    }

    #[test]
    fn search_finds_a_chunk_by_text() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("c1", Kind::Chunk, "fn add(a: i32, b: i32) -> i32", None)).unwrap();
        let hits = search(&db, "i32");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "c1");
    }

    #[test]
    fn search_never_returns_a_lookup_even_when_its_text_matches() {
        // The defect this test names: a Lookup is a different door (only
        // by_key answers it) - it must never leak into general search even
        // when its own text contains the query.
        let mut db = EventStore::in_memory().unwrap();
        let mut lookup = item("l1", Kind::Lookup, "the release checklist lives in RELEASE.md", None);
        lookup.key = Some("release-checklist".to_string());
        store::declare(&mut db, "s", "l", "a", &lookup).unwrap();
        assert!(search(&db, "checklist").is_empty(), "a Lookup must never appear in general search results");
    }

    #[test]
    fn search_is_not_scoped_to_any_single_project() {
        // Two DIFFERENT items (the near-duplicate gate would refuse a
        // second live Report with the same or near-same text regardless of
        // project - project is not part of that comparison), each in its
        // own project, both still matching the query - what this test is
        // actually about is that both projects come back at all.
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &item("a1", Kind::Report, "filament tuning notes for the printer project", Some("printer-project"))).unwrap();
        store::declare(&mut db, "s", "l", "a", &item("b1", Kind::Report, "filament calibration log for the laser cutter", Some("other-project"))).unwrap();
        let hits = search(&db, "filament");
        assert_eq!(hits.len(), 2, "items from every project must participate, never scoped by default");
    }

    #[test]
    fn search_matches_are_ordered_by_id() {
        let mut db = EventStore::in_memory().unwrap();
        // Distinct text per item (the near-duplicate gate refuses a second
        // live item of the same kind with the same or near-same text), each
        // still containing "matching" so all three stay hits, and
        // deliberately NOT in id order: declared z1, a1, m1, and each
        // item's own text sorts m1, z1, a1 alphabetically - neither the
        // declare order nor the text order equals "a1, m1, z1", so only a
        // sort keyed on id produces the order this test asserts.
        let texts = [
            ("z1", "the matching color palette is stored separately"),
            ("a1", "these matching socks always go missing"),
            ("m1", "a matching brand of screws was ordered"),
        ];
        for (id, text) in texts {
            store::declare(&mut db, "s", "l", "a", &item(id, Kind::Report, text, None)).unwrap();
        }
        let hits = search(&db, "matching");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["a1", "m1", "z1"]);
    }

    #[test]
    fn search_also_finds_a_rule_or_orientation_not_only_archive_kinds() {
        let mut db = EventStore::in_memory().unwrap();
        let mut rule = item("rule1", Kind::Rule, "never force-push to main", None);
        rule.bindings = vec![Binding::Always];
        rule.severity = Some(Severity::Irreversible);
        // Gate ground 11: bound Always there is no literal to forbid, so the
        // tag is the honest answer. This test is about search, not teeth.
        rule.tags = vec![format!("{}a test fixture with nothing literal to catch", store::NO_LITERAL_REASON_PREFIX)];
        store::declare(&mut db, "s", "l", "a", &rule).unwrap();
        let hits = search(&db, "force-push");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn by_key_finds_the_exact_lookup() {
        let mut db = EventStore::in_memory().unwrap();
        let mut lookup = item("l2", Kind::Lookup, "the release checklist lives in RELEASE.md", None);
        lookup.key = Some("release-checklist".to_string());
        store::declare(&mut db, "s", "l", "a", &lookup).unwrap();
        let hit = by_key(&db, "release-checklist").expect("expected to find the lookup by its key");
        assert_eq!(hit.id, "l2");
    }

    #[test]
    /// THE ASYMMETRY THIS CLOSES, from a real session 2026-08-19: the write
    /// gate refuses a second register whose key differs only in case or
    /// punctuation, while this door compared byte for byte - so a key the
    /// store itself calls taken answered "not found".
    #[test]
    fn by_key_finds_a_register_whose_key_differs_only_in_case_or_punctuation() {
        let mut store = EventStore::in_memory().unwrap();
        let mut reg = item("uitgaven", Kind::Lookup, "2026-08-01 koffie 3,20", None);
        reg.key = Some("Uitgaven 2026-08".to_string());
        store::declare(&mut store, "s", "l", "t", &reg).unwrap();

        assert!(by_key(&store, "Uitgaven 2026-08").is_some(), "the exact spelling still works");
        assert!(by_key(&store, "uitgaven-2026-08").is_some(), "and so does the one the gate calls the same");
        assert!(by_key(&store, "uitgaven 2026 08").is_some());
        assert!(by_key(&store, "uitgaven-2026-09").is_none(), "a different key stays a different key");
    }

    fn by_key_never_matches_an_unknown_key() {
        let db = EventStore::in_memory().unwrap();
        assert!(by_key(&db, "does-not-exist").is_none());
    }

    #[test]
    fn by_key_never_matches_a_different_kind_even_with_a_target_binding() {
        // A Target binding's `value` is a plain path/command string, never
        // this door's `key` - construct a Rule whose declared Target VALUE
        // happens to equal the key text, and confirm by_key still refuses it
        // because it is not a Lookup at all. Target kind Command, not Path:
        // "release-checklist" has no separator and no extension, so as a
        // Path value the write gate's ground 16 (`model::gate`, via
        // `model::anchor_shape::unmatchable`) refuses it as a BareWord that
        // can never match a real file - Command carries no such rule, and
        // this test's collision only needs the VALUE to equal the key, never
        // any particular Target kind.
        let mut db = EventStore::in_memory().unwrap();
        let mut rule = item("rule2", Kind::Rule, "see the release checklist", None);
        rule.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "release-checklist".to_string() }];
        rule.severity = Some(Severity::HouseStyle);
        store::declare(&mut db, "s", "l", "a", &rule).unwrap();
        assert!(by_key(&db, "release-checklist").is_none(), "only a real Lookup's key may answer here");
    }
}

#[cfg(test)]
mod code_search_tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// A throwaway git repository under a tempdir - the same minimal fixture
    /// shape `codeindex`'s own tests use, rebuilt here rather than shared
    /// across a test-only dependency edge, since this is the only place in
    /// `serve` that ever needs one.
    struct TestRepo {
        dir: tempfile::TempDir,
    }

    impl TestRepo {
        fn init() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let run = |args: &[&str]| {
                let out = Command::new("git").arg("-C").arg(dir.path()).args(args).output().expect("git must be on PATH");
                assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
            };
            run(&["init", "--quiet"]);
            run(&["config", "user.email", "test@example.invalid"]);
            run(&["config", "user.name", "lookup code-search tests"]);
            TestRepo { dir }
        }

        fn write(&self, rel_path: &str, content: &str) {
            fs::write(self.dir.path().join(rel_path), content).unwrap();
        }

        fn commit(&self, message: &str) -> String {
            let out = Command::new("git").arg("-C").arg(self.dir.path()).args(["add", "-A"]).output().unwrap();
            assert!(out.status.success());
            let out =
                Command::new("git").arg("-C").arg(self.dir.path()).args(["commit", "--quiet", "-m", message]).output().unwrap();
            assert!(out.status.success());
            let out = Command::new("git").arg("-C").arg(self.dir.path()).args(["rev-parse", "HEAD"]).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn path(&self) -> &std::path::Path {
            self.dir.path()
        }
    }

    #[test]
    fn search_code_finds_a_hit_with_its_commit() {
        let repo = TestRepo::init();
        repo.write("greeter.rs", "fn greet() -> &'static str { \"hello from the fixture\" }");
        let commit = repo.commit("initial");

        let index_dir = tempfile::tempdir().unwrap();
        let index_db = index_dir.path().join("index.db");
        {
            let mut store = codeindex::Store::open(&index_db).unwrap();
            codeindex::build_full(&mut store, repo.path()).unwrap();
        }

        let answer = search_code(&index_db, repo.path(), "hello from the fixture", 10).unwrap();
        assert_eq!(answer.hits.len(), 1);
        assert_eq!(answer.hits[0].path, "greeter.rs");
        assert_eq!(answer.hits[0].commit_id, commit);
        assert_eq!(answer.provenance.indexed_commit, commit);
        assert_eq!(answer.provenance.current_commit, Ok(commit));
        assert_eq!(answer.provenance.files_differ, Some(0));
    }

    #[test]
    fn search_code_over_an_unbuilt_index_is_a_named_error_not_a_panic() {
        let repo = TestRepo::init();
        repo.write("a.txt", "content");
        repo.commit("initial");
        let index_dir = tempfile::tempdir().unwrap();
        let index_db = index_dir.path().join("index.db");
        // deliberately never built
        let err = search_code(&index_db, repo.path(), "content", 10).unwrap_err();
        assert!(!err.is_empty(), "a lookup, not an injection surface, must report the error plainly");
    }

    #[test]
    fn code_index_status_reports_drift_after_a_new_commit() {
        let repo = TestRepo::init();
        repo.write("a.txt", "version one");
        let first = repo.commit("v1");

        let index_dir = tempfile::tempdir().unwrap();
        let index_db = index_dir.path().join("index.db");
        {
            let mut store = codeindex::Store::open(&index_db).unwrap();
            codeindex::build_full(&mut store, repo.path()).unwrap();
        }

        repo.write("a.txt", "version two");
        let second = repo.commit("v2");

        let status = code_index_status(&index_db, repo.path()).unwrap();
        assert_eq!(status.indexed_commit, first);
        assert_eq!(status.current_commit, Ok(second));
        assert_eq!(status.files_differ, Some(1));
    }
}

/// The shared ranking core (A1 cosine reorder, 2026-08-05; BM25 fusion
/// shipped 2026-08-05), tested directly against synthetic vectors/texts - no
/// store, no ONNX embedder needed, since `rank_literal_and_extras` takes
/// plain ids, a vector map and a text map. This is the ONE ranking
/// implementation both `search_best_effort_cached` (below) and the
/// `recall_native`/`identifier_battery_eval` eval harnesses call, so a test
/// here covers both.
///
/// Most tests below pass `query = ""` and an empty `texts` map: an empty
/// query tokenizes to zero terms, so `bm_norm` is 0.0 for every candidate
/// regardless of `texts` (see `bm25_raw_scores`/`bm25_min_max_normalize`) -
/// the fused score collapses to `BM25_LAMBDA * max(cos, 0)`, a positive
/// scaling of cosine alone, so these tests still isolate the cosine-only
/// half of the ranking exactly as they did before BM25 fusion shipped. The
/// BM25-specific tests at the end pass a real query and real texts.
#[cfg(all(test, feature = "semantic"))]
mod rank_literal_and_extras_tests {
    use super::*;

    fn v(vectors: &mut HashMap<String, Vec<f32>>, id: &str, vec: Vec<f32>) {
        vectors.insert(id.to_string(), vec);
    }

    #[test]
    fn literal_hits_are_reordered_by_the_fused_score_never_dropped_or_added_to() {
        let mut vectors = HashMap::new();
        let query_vec = vec![1.0, 0.0];
        v(&mut vectors, "weak", vec![0.6, 0.8]); // lower cosine to [1,0]
        v(&mut vectors, "strong", vec![0.99, 0.14]); // higher cosine to [1,0]
        let literal_ids = vec!["weak".to_string(), "strong".to_string()];
        let texts = HashMap::new();
        let (lit, extras) = rank_literal_and_extras("", &query_vec, &literal_ids, &[], &vectors, &texts, 0.0, 10);
        assert_eq!(lit, vec!["strong".to_string(), "weak".to_string()], "the more similar literal hit must come first");
        assert!(extras.is_empty(), "an empty candidate pool yields no extras");
        let mut lit_sorted = lit.clone();
        lit_sorted.sort();
        assert_eq!(lit_sorted, vec!["strong".to_string(), "weak".to_string()], "reordering never drops or adds a literal hit");
    }

    #[test]
    fn a_literal_hit_with_no_vector_and_no_bm25_signal_ranks_by_cosine_alone() {
        // Named after the defect the OLD cosine-only version guarded
        // differently (an unconditional "sink to last" rule): under the
        // fused score a missing vector contributes 0 to the cosine term
        // rather than forcing a fixed last place, so this case still ends
        // up in the same order here (no BM25 signal for either id, so the
        // one WITH a positive cosine wins) - but see the BM25-signal tests
        // below for a case where a missing-vector hit now WINS outright.
        let mut vectors = HashMap::new();
        let query_vec = vec![1.0, 0.0];
        v(&mut vectors, "scored", vec![0.5, 0.5]);
        // "unscored" has no entry in `vectors` at all.
        let literal_ids = vec!["unscored".to_string(), "scored".to_string()];
        let texts = HashMap::new();
        let (lit, _) = rank_literal_and_extras("", &query_vec, &literal_ids, &[], &vectors, &texts, 0.0, 10);
        assert_eq!(lit, vec!["scored".to_string(), "unscored".to_string()], "with no BM25 signal, a hit with no vector never jumps ahead of a scored one");
    }

    #[test]
    fn semantic_extras_respect_the_floor_and_the_cap_and_exclude_literal_ids() {
        let mut vectors = HashMap::new();
        let query_vec = vec![1.0, 0.0];
        v(&mut vectors, "lit1", vec![1.0, 0.0]); // a literal hit - must never appear as an extra too
        v(&mut vectors, "below_floor", vec![0.0, 1.0]); // cosine 0.0, below any positive floor
        for i in 0..15 {
            // 15 candidates all comfortably above the floor, to prove the cap trims to 10.
            v(&mut vectors, &format!("extra{i}"), vec![0.9, 0.1]);
        }
        let literal_ids = vec!["lit1".to_string()];
        let all_ids: Vec<String> = vectors.keys().cloned().collect();
        let texts = HashMap::new();
        let (lit, extras) = rank_literal_and_extras("", &query_vec, &literal_ids, &all_ids, &vectors, &texts, 0.5, 10);
        assert_eq!(lit, vec!["lit1".to_string()]);
        assert_eq!(extras.len(), 10, "the cap must never be exceeded");
        assert!(!extras.contains(&"lit1".to_string()), "a literal hit must never also appear as an extra");
        assert!(!extras.contains(&"below_floor".to_string()), "an extra below the floor must never be included");
    }

    #[test]
    fn no_literal_hits_and_no_extras_above_floor_yields_nothing() {
        let mut vectors = HashMap::new();
        let query_vec = vec![1.0, 0.0];
        v(&mut vectors, "far", vec![-1.0, 0.0]); // cosine -1.0
        let texts = HashMap::new();
        let (lit, extras) = rank_literal_and_extras("", &query_vec, &[], &["far".to_string()], &vectors, &texts, 0.45, 10);
        assert!(lit.is_empty());
        assert!(extras.is_empty(), "a candidate scoring below the floor must never be added as an extra");
    }

    #[test]
    fn bm25_breaks_a_cosine_tie_via_term_frequency_and_length_normalization() {
        // Both hits get the SAME cosine, so the fused ranking can only be
        // decided by the BM25 term - proves the lexical leg actually
        // contributes signal, not just a cosine pass-through.
        let mut vectors = HashMap::new();
        let query_vec = vec![1.0, 0.0];
        v(&mut vectors, "freq_hit", vec![0.5, 0.8660254]); // cos 0.5, tied with below
        v(&mut vectors, "single_hit", vec![0.5, 0.8660254]); // cos 0.5, tied with above
        let literal_ids = vec!["freq_hit".to_string(), "single_hit".to_string()];
        let mut texts = HashMap::new();
        texts.insert("freq_hit".to_string(), "uniquetoken uniquetoken uniquetoken".to_string());
        texts.insert("single_hit".to_string(), "uniquetoken other unrelated padding words more".to_string());
        let (lit, _) = rank_literal_and_extras("uniquetoken", &query_vec, &literal_ids, &[], &vectors, &texts, 0.0, 10);
        assert_eq!(
            lit,
            vec!["freq_hit".to_string(), "single_hit".to_string()],
            "with tied cosine, higher term frequency (length-normalized) must win"
        );
    }

    #[test]
    fn a_literal_hit_with_no_vector_can_still_outrank_a_vectored_one_via_bm25() {
        // The behavior change this documents: a missing vector no longer
        // forces a fixed last place (contrast with the cosine-only test
        // above) - a strong enough BM25 signal can outrank a hit that DOES
        // have a vector but a weak one and a weak keyword match.
        let mut vectors = HashMap::new();
        let query_vec = vec![1.0, 0.0];
        // "no_vector_strong_bm25" has NO entry in `vectors` at all.
        v(&mut vectors, "has_vector_weak_bm25", vec![0.3, 0.9539392]); // cos 0.3
        let literal_ids = vec!["no_vector_strong_bm25".to_string(), "has_vector_weak_bm25".to_string()];
        let mut texts = HashMap::new();
        texts.insert("no_vector_strong_bm25".to_string(), "rareterm rareterm rareterm rareterm rareterm".to_string());
        texts.insert("has_vector_weak_bm25".to_string(), "rareterm".to_string());
        let (lit, _) = rank_literal_and_extras("rareterm", &query_vec, &literal_ids, &[], &vectors, &texts, 0.0, 10);
        assert_eq!(
            lit,
            vec!["no_vector_strong_bm25".to_string(), "has_vector_weak_bm25".to_string()],
            "a missing vector must not force a hit to the bottom when its BM25 signal is strong enough"
        );
    }
}

/// `search_best_effort`'s own fallback/guard paths - deliberately never
/// loading the real ONNX model (both scenarios below return before
/// `embed::Embedder::load` is ever called), so these stay fast and
/// hermetic, unlike the real-data run CONTRACT's own gate calls for.
#[cfg(all(test, feature = "semantic"))]
mod semantic_search_tests {
    use super::*;
    use model::store;

    fn report(id: &str, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: text.to_string(),
            bindings: vec![],
            severity: None,
            project: Some("test-project".to_string()),
            tags: vec![],
            expires: Some("2027-01-01".to_string()),
            key: None,
            falsifier: None,
            check: None,
        }
    }

    #[test]
    fn falls_back_to_plain_search_when_no_model_is_present() {
        // Named after the defect it prevents: a build with the feature
        // compiled in, but no model files at the resolved directory, must
        // still answer with exactly `search`'s own hits - never an empty
        // result, never a panic, never a wrong (garbage-scored) result.
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report("r1", "the bbq recipe lives here")).unwrap();

        let empty_model_dir = tempfile::tempdir().unwrap(); // no model files at all
        let vectors_dir = tempfile::tempdir().unwrap();
        let vectors_path = vectors_dir.path().join("v.db");

        let plain = search(&db, "bbq");
        let best_effort = search_best_effort(&db, &vectors_path, Some(empty_model_dir.path()), "bbq");
        assert_eq!(best_effort.iter().map(|h| &h.id).collect::<Vec<_>>(), plain.iter().map(|h| &h.id).collect::<Vec<_>>());
        assert_eq!(best_effort.len(), 1, "the fixture's own text match must still be found");
    }

    #[test]
    fn refuses_a_vector_sidecar_with_a_mismatched_model_id() {
        // Named after the defect CONTRACT.md calls out directly: "een
        // verouderde vectorenset wordt herkend in plaats van stil verkeerde
        // antwoorden te geven". A sidecar stamped with a DIFFERENT model_id
        // must never be trusted for scoring, even though the model
        // directory itself looks completely valid (every required filename
        // present) - this is checked before the embedder ever loads, so no
        // real ONNX model is touched by this test at all.
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report("r1", "the office thermostat notes")).unwrap();
        // A second item this test's query would only ever find through a
        // (wrongly trusted) semantic hit, never through plain text overlap.
        store::declare(&mut db, "s", "l", "a", &report("r2", "a completely unrelated fact about zebras")).unwrap();

        let model_dir = tempfile::tempdir().unwrap();
        for f in crate::semantic_paths::MODEL_FILES {
            std::fs::write(model_dir.path().join(f), "not a real model, presence check only").unwrap();
        }
        let vectors_dir = tempfile::tempdir().unwrap();
        let vectors_path = vectors_dir.path().join("v.db");
        {
            let vs = crate::vectors::VectorStore::open(&vectors_path).unwrap();
            vs.set_model_id("some-other-model@v0").unwrap(); // deliberately wrong
        }

        let plain = search(&db, "thermostat");
        let best_effort = search_best_effort(&db, &vectors_path, Some(model_dir.path()), "thermostat");
        assert_eq!(
            best_effort.iter().map(|h| &h.id).collect::<Vec<_>>(),
            plain.iter().map(|h| &h.id).collect::<Vec<_>>(),
            "a mismatched model_id must fall back to plain text hits only, never add an untrustworthy semantic hit"
        );
    }

    /// THE DEFECT THIS PREVENTS (GAP 1): the MCP server keeps ONE cache slot
    /// for the life of the process and passes the SAME `&mut
    /// Option<Embedder>` to every `lookup` call - a second call sharing that
    /// slot must behave exactly like the first, never fight over it, corrupt
    /// it, or diverge in its answer. This reuses the same hermetic
    /// mismatched-sidecar fixture the test above does, so - like every other
    /// test in this module - it never touches a real ONNX model.
    ///
    /// What this does NOT prove: that a SUCCESSFUL load only happens once.
    /// Proving that would need the cache to actually hold `Some(Embedder)`
    /// at some point, which needs a real ONNX model file loaded through
    /// `fastembed`/`ort` - exactly what this module's own doc comment rules
    /// out on purpose ("fast and hermetic ... never loading the real ONNX
    /// model"), and this workspace ships no such fixture (nor should it: the
    /// model is a multi-megabyte per-user download, not test data). The
    /// "loaded at most once" guarantee instead follows from
    /// `search_best_effort_cached`'s own body, which a reader can check
    /// directly: the ONLY call to `embed::Embedder::load` in that function
    /// sits behind `if embedder_cache.is_none()`, so a cache already holding
    /// `Some(_)` can never reach it - the same "read the one place that
    /// decides it" proof this workspace already relies on for
    /// `Kind::can_fire` and for `decay::is_stale`.
    #[test]
    fn search_best_effort_cached_reuses_the_same_cache_slot_across_two_calls() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report("r1", "the office thermostat notes")).unwrap();
        store::declare(&mut db, "s", "l", "a", &report("r2", "a completely unrelated fact about zebras")).unwrap();

        let model_dir = tempfile::tempdir().unwrap();
        for f in crate::semantic_paths::MODEL_FILES {
            std::fs::write(model_dir.path().join(f), "not a real model, presence check only").unwrap();
        }
        let vectors_dir = tempfile::tempdir().unwrap();
        let vectors_path = vectors_dir.path().join("v.db");
        {
            let vs = crate::vectors::VectorStore::open(&vectors_path).unwrap();
            vs.set_model_id("some-other-model@v0").unwrap(); // deliberately wrong, same as above
        }

        let plain = search(&db, "thermostat");
        let mut cache = None;
        let first = search_best_effort_cached(&db, &vectors_path, Some(model_dir.path()), "thermostat", &mut cache);
        let second = search_best_effort_cached(&db, &vectors_path, Some(model_dir.path()), "thermostat", &mut cache);

        assert_eq!(first.iter().map(|h| &h.id).collect::<Vec<_>>(), plain.iter().map(|h| &h.id).collect::<Vec<_>>());
        assert_eq!(
            second.iter().map(|h| &h.id).collect::<Vec<_>>(),
            first.iter().map(|h| &h.id).collect::<Vec<_>>(),
            "a second call sharing the same cache slot must answer exactly like the first"
        );
        assert!(cache.is_none(), "a sidecar this function never trusts must never populate the embedder cache either");
    }

    // -------------------------------------------------------------- expiry

    /// `subject` is not decoration. Two of these live in one store in the
    /// expiry test, and under `--features semantic` the write gate's
    /// near-duplicate check compares MEANING, not the id woven into the
    /// sentence - "a settled report about stale-1" and "... about fresh-1"
    /// read as the same fact and the second declare is refused. Each caller
    /// says what its report is actually about; "settled report" stays in the
    /// text so one query still reaches them all.
    fn report_expiring(id: &str, expires: &str, subject: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: format!("a settled report: {subject}"),
            bindings: vec![],
            severity: None,
            project: Some("test-project".to_string()),
            tags: vec![],
            expires: Some(expires.to_string()),
            key: None,
            falsifier: None,
            check: None,
        }
    }

    /// THE DEFECT THIS PREVENTS, found by auditing the migration
    /// (2026-08-03). `expires` was written and validated at the write gate
    /// and read by NOTHING: the live store held 169 items with an expiry, 77
    /// already past it, and every one came back from a search ranked exactly
    /// like a current answer. "Old facts coming back" is one of the two
    /// complaints this rebuild exists to answer.
    #[test]
    fn an_expired_report_is_held_back_from_search() {
        let mut db = EventStore::in_memory().unwrap();
        model::store::declare(&mut db, "s", "l", "a", &report_expiring("stale-1", "2020-01-01", "the courier rollout and why it was rolled back")).unwrap();
        model::store::declare(&mut db, "s", "l", "a", &report_expiring("fresh-1", "2099-01-01", "how the NAS replica was sized for the estimator")).unwrap();

        let (hits, withheld) = search_with_expired(&db, "settled report");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["fresh-1"], "the expired one must not come back as current");
        assert_eq!(withheld, 1, "and the caller must be able to say how many were held back");
    }

    /// Held back is not hidden: an expired item is still there whole for
    /// anyone who asks for it by id. Nothing is deleted.
    #[test]
    fn an_expired_item_is_still_readable_by_id() {
        let mut db = EventStore::in_memory().unwrap();
        model::store::declare(&mut db, "s", "l", "a", &report_expiring("stale-2", "2020-01-01", "the abandoned quote-number scheme of last spring")).unwrap();
        let shown = model::store::show(&db, "stale-2").expect("still readable whole");
        assert_eq!(shown.id, "stale-2");
        assert_eq!(shown.expires.as_deref(), Some("2020-01-01"), "and it still says when it expired");
    }

    /// A date this cannot read must never retire anything - guessing what a
    /// malformed date meant is worse than ignoring it.
    #[test]
    fn a_malformed_expiry_expires_nothing() {
        let mut db = EventStore::in_memory().unwrap();
        let mut odd = report_expiring("odd-1", "2020-01-01", "an expiry string nobody can parse");
        odd.expires = Some("soon".to_string());
        // Straight into the log, bypassing the gate's own date validation.
        let body = model::store::canonical_body(&odd).unwrap();
        db.append_event("s", "l", "a", thor_core::event_store::EventKind::FactCreated, "odd-1", None, &body)
            .unwrap();

        let (hits, withheld) = search_with_expired(&db, "settled report");
        assert_eq!(withheld, 0, "an unreadable date retires nothing");
        assert!(hits.iter().any(|h| h.id == "odd-1"));
    }
}
