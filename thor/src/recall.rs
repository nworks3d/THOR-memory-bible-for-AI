use crate::cas::compute_head_sets;
use crate::event_store::{Event, EventKind, EventStore};
use rusqlite::params;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct RecallHit {
    pub entity_id: String,
    pub rev: String,
    pub body: String,
    pub kind: EventKind,
    /// The entity this head belongs to is diverged (more than one contested
    /// head). Surfaced so the courier can stamp the hit.
    pub is_diverged: bool,
    /// FTS5 bm25 rank (lower = better match).
    pub rank: f64,
    /// The fact's effective project (`None` = the global tier). Lets the courier /
    /// CLI label a hit `[global]` or `[proj:<key>]` so the agent knows its scope.
    pub project: Option<String>,
    /// The constraint class of a typed hand-written fact (gotcha / decision /
    /// preference), parsed from its body. None for chunks and untyped notes.
    pub fact_type: Option<crate::repo::FactType>,
    /// True when this hit came from the strict AND pass (every content term of
    /// the query matched) - or from the fused path, which has its own evidence.
    /// False = an OR-fallback hit; the courier applies a coverage gate to those
    /// so a one-word coincidence is never injected as if it were an answer.
    pub matched_and: bool,
}

/// A one-line preview of a fact body for display: whitespace collapsed to single
/// spaces, capped at `max` chars. When the body is longer than `max`, the window
/// is chosen for MAXIMUM DISTINCT query-term coverage (not merely the first
/// match): the measured drift-miss mode was "right hit injected, actionable
/// details cut" - the details often sit past the first matching term. Wide caps
/// (>= 500, the courier's slot 1) may spend their budget on TWO disjoint
/// windows when a second region covers query terms the first one misses.
/// Falls back to head-truncation when no query term is found (or the best
/// window is already the head). `query` may be empty (pure head-truncation).
pub fn snippet(body: &str, max: usize, query: &str) -> String {
    // Window selection and display operate on the CONTENT only: the trailing
    // [memory/...] / [repo file ...] footer is metadata. A query term matching
    // inside it (tags, fires-when vocabulary, the literal word 'project', a
    // heading crumb) used to drag the window to the tail - at narrow caps the
    // served snippet could be bracket syntax instead of the answer. The footer
    // stays FTS-findable; it is only excluded from what the agent reads.
    let body = crate::footer::strip(body);
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = collapsed.chars().collect();
    let total = chars.len();
    if total <= max {
        return collapsed;
    }
    let lower = collapsed.to_lowercase();
    // All char-index occurrences per distinct (non-stopword) query term. Byte
    // offsets are mapped to char indexes of `lower`, then clamped into the
    // `chars` domain below (lowercasing can expand a codepoint, so the two
    // strings may differ in length - a mismatch must never panic the hook path).
    let terms: Vec<String> = {
        let mut seen = HashSet::new();
        query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.chars().count() >= 2)
            .map(|t| t.to_lowercase())
            .filter(|t| !STOPWORDS.contains(&t.as_str()))
            .filter(|t| seen.insert(t.clone()))
            .collect()
    };
    let positions: Vec<Vec<usize>> = terms
        .iter()
        .map(|t| {
            lower
                .match_indices(t.as_str())
                .take(16)
                .map(|(byte, _)| lower[..byte].chars().count().min(total))
                .collect()
        })
        .collect();

    let lead = max / 5;
    // Candidate window starts: the head, plus one window anchored before each
    // term occurrence. Score = how many DISTINCT terms fall inside.
    let window = |width: usize, start: usize, exclude: Option<(usize, usize)>| -> usize {
        let end = start + width;
        positions
            .iter()
            .filter(|occ| {
                occ.iter().any(|&p| {
                    p >= start
                        && p < end
                        && exclude.map_or(true, |(xs, xe)| p < xs || p >= xe)
                })
            })
            .count()
    };
    let candidates = |width: usize| -> Vec<usize> {
        let mut c: Vec<usize> = positions
            .iter()
            .flatten()
            .map(|&p| p.saturating_sub(lead).min(total.saturating_sub(width)))
            .collect();
        c.push(0);
        c.sort_unstable();
        c.dedup();
        c
    };
    let best = |width: usize, exclude: Option<(usize, usize)>| -> (usize, usize) {
        candidates(width)
            .into_iter()
            .map(|s| (window(width, s, exclude), s))
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1))) // ties -> earliest
            .map(|(score, s)| (s, score))
            .unwrap_or((0, 0))
    };

    let cut = |start: usize, width: usize| -> String {
        let start = start.min(total);
        let end = (start + width).min(total);
        let mut mid: String = chars[start..end].iter().collect();
        let pre = if start > 0 {
            mid = mid.trim_start().to_string();
            "..."
        } else {
            ""
        };
        let suf = if end < total {
            mid = mid.trim_end().to_string();
            "..."
        } else {
            ""
        };
        format!("{}{}{}", pre, mid, suf)
    };

    let (start1, score1) = best(max, None);
    if score1 == 0 {
        return cut(0, max); // no term found anywhere: head-truncation
    }
    // Wide budget: try splitting into two disjoint half-windows when a second
    // region contributes terms the first misses. Only when it genuinely adds
    // coverage - otherwise one contiguous window reads better.
    if max >= 500 {
        let half = max / 2 - 4; // room for the " ... " joint
        let (h1, s1) = best(half, None);
        let (h2, s2) = best(half, Some((h1, h1 + half)));
        if s2 > 0 && s1 + s2 > score1 {
            let (first, second) = if h1 <= h2 { (h1, h2) } else { (h2, h1) };
            let a = cut(first, half);
            let b = cut(second, half);
            let b = b.strip_prefix("...").unwrap_or(&b).trim_start().to_string();
            return format!("{} ... {}", a.trim_end_matches("...").trim_end(), b);
        }
    }
    cut(start1, max)
}

/// Once the best head is found, stop taking hits whose bm25 rank is much weaker
/// than it (FTS5 bm25 is negative; more-negative = stronger). A relevance floor
/// keeps the 3 injection slots from filling with weakly-matching junk that reads
/// as a non-answer. Conservative (only trims clearly-weaker trailing hits; the
/// top hit is always kept).
const RELEVANCE_FLOOR_FRAC: f64 = 0.3;
/// Length of the normalized body prefix used to collapse near-duplicate hits
/// (imported doc-chunks of one fact otherwise burn all 3 slots on one answer).
///
/// NOTE: a slot-quality experiment (max-1-chunk-per-source-file dedup + a reserved
/// memory/doc slot) was measured with a blind A/B/C sim over 200 coverage + 73 drift
/// scenarios and REVERTED: it cost ~2 points of code coverage (THOR's strongest
/// category - consecutive chunks of one file are often both needed) for no drift gain
/// (drift recall is already memory-heavy). The near-duplicate collapse below stayed.
const DEDUP_PREFIX_CHARS: usize = 120;

/// Common English + Dutch function words. Dropped from the FTS query so a sum of
/// stopword matches cannot outrank the one rare, content-bearing term. Never
/// applied if it would empty the query (see `content_tokens`).
const STOPWORDS: &[&str] = &[
    // English
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "at", "for", "with", "is", "are", "was",
    "were", "be", "been", "do", "did", "does", "how", "what", "why", "when", "where", "which",
    "that", "this", "it", "we", "you", "about", "from", "have", "has", "had", "not", "no", "our",
    "my", "your", "as", "by", "so", "if", "up", "out", "did", "was",
    // Dutch
    "de", "het", "een", "en", "of", "van", "voor", "met", "zijn", "was", "waren", "hoe", "wat",
    "waarom", "wanneer", "waar", "welke", "dat", "dit", "ook", "er", "al", "nog", "dan", "dus",
    "maar", "die", "naar", "niet", "geen", "ons", "mijn", "jij", "over", "om", "te", "op", "aan",
];

/// Alphanumeric tokens (>= 2 chars), FTS5-escaped (embedded quotes doubled).
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .map(|t| t.replace('"', "\"\""))
        .collect()
}

/// Content tokens: stopwords removed, capped at 32. Falls back to the full token
/// list if stopword removal would empty the query (a prompt that is ALL stopwords
/// still gets a best-effort search rather than nothing).
fn content_tokens(text: &str) -> Vec<String> {
    let all = tokens(text);
    if all.is_empty() {
        return all;
    }
    let filtered: Vec<String> = all
        .iter()
        .filter(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
        .cloned()
        .collect();
    let mut chosen = if filtered.is_empty() { all } else { filtered };
    chosen.truncate(32); // cap query size; the prompt is truncated upstream anyway
    chosen
}

/// Fold common Latin diacritics to ASCII, mirroring what FTS5's default
/// unicode61 tokenizer (remove_diacritics=1) does at index time. Without this,
/// a hit FTS legitimately matched on "privé" vs "prive" would be miscounted by
/// the coverage gate and silenced - a real regression for a Dutch/EN store.
fn fold_diacritics(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'ç' => 'c',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            other => other,
        })
        .collect()
}

/// Coverage gate for OR-fallback hits: does `body` literally contain (word-
/// boundary, case-insensitive, diacritics folded like FTS unicode61) at least
/// min(2, #query-content-terms) DISTINCT content terms of `query`? A hit that
/// matches only one word of a multi-word question is a coincidence, not an
/// answer - the courier stays silent instead of injecting it confidently.
pub fn covers_query(body: &str, query: &str) -> bool {
    // Gate on CONTENT, not metadata: every memory footer contains the literal
    // word 'project' (and tags/fires-when vocabulary), so an unstripped body
    // hands any prompt mentioning 'project' a free coverage term - measured as
    // real courier noise on the silence set (n02). Author-declared triggers
    // have their own dedicated path (trigger_rescues); they do not need to
    // leak through this gate.
    let body = crate::footer::strip(body);
    // NO stopword fallback here (unlike content_tokens' best-effort search): a
    // stopword-only query has no content terms and never covers - with the
    // fallback, "wat is dat dan" would count its own stopwords as content and
    // any body containing two of them would pass the gate as a confident answer.
    let qterms: HashSet<String> = tokens(query)
        .iter()
        .filter(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
        .map(|t| fold_diacritics(&t.to_lowercase()))
        .collect();
    if qterms.is_empty() {
        return false;
    }
    let required = 2.min(qterms.len());
    let body_tokens: HashSet<String> = body
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .map(|t| fold_diacritics(&t.to_lowercase()))
        .collect();
    qterms.iter().filter(|t| body_tokens.contains(*t)).count() >= required
}

/// True when the query has at least one non-stopword token. The courier gates
/// recall on this: an all-stopword prompt ("wat is dat dan") carries no content
/// to match, and content_tokens' best-effort fallback would otherwise let its
/// stopwords earn matched_and via the AND query and bypass the coverage gate.
pub fn has_content_terms(query: &str) -> bool {
    tokens(query).iter().any(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
}

/// What KIND of source a hit is, from a pure entity-id parse: a hand-written
/// memory, a prose/doc chunk, or a code chunk. The fused ranker uses this for a
/// small query-routed prior: code chunks are long, numerous and identifier-dense,
/// so on a knowledge-phrased question they crowd the few hand-written facts out
/// of the top slots on raw bm25 - the measured root of the dual-written loss.
#[cfg(feature = "semantic")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceClass {
    Memory,
    Doc,
    Code,
}

/// Extensions whose chunks are prose, not code. Everything else under a chunk id
/// counts as code (the conservative default: code never gets a knowledge boost).
#[cfg(feature = "semantic")]
const DOC_EXTS: &[&str] = &["md", "markdown", "txt", "rst", "adoc", "org"];

#[cfg(feature = "semantic")]
fn source_class(entity_id: &str) -> SourceClass {
    if !crate::repo::is_chunk_id(entity_id) {
        return SourceClass::Memory;
    }
    // `<project>:<rel>#<n>` - classify by the rel path's extension.
    let rel = entity_id.split_once(':').map(|(_, r)| r).unwrap_or(entity_id);
    let rel = rel.rsplit_once('#').map(|(r, _)| r).unwrap_or(rel);
    let ext = std::path::Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if DOC_EXTS.contains(&ext.as_str()) {
        SourceClass::Doc
    } else {
        SourceClass::Code
    }
}

/// How a query is phrased, from its surface form alone: identifier-shaped tokens
/// route to code, decision/constraint vocabulary routes to knowledge, anything
/// else (or both at once) stays neutral. Routing decides which class prior may
/// apply - a code-routed query gets NO prior at all, which is what keeps the
/// code categories' ranking byte-identical (the no-regression gate).
#[cfg(feature = "semantic")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryRoute {
    Code,
    Knowledge,
    Neutral,
}

/// Decision/constraint vocabulary (EN + NL), matched on word boundaries over the
/// lowercased query. Deliberately narrow: generic verbs ("use", "werkt") must
/// not route ordinary code questions to knowledge.
#[cfg(feature = "semantic")]
const KNOWLEDGE_WORDS: &[&str] = &[
    "beslissing", "besloten", "besluit", "beslist", "afspraak", "afgesproken", "regel",
    "voorkeur", "werkvoorkeur", "gotcha", "waarom", "conventie", "beleid", "werkwijze",
    "decision", "decided", "agreed", "agreement", "rule", "preference", "convention",
    "policy", "why", "rationale", "constraint",
];

#[cfg(feature = "semantic")]
fn route_query(query: &str) -> QueryRoute {
    let lower = query.to_lowercase();
    let words: HashSet<&str> =
        lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let knowledge = KNOWLEDGE_WORDS.iter().any(|w| words.contains(w));
    // Identifier-shaped tokens: snake_case, ::-paths, call syntax, file names or
    // path separators - the surface forms of a question ABOUT code.
    let code = query.split_whitespace().any(|t| {
        let core = t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != ':' && c != '.' && c != '/');
        core.contains('_')
            || core.contains("::")
            || t.contains("()")
            || core.contains('/')
            || std::path::Path::new(core)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphabetic()) && core.contains('.'))
    });
    match (knowledge, code) {
        (true, false) => QueryRoute::Knowledge,
        (false, true) => QueryRoute::Code,
        _ => QueryRoute::Neutral,
    }
}

/// The query-routed class delta added to a candidate's fused score. Small on
/// purpose: it breaks the near-ties where a memory sits just below a wall of
/// same-topic chunks, and must never lift an unrelated memory over a strong
/// lexical/semantic match (bm_norm spans 1.0, cosine up to ~lambda).
#[cfg(feature = "semantic")]
fn class_delta(route: QueryRoute, class: SourceClass) -> f64 {
    match (route, class) {
        (QueryRoute::Knowledge, SourceClass::Memory) => 0.30,
        (QueryRoute::Knowledge, SourceClass::Doc) => 0.15,
        (QueryRoute::Neutral, SourceClass::Memory) => 0.10,
        // Code-routed queries get no delta anywhere: the code categories'
        // ranking stays untouched by construction.
        _ => 0.0,
    }
}

/// Term-coverage + proximity bonus over a candidate's body: a hit containing
/// ALL the query's content terms - tightly - outranks a long chunk matching one
/// common term often. This is the cheap stand-in for a cross-encoder: exactly
/// the failure signature (single-term tf winning over full-question matches)
/// behind clean-note misses. Quadratic in coverage so partial matches gain
/// little; the tight bonus only fires on FULL coverage within a small window.
#[cfg(feature = "semantic")]
const COVERAGE_WEIGHT: f64 = 0.30;
#[cfg(feature = "semantic")]
const TIGHT_BONUS: f64 = 0.15;
#[cfg(feature = "semantic")]
const TIGHT_WINDOW_CHARS: usize = 240;

/// Author-declared firing vocabulary: a fact stored with `triggers` carries a
/// `fires-when:` footer field naming the task words that should surface it.
/// A query hitting those words is a DELIBERATE match the author asked for -
/// scored over the TRIGGER terms (matched/declared), not the query terms, so
/// a long task prompt touching two of three declared triggers still fires
/// strongly. Bounded like every other delta: it breaks near-ties, it never
/// lifts an unrelated fact over a strong lexical/semantic match. Facts
/// without the field always score 0 - pre-existing ranking is untouched by
/// construction. NOT semantic-gated: the boost is pure lexical work and must
/// fire identically on the bm25-only path (non-semantic builds, the courier's
/// no-sidecar fallback).
const TRIGGER_WEIGHT: f64 = 0.35;

/// A trigger term shorter than this is too generic to count as evidence in
/// either match mode. It still occupies its slot in the DENOMINATOR: filtering
/// it out entirely used to collapse `fires-when: docker cp` into the 1-element
/// list ["docker"], which then fired at FULL weight on any prompt mentioning
/// docker - measured as real courier noise on the silence set (n02).
const MIN_TRIGGER_TERM_LEN: usize = 3;
/// Stricter floor for SUBSTRING-CONTAINMENT matching (identifier/path-shaped
/// terms): containment is inherently more permissive than token equality - a
/// short structured fragment like "a.b" appears by coincidence inside URLs and
/// version strings. Load-bearing, not decorative.
const MIN_IDENTIFIER_PHRASE_LEN: usize = 5;

/// True when `s` looks like an identifier or path rather than a plain word: a
/// `::` scope, a slash/dot/underscore/hyphen-separated form, or a genuine
/// camelCase transition (a lowercase char immediately followed by an uppercase
/// one - "Docker" has none, "MatrixCache"/"fooBar" do). Such terms are matched
/// by case-folded substring containment on the RAW query (the guard's anchor
/// idiom), because tokens() shreds them into fragments that exact token
/// equality can never reassemble.
fn is_identifier_shaped(s: &str) -> bool {
    if s.contains("::") || s.contains('/') || s.contains('.') || s.contains('_') || s.contains('-') {
        return true;
    }
    s.chars().zip(s.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase())
}

/// Matched trigger terms, split by evidence kind: (generic word matches,
/// identifier/path matches, declared term count). Shared by the scoring bonus
/// and the below-floor rescue so both read the same match semantics.
fn trigger_evidence(body: &str, query: &str, qterms: &[String]) -> (usize, usize, usize) {
    if qterms.is_empty() {
        return (0, 0, 0);
    }
    let Some(fires) = crate::footer::fires_when(body) else {
        return (0, 0, 0);
    };
    let tterms: Vec<String> =
        fires.split_whitespace().map(|t| fold_diacritics(&t.to_lowercase())).collect();
    if tterms.is_empty() {
        return (0, 0, 0);
    }
    let folded_query = fold_diacritics(&query.to_lowercase());
    let (mut generic, mut identifier) = (0usize, 0usize);
    for t in &tterms {
        if t.chars().count() < MIN_TRIGGER_TERM_LEN {
            continue; // too generic to count - but stays in the denominator
        }
        if is_identifier_shaped(t) {
            if t.chars().count() >= MIN_IDENTIFIER_PHRASE_LEN && folded_query.contains(t.as_str()) {
                identifier += 1;
            }
        } else if qterms.iter().any(|q| q == t) {
            generic += 1;
        }
    }
    (generic, identifier, tterms.len())
}

/// Absolute-evidence scoring, NOT matched/declared ratio. Under sparse tagging
/// the ratio worked; once the whole typed population carries triggers (the v4
/// sweep), ratio rewards SHORT lists - a bystander matching 1 of its 2 terms
/// (0.5) outscored the actual preventer matching 2 of its 5 (0.4), measured as
/// 3 lost scenarios on the live replay. Matched COUNT is what discriminates
/// the on-topic fact from its topical siblings; identifier/path matches count
/// double (a declared file or scoped name appearing verbatim in the prompt is
/// the strongest firing evidence there is). Bounded at 3 points = full weight.
fn trigger_bonus(body: &str, query: &str, qterms: &[String]) -> f64 {
    let (generic, identifier, declared) = trigger_evidence(body, query, qterms);
    if declared == 0 {
        return 0.0;
    }
    let points = (generic + 2 * identifier).min(3);
    TRIGGER_WEIGHT * (points as f64 / 3.0)
}

/// Below-floor rescue takes SPECIFIC evidence, not any partial ratio: at least
/// two matched trigger terms, or a single match that is itself an
/// identifier/path phrase (a declared file or scoped name appearing verbatim
/// in the prompt). One generic word ('docker') never lifts a fact past the
/// relevance floor - measured as real courier noise on the silence set (n02)
/// when it could. Rescoring within the pool still uses the full partial ratio;
/// only the RESCUE is gated.
fn trigger_rescues(body: &str, query: &str, qterms: &[String]) -> bool {
    let (generic, identifier, _) = trigger_evidence(body, query, qterms);
    identifier >= 1 || generic + identifier >= 2
}

/// Author-declared firing evidence for the courier's coverage gate: the same
/// specificity rule as the below-floor rescue, computed from the raw query.
/// covers_query gates on CONTENT (footer stripped), so declared triggers -
/// which live in the footer - need this dedicated door instead of leaking
/// through the coverage count.
pub fn trigger_authorizes(body: &str, query: &str) -> bool {
    let qterms: Vec<String> = {
        let mut seen = HashSet::new();
        content_tokens(query)
            .iter()
            .map(|t| fold_diacritics(&t.to_lowercase()))
            .filter(|t| seen.insert(t.clone()))
            .collect()
    };
    trigger_rescues(body, query, &qterms)
}

/// Bounded reward when a candidate body contains an exact identifier/path
/// phrase from the query (`MatrixCache::ensure`, `server/lib/be-postal.json`):
/// for a question ABOUT a specific symbol, the defining body beats one that
/// merely shares the generic fragments bm25 sees. Folded into coverage_bonus -
/// the ONE lexical-evidence bonus concept, not a second nudge. 0.20 validated
/// on the 52-query battery (no category regressed, @5 held 71-73% across
/// lambdas) and the committed drift corpus (catch unchanged, noise unchanged);
/// re-sweep together with the serving-side footer-strip round before raising.
#[cfg(feature = "semantic")]
const IDENTIFIER_BONUS: f64 = 0.20;

/// Identifier/path-shaped tokens of the raw query, folded, deduped, floor-gated.
#[cfg(feature = "semantic")]
fn identifier_phrases(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && !"_:./-".contains(c)))
        .map(|t| fold_diacritics(&t.to_lowercase()))
        .filter(|t| t.chars().count() >= MIN_IDENTIFIER_PHRASE_LEN && is_identifier_shaped(t))
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

#[cfg(feature = "semantic")]
fn coverage_bonus(body: &str, qterms: &[String], id_phrases: &[String]) -> f64 {
    if qterms.is_empty() {
        return 0.0;
    }
    let hay = fold_diacritics(&body.to_lowercase());
    // First-occurrence positions of each distinct term (word-boundary-free
    // substring find is fine here: this is a tie-break bonus, not the gate).
    let mut found = 0usize;
    let mut min_pos = usize::MAX;
    let mut max_pos = 0usize;
    for t in qterms {
        if let Some(p) = hay.find(t.as_str()) {
            found += 1;
            min_pos = min_pos.min(p);
            max_pos = max_pos.max(p + t.len());
        }
    }
    let id_hit = id_phrases.iter().any(|p| hay.contains(p.as_str()));
    if found == 0 {
        // An exact identifier phrase is stronger evidence than any single
        // fragment, so it stands on its own even without term coverage.
        return if id_hit { IDENTIFIER_BONUS } else { 0.0 };
    }
    let cov = found as f64 / qterms.len() as f64;
    let mut bonus = COVERAGE_WEIGHT * cov * cov;
    if found == qterms.len() && (max_pos - min_pos) <= TIGHT_WINDOW_CHARS {
        bonus += TIGHT_BONUS;
    }
    if id_hit {
        bonus += IDENTIFIER_BONUS;
    }
    bonus
}

/// The normalized body prefix used for near-duplicate detection, shared by
/// recall's collapse and the MCP remember refusal: whitespace-collapsed,
/// lowercased, capped at DEDUP_PREFIX_CHARS - with any trailing
/// `[memory/... ]` / `[repo file ...]` footer stripped first, so a stored
/// typed fact (whose footer bleeds into a short body's prefix) still matches
/// the same body offered without a footer.
pub fn dedup_prefix(body: &str) -> String {
    strip_footer(body)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .chars()
        .take(DEDUP_PREFIX_CHARS)
        .collect()
}

/// Strip a trailing single-line `[...]` metadata footer (shim: the footer
/// format and its parsers live together in crate::footer).
fn strip_footer(body: &str) -> &str {
    crate::footer::strip(body)
}

/// Build a safe FTS5 MATCH query (content tokens OR-ed, each double-quoted so
/// FTS5 can never read one as an operator/column/prefix). Returns None when
/// there is nothing searchable, so the caller skips recall entirely instead of
/// running a degenerate query.
pub fn fts_query(text: &str) -> Option<String> {
    let terms = content_tokens(text);
    if terms.is_empty() {
        None
    } else {
        Some(terms.iter().map(|t| format!("\"{}\"", t)).collect::<Vec<_>>().join(" OR "))
    }
}

/// The strict AND form of the query: prefer memories that match the WHOLE
/// question over a single-word coincidence. Only meaningful with >= 2 tokens;
/// None otherwise (the caller then uses only the OR query).
fn fts_query_and(text: &str) -> Option<String> {
    let terms = content_tokens(text);
    if terms.len() < 2 {
        None
    } else {
        Some(terms.iter().map(|t| format!("\"{}\"", t)).collect::<Vec<_>>().join(" AND "))
    }
}

/// Walk one FTS MATCH in rank order, keeping only current heads, applying the
/// relevance floor and near-duplicate collapse, until `limit` heads are kept.
/// There is NO fixed candidate window: iterating lazily can never let the many
/// superseded revs of one frequently-revised entity starve its current head.
/// How many rank-ordered rows past the relevance floor are still scanned for
/// author-declared trigger carriers before giving up (bounds the extra cost on
/// a huge OR cursor).
const FLOOR_TRIGGER_SCAN_CAP: usize = 200;

fn collect_heads(
    store: &EventStore,
    fts: &str,
    by_seq: &HashMap<i64, &Event>,
    heads: &HashMap<String, crate::cas::HeadSet>,
    filter: &ScopeFilter,
    query: &str,
    qterms: &[String],
    limit: usize,
) -> Vec<RecallHit> {
    let conn = store.conn();
    let mut stmt = match conn.prepare("SELECT rowid, rank FROM event_fts WHERE event_fts MATCH ? ORDER BY rank") {
        Ok(stmt) => stmt,
        Err(_) => return vec![], // fail-soft
    };
    let rows = match stmt.query_map(params![fts], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))) {
        Ok(rows) => rows,
        Err(_) => return vec![], // malformed MATCH etc: fail-soft
    };

    let mut hits: Vec<RecallHit> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_prefixes: Vec<String> = Vec::new();
    let mut best_rank: Option<f64> = None;
    let mut below_floor = 0usize;
    for row in rows {
        let (seq, rank) = match row {
            Ok(pair) => pair,
            Err(_) => break, // fail-soft: return what we have
        };
        let ev = match by_seq.get(&seq) {
            Some(e) => *e,
            None => continue,
        };
        let head_set = match heads.get(&ev.entity_id) {
            Some(h) => h,
            None => continue,
        };
        if !head_set.heads.contains(&ev.this_hash) {
            continue; // drop hits on revs that are no longer a current head
        }
        if !filter.keep(ev) {
            continue; // out of project scope, or a retracted (non-live) head
        }
        if !seen.insert(ev.this_hash.clone()) {
            continue;
        }
        // Relevance floor: once we have the strongest head, hits far weaker
        // than it are junk (rows are rank-sorted, so everything after is
        // weaker too) - EXCEPT an author-declared trigger match: a fires-when
        // fact competes from below the floor, because being findable on weak
        // lexical evidence is exactly what declaring triggers is for. The
        // continued scan is bounded (FLOOR_TRIGGER_SCAN_CAP rows).
        if let Some(best) = best_rank {
            if rank > best * RELEVANCE_FLOOR_FRAC {
                below_floor += 1;
                if below_floor > FLOOR_TRIGGER_SCAN_CAP {
                    break;
                }
                if !trigger_rescues(&ev.body, query, qterms) {
                    continue;
                }
            }
        }
        // Near-duplicate collapse: skip a body whose normalized prefix duplicates
        // an already-kept hit, freeing the slot for distinct content.
        let prefix = dedup_prefix(&ev.body);
        if seen_prefixes.iter().any(|p| *p == prefix) {
            continue;
        }
        if best_rank.is_none() {
            best_rank = Some(rank);
        }
        seen_prefixes.push(prefix);
        hits.push(RecallHit {
            entity_id: ev.entity_id.clone(),
            rev: ev.this_hash.clone(),
            body: ev.body.clone(),
            kind: ev.kind,
            is_diverged: head_set.is_diverged,
            rank,
            project: filter.projects.get(&ev.entity_id).cloned().flatten(),
            fact_type: crate::repo::fact_type(&ev.body),
            matched_and: false, // stamped true by the caller for the strict pass
        });
        if hits.len() >= limit {
            break;
        }
    }
    hits
}

/// Recall the current-head facts whose body best matches `query`, most-relevant
/// first, at most `limit`. Only CURRENT HEADS are returned: an FTS hit on a rev
/// that a later mutation replaced (no longer a head) is skipped, so recall
/// always reflects the authoritative head projection, never stale content.
/// Fail-soft: a malformed FTS query or a query error yields an empty result,
/// never an error the courier would have to handle.
/// What recall may surface. The default (derived from the session cwd) is the
/// CURRENT project + the always-in-scope global tier; `all_projects` widens to
/// everything (an explicit cross-project search). A `None` project with
/// `all_projects=false` = global-only (a scratch dir with no project).
#[derive(Debug, Clone, Default)]
pub struct RecallScope {
    pub project: Option<String>,
    pub all_projects: bool,
}

impl RecallScope {
    /// No scoping: every project + global. Used by tests, the eval harness, and the
    /// `recall`/`recall_fused` compatibility wrappers.
    pub fn everything() -> Self {
        RecallScope { project: None, all_projects: true }
    }
    /// Scope to a specific project (its facts + the global tier); `None` = global-only.
    pub fn current(key: Option<String>) -> Self {
        RecallScope { project: key, all_projects: false }
    }
    /// Whether a fact with the given EFFECTIVE project (None = global) is in scope.
    pub fn allows(&self, effective: Option<&str>) -> bool {
        if self.all_projects {
            return true;
        }
        if crate::repo::is_global(effective) {
            return true; // the global tier always surfaces
        }
        match &self.project {
            Some(cur) => effective == Some(cur.as_str()),
            None => false, // no current project -> global only
        }
    }
}

/// Bundles the per-entity effective-project map with the scope, plus the "a
/// retracted head is not live" rule, so every candidate path filters identically
/// with a single `keep(ev)` check. `memories_only` additionally drops repo
/// chunks, for callers (the file-touch guard) where a code chunk is by
/// construction noise - the empirically-verified failure of raw tool-call
/// recall is chunks crowding out the one governing memory.
struct ScopeFilter<'a> {
    projects: &'a HashMap<String, Option<String>>,
    scope: &'a RecallScope,
    memories_only: bool,
}

impl ScopeFilter<'_> {
    fn keep(&self, ev: &Event) -> bool {
        if matches!(ev.kind, EventKind::FactRetracted) {
            return false; // a retracted head is not live: never surface it
        }
        if self.memories_only && crate::repo::is_chunk_id(&ev.entity_id) {
            return false; // chunks never spend a slot in a memories-only recall
        }
        let effective = self.projects.get(&ev.entity_id).and_then(|o| o.as_deref());
        self.scope.allows(effective)
    }
}

/// Lexical bm25 recall over current heads, scoped to `scope` (global tier always;
/// other projects only when matched, or under `all_projects`).
pub fn recall_scoped(
    store: &EventStore,
    query: &str,
    limit: usize,
    scope: &RecallScope,
) -> anyhow::Result<Vec<RecallHit>> {
    recall_scoped_inner(store, query, limit, scope, false)
}

/// Memories-only bm25 recall: identical to `recall_scoped` but repo chunks never
/// occupy a slot. For the file-touch guard, where only hand-written memories
/// (gotchas/decisions about the file) are signal and chunks are always noise.
pub fn recall_memories_scoped(
    store: &EventStore,
    query: &str,
    limit: usize,
    scope: &RecallScope,
) -> anyhow::Result<Vec<RecallHit>> {
    recall_scoped_inner(store, query, limit, scope, true)
}

fn recall_scoped_inner(
    store: &EventStore,
    query: &str,
    limit: usize,
    scope: &RecallScope,
    memories_only: bool,
) -> anyhow::Result<Vec<RecallHit>> {
    if limit == 0 {
        return Ok(vec![]);
    }
    let or_query = match fts_query(query) {
        Some(q) => q,
        None => return Ok(vec![]),
    };
    let and_query = fts_query_and(query);

    // Head projection. M1a folds the whole log per call (O(n)); a materialized
    // heads table updated in the append tx is the M2 optimization. Loaded before
    // the FTS cursor so the (owned) events/heads outlive the lazy iteration. The
    // project fold rides the SAME pass over the events.
    let events = store.get_all_events()?;
    let by_seq: HashMap<i64, &Event> = events.iter().map(|e| (e.seq, e)).collect();
    let heads = compute_head_sets(&events);
    let projects = crate::cas::compute_projects(&events);
    let filter = ScopeFilter { projects: &projects, scope, memories_only };

    // AND-first: prefer memories that match the WHOLE question over a single-word
    // coincidence; fall back to the OR query only when the strict pass finds no
    // head. collect_heads applies the relevance floor + near-duplicate collapse.
    // The pool is a little wider than `limit` so an author-declared trigger hit
    // sitting just below the cut can be boosted into it (see trigger_boost).
    let qterms: Vec<String> = {
        let mut seen = HashSet::new();
        content_tokens(query)
            .iter()
            .map(|t| fold_diacritics(&t.to_lowercase()))
            .filter(|t| seen.insert(t.clone()))
            .collect()
    };
    let fetch = limit.saturating_add(TRIGGER_POOL_EXTRA);
    if let Some(aq) = and_query {
        let mut strict = collect_heads(store, &aq, &by_seq, &heads, &filter, query, &qterms, fetch);
        if !strict.is_empty() {
            for h in &mut strict {
                h.matched_and = true; // every content term matched: strong evidence
            }
            return Ok(trigger_boost(strict, query, &qterms, limit));
        }
    }
    let pool = collect_heads(store, &or_query, &by_seq, &heads, &filter, query, &qterms, fetch);
    Ok(trigger_boost(pool, query, &qterms, limit))
}

/// Extra heads collected past `limit` purely as trigger-boost headroom.
const TRIGGER_POOL_EXTRA: usize = 8;

/// The lexical-path trigger boost: rescore the collected pool as (min-max
/// normalized bm25) + trigger_bonus and cut to `limit`. When NO collected hit
/// carries a fires-when field, the pool is returned in plain bm25 order
/// truncated to `limit` - byte-identical to the pre-trigger behavior by
/// construction (the wider fetch walks the same rank-ordered cursor).
fn trigger_boost(mut hits: Vec<RecallHit>, query: &str, qterms: &[String], limit: usize) -> Vec<RecallHit> {
    let bonuses: Vec<f64> = hits.iter().map(|h| trigger_bonus(&h.body, query, qterms)).collect();
    if bonuses.iter().all(|b| *b == 0.0) {
        hits.truncate(limit);
        return hits;
    }
    // FTS5 bm25 rank: more negative = stronger. Normalize the pool to [0,1]
    // (strongest = 1.0) so the bounded bonus trades against lexical strength
    // exactly like on the fused path.
    let (rmin, rmax) = hits
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), h| (lo.min(h.rank), hi.max(h.rank)));
    let span = rmax - rmin;
    let mut order: Vec<usize> = (0..hits.len()).collect();
    let score = |i: usize| {
        let norm = if span > 0.0 { (rmax - hits[i].rank) / span } else { 1.0 };
        norm + bonuses[i]
    };
    // stable + deterministic: equal scores keep bm25 order
    order.sort_by(|&a, &b| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
    let mut out: Vec<RecallHit> = Vec::with_capacity(limit.min(hits.len()));
    let mut slots: Vec<Option<RecallHit>> = hits.drain(..).map(Some).collect();
    for i in order.into_iter().take(limit) {
        if let Some(h) = slots[i].take() {
            out.push(h);
        }
    }
    out
}

/// Unscoped bm25 recall (every project + global). Thin wrapper over `recall_scoped`.
pub fn recall(store: &EventStore, query: &str, limit: usize) -> anyhow::Result<Vec<RecallHit>> {
    recall_scoped(store, query, limit, &RecallScope::everything())
}

// ---- Semantic score-fusion recall (feature `semantic`) --------------------
//
// Reranks the bm25 candidate pool by `-rank + W*cosine(query, doc)`, then
// projects to current heads with the same dedup/limit as bm25 recall. The query
// vector is passed IN (the model lives in the warm embedder/daemon, never here),
// so this whole path is pure and unit-tested by injecting vectors - no ONNX in
// the test binary. bm25 stays the floor: a candidate with no stored vector
// contributes cosine 0 (pure bm25), so an empty/absent sidecar reduces exactly
// to today's bm25 order.

/// Weight on the (absolute) cosine term in the normalized fusion. The lexical leg
/// is min-max normalized to [0,1] per query while cosine keeps its absolute scale
/// (~[0,1] for relevant matches), so LAMBDA directly trades lexical vs semantic
/// evidence: LAMBDA > 1 lets a strong semantic hit outrank a weak lexical one (and
/// lets a zero-overlap dense hit compete at all). Chosen by the one-shot sweep in
/// examples/recall_eval.rs (highest recall@5 with no category regression).
#[cfg(feature = "semantic")]
pub const FUSION_LAMBDA: f64 = 1.5;

/// How many bm25 candidates to rerank. Matches the eval's pool: wide enough that
/// a semantically-strong but lexically-weak gold is in reach, bounded enough that
/// fetching + scoring stays well under a millisecond.
#[cfg(feature = "semantic")]
const FUSION_POOL: usize = 200;

/// How many DENSE (cosine) candidates to add to the bm25 pool. These are what let
/// a zero-lexical-overlap paraphrase gold surface at all: a bm25-pool rerank can
/// never reach a doc it never lexically matched. Bounded so a burst of near-tied
/// semantic distractors cannot crowd out the lexical hits.
#[cfg(feature = "semantic")]
const DENSE_TOPM: usize = 64;

/// Minimum cosine for a fused hit to count as SEMANTIC evidence (matched_and).
/// Below it, a hit that also failed the AND pass is a lexical coincidence and
/// stays subject to the courier's coverage gate. Conservative for a MiniLM-class
/// embedder, where genuinely related pairs sit well above this.
#[cfg(feature = "semantic")]
const DENSE_EVIDENCE_MIN: f64 = 0.30;

/// Cosine of two unit-norm vectors (a plain dot product, in f64 to avoid drift).
#[cfg(feature = "semantic")]
fn dot(a: &[f32], b: &[f32]) -> f64 {
    let s: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
    // A corrupt (aligned-but-garbage) stored vector can decode to NaN/inf; a
    // non-finite score would poison the sort comparator, so neutralize it to 0.
    if s.is_finite() {
        s
    } else {
        0.0
    }
}

/// Lexical candidate leg for fusion: walk the FTS MATCH in rank order and keep the
/// first `cap` CURRENT-HEAD seqs (each with its best bm25 rank), streaming with NO
/// fixed raw-row LIMIT. A flat `LIMIT cap` over raw rows would let a heavily-revised
/// entity's many superseded revs (all matching, all near the same rank) fill the
/// window and evict its current head before it is ever considered - exactly the
/// starvation bm25 `collect_heads` is written to avoid. Returned in rank order, so
/// the caller's candidate list is deterministic (not seeded from HashMap order).
#[cfg(feature = "semantic")]
fn lexical_head_pool(
    store: &EventStore,
    fts: &str,
    by_seq: &HashMap<i64, &Event>,
    heads: &HashMap<String, crate::cas::HeadSet>,
    filter: &ScopeFilter,
    cap: usize,
) -> Vec<(i64, f64)> {
    let conn = store.conn();
    let mut stmt = match conn.prepare("SELECT rowid, rank FROM event_fts WHERE event_fts MATCH ? ORDER BY rank") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows = match stmt.query_map(params![fts], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let mut out: Vec<(i64, f64)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for row in rows {
        let (seq, rank) = match row {
            Ok(p) => p,
            Err(_) => break, // fail-soft: return what we have
        };
        let ev = match by_seq.get(&seq) {
            Some(e) => *e,
            None => continue,
        };
        let head_set = match heads.get(&ev.entity_id) {
            Some(h) => h,
            None => continue,
        };
        if !head_set.heads.contains(&ev.this_hash) {
            continue; // a superseded rev never counts toward the head budget
        }
        if !filter.keep(ev) {
            continue; // out of scope / retracted: never spends a pool slot
        }
        if !seen.insert(ev.this_hash.clone()) {
            continue;
        }
        out.push((seq, rank));
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// Walk seqs in the given (already-ordered) sequence, keeping only current heads
/// and collapsing near-duplicate bodies, until `limit` are kept. The shared final
/// projection step for the fused path. Each item is (seq, bm25_rank); the kept
/// `RecallHit.rank` stays the bm25 rank (lower = better) for consistent
/// downstream semantics, independent of the fused ordering score.
#[cfg(feature = "semantic")]
fn finalize_heads(
    ordered: impl Iterator<Item = (i64, f64)>,
    by_seq: &HashMap<i64, &Event>,
    heads: &HashMap<String, crate::cas::HeadSet>,
    filter: &ScopeFilter,
    strong: &HashSet<i64>,
    limit: usize,
) -> Vec<RecallHit> {
    let mut hits: Vec<RecallHit> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_prefixes: Vec<String> = Vec::new();
    for (seq, rank) in ordered {
        let ev = match by_seq.get(&seq) {
            Some(e) => *e,
            None => continue,
        };
        let head_set = match heads.get(&ev.entity_id) {
            Some(h) => h,
            None => continue,
        };
        if !head_set.heads.contains(&ev.this_hash) {
            continue; // drop hits on revs that are no longer a current head
        }
        if !filter.keep(ev) {
            continue; // out of project scope, or a retracted (non-live) head
        }
        if !seen.insert(ev.this_hash.clone()) {
            continue;
        }
        let prefix = dedup_prefix(&ev.body);
        if seen_prefixes.contains(&prefix) {
            continue;
        }
        seen_prefixes.push(prefix);
        hits.push(RecallHit {
            entity_id: ev.entity_id.clone(),
            rev: ev.this_hash.clone(),
            body: ev.body.clone(),
            kind: ev.kind,
            is_diverged: head_set.is_diverged,
            rank,
            project: filter.projects.get(&ev.entity_id).cloned().flatten(),
            fact_type: crate::repo::fact_type(&ev.body),
            // Per-hit evidence: an AND-pass match or real dense (cosine) support.
            // A one-word OR coincidence with ~0 cosine must NOT claim strength,
            // or the courier's silence gate is bypassed on semantic builds.
            matched_and: strong.contains(&seq),
        });
        if hits.len() >= limit {
            break;
        }
    }
    hits
}

/// Hybrid score-fusion recall. `qvec` is the unit-norm query embedding; `vecs` is
/// the precomputed dense sidecar. Candidates come from BOTH a lexical leg (the
/// bm25 pool) AND a dense leg (top-M by cosine over all stored vectors); their
/// union is scored by NORMALIZED fusion - the bm25 leg is min-max scaled to [0,1]
/// per query, cosine keeps its absolute scale, `fused = bm_norm + lambda*cos` -
/// and the current heads are projected out (dedup + limit). Normalizing is what
/// lets the dense leg matter: a strong semantic hit (or a zero-overlap paraphrase
/// gold, absent from the bm25 pool) can outrank a weak lexical one, instead of the
/// unbounded raw bm25 score always dominating.
///
/// Fail-soft and bm25-flooring: an empty/absent sidecar means no dense leg and
/// every cosine is 0, so `fused = bm_norm` and the order reduces EXACTLY to bm25;
/// an empty query or no candidates yields an empty result; a vector-load error
/// degrades to bm25. The bm25 relevance floor is deliberately NOT applied here -
/// under fusion a low-bm25 but high-cosine hit is precisely what we want to surface.
#[cfg(feature = "semantic")]
pub fn recall_fused_scoped(
    store: &EventStore,
    query: &str,
    qvec: &[f32],
    vecs: &crate::vectors::VectorStore,
    limit: usize,
    lambda: f64,
    scope: &RecallScope,
) -> anyhow::Result<Vec<RecallHit>> {
    if limit == 0 {
        return Ok(vec![]);
    }

    let events = store.get_all_events()?;
    let by_seq: HashMap<i64, &Event> = events.iter().map(|e| (e.seq, e)).collect();
    let heads = compute_head_sets(&events);
    let projects = crate::cas::compute_projects(&events);
    let filter = ScopeFilter { projects: &projects, scope, memories_only: false };

    // Lexical leg: current-head candidates in bm25 rank order (streaming head-walk,
    // NO fixed raw-row window - see lexical_head_pool). Empty if the query has no
    // content tokens.
    let lexical: Vec<(i64, f64)> = match fts_query(query) {
        Some(or_query) => lexical_head_pool(store, &or_query, &by_seq, &heads, &filter, FUSION_POOL),
        None => vec![],
    };

    // Strict-AND pass: the seqs that matched the WHOLE question. Besides feeding
    // the matched_and evidence set, these are CANDIDATES in their own right: a
    // head matching every content term can still bm25-rank below FUSION_POOL
    // single-term high-scorers on the OR query (one very common token matched by
    // hundreds of short chunks), and without a dense vector it would otherwise
    // not be a candidate at all - dropped exactly where pure bm25 (AND-first)
    // would have returned it first.
    let and_pool: Vec<(i64, f64)> = match fts_query_and(query) {
        Some(aq) => lexical_head_pool(store, &aq, &by_seq, &heads, &filter, FUSION_POOL),
        None => vec![],
    };
    let mut strong: HashSet<i64> = and_pool.iter().map(|(s, _)| *s).collect();

    // Lexical rank for scoring: the OR walk's rank, else the AND walk's (an
    // AND-only candidate gets its own bm25 evidence instead of bm_norm 0).
    let mut bm_rank: HashMap<i64, f64> = lexical.iter().copied().collect();
    for (seq, rank) in &and_pool {
        bm_rank.entry(*seq).or_insert(*rank);
    }

    // Dense leg: brute-force the query cosine over EVERY stored vector, keep the
    // top-M. Real dense retrieval (not a pool rerank), so a paraphrase gold with
    // no lexical overlap is reachable. An empty/absent sidecar leaves this empty.
    let all = vecs.all_vectors().unwrap_or_default();
    let cos_by_seq: HashMap<i64, f64> = all.iter().map(|(s, v)| (*s, dot(qvec, v))).collect();
    // Keep only in-scope CURRENT-HEAD dense candidates. The head check mirrors
    // lexical_head_pool's guard: without it, the near-identical vectors of a
    // frequently-revised entity's superseded revs cluster at the top of the
    // cosine order, eat DENSE_TOPM slots, and are then discarded in
    // finalize_heads - starving the paraphrase reach this leg exists for as a
    // store ages (the same starvation the lexical leg is hardened against).
    let mut dense: Vec<(i64, f64)> = cos_by_seq
        .iter()
        .filter(|(s, _)| {
            by_seq.get(s).map_or(false, |ev| {
                filter.keep(ev)
                    && heads
                        .get(&ev.entity_id)
                        .map_or(false, |h| h.heads.contains(&ev.this_hash))
            })
        })
        .map(|(s, c)| (*s, *c))
        .collect();
    dense.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    dense.truncate(DENSE_TOPM);

    // Union of the candidate sets, de-duplicated, in a DETERMINISTIC order:
    // OR-pool hits first (rank order), then AND-only extras, then dense-only
    // hits (cosine order). Never seeded from HashMap iteration, so identical
    // inputs give identical candidates.
    let mut seen: HashSet<i64> = HashSet::new();
    let mut cand: Vec<i64> = Vec::new();
    for (seq, _) in lexical.iter().chain(and_pool.iter()) {
        if seen.insert(*seq) {
            cand.push(*seq);
        }
    }
    for (seq, _) in &dense {
        if seen.insert(*seq) {
            cand.push(*seq);
        }
    }
    if cand.is_empty() {
        return Ok(vec![]);
    }

    // Dense evidence over ALL candidates' cosines, not only the truncated
    // top-M: the stated rule is "an AND match or real cosine support"
    // (>= DENSE_EVIDENCE_MIN), and a lexical candidate genuinely resembling
    // the query must not lose its evidence - and then get silenced by the
    // courier's coverage gate - just because DENSE_TOPM other vectors rank
    // higher on this query.
    for seq in &cand {
        if cos_by_seq.get(seq).copied().unwrap_or(0.0) >= DENSE_EVIDENCE_MIN {
            strong.insert(*seq);
        }
    }

    // Normalize the lexical leg: min-max the pool's -rank to [0,1] so a strong
    // semantic match can compete with (and outrank a weak) lexical hit, instead of
    // the unbounded raw bm25 score always dominating. `fused = bm_norm + lambda*cos`
    // with cosine clamped to >= 0 (a negatively-correlated doc gets no bonus, and
    // is never pushed below a no-vector candidate). A dense-only candidate has no
    // bm25 evidence (bm_norm 0) and competes on cosine alone. RecallHit.rank keeps
    // the bm25 rank when present (0.0 for a dense-only hit; informational only).
    let raws: Vec<f64> = bm_rank.values().map(|r| -r).collect();
    let rmin = raws.iter().copied().fold(f64::INFINITY, f64::min);
    let rmax = raws.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = rmax - rmin;
    // Query-routed class prior + term-coverage rerank inputs, computed once per
    // query. The folded qterms match covers_query's normalization so "coverage"
    // means the same thing in both places.
    let route = route_query(query);
    let qterms: Vec<String> = {
        let mut seen = HashSet::new();
        content_tokens(query)
            .iter()
            .map(|t| fold_diacritics(&t.to_lowercase()))
            .filter(|t| seen.insert(t.clone()))
            .collect()
    };
    let id_phrases = identifier_phrases(query);
    let mut scored: Vec<(i64, f64, f64)> = cand
        .into_iter()
        .map(|seq| {
            let rank = bm_rank.get(&seq).copied();
            let bm_norm = match rank {
                // a single lexical hit (or all-equal ranks) -> full lexical weight
                Some(r) if span > 0.0 => (-r - rmin) / span,
                Some(_) => 1.0,
                None => 0.0,
            };
            let cos = cos_by_seq.get(&seq).copied().unwrap_or(0.0).max(0.0);
            let (delta, coverage, trigger) = match by_seq.get(&seq) {
                Some(ev) => (
                    class_delta(route, source_class(&ev.entity_id)),
                    coverage_bonus(&ev.body, &qterms, &id_phrases),
                    trigger_bonus(&ev.body, query, &qterms),
                ),
                None => (0.0, 0.0, 0.0),
            };
            (seq, rank.unwrap_or(0.0), bm_norm + lambda * cos + delta + coverage + trigger)
        })
        .collect();
    // Total order (NaN-safe) with a deterministic seq tie-break, so equal fused
    // scores always resolve the same way across runs.
    scored.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));

    // NOTE: a min-score floor on the fused score was measured here (53-question
    // set, floors 0.35-1.2) and REJECTED: no floor value improved coverage -
    // every cut removed gold-carrying tail hits (-0.1pp at 0.8 up to -2.2pp at
    // 1.2) while the courier's coverage/AND gates already handle noise more
    // precisely. Same verdict as the removed bm25 floor: do not re-add one.
    Ok(finalize_heads(
        scored.into_iter().map(|(seq, rank, _)| (seq, rank)),
        &by_seq,
        &heads,
        &filter,
        &strong,
        limit,
    ))
}

/// Unscoped hybrid score-fusion recall (every project + global). Thin wrapper over
/// `recall_fused_scoped` - used by tests and the eval harness.
#[cfg(feature = "semantic")]
pub fn recall_fused(
    store: &EventStore,
    query: &str,
    qvec: &[f32],
    vecs: &crate::vectors::VectorStore,
    limit: usize,
    lambda: f64,
) -> anyhow::Result<Vec<RecallHit>> {
    recall_fused_scoped(store, query, qvec, vecs, limit, lambda, &RecallScope::everything())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_allows_global_and_current_project() {
        let all = RecallScope::everything();
        assert!(all.allows(None) && all.allows(Some("X")) && all.allows(Some("@global")));
        let a = RecallScope::current(Some("ProjA".to_string()));
        assert!(a.allows(None), "global memory always in scope");
        assert!(a.allows(Some("@global")), "global file always in scope");
        assert!(a.allows(Some("ProjA")), "current project in scope");
        assert!(!a.allows(Some("ProjB")), "another project is hidden (no bleed)");
        let none = RecallScope::current(None);
        assert!(none.allows(None) && none.allows(Some("@global")), "projectless -> global tier only");
        assert!(!none.allows(Some("ProjA")), "projectless hides all projects");
    }

    #[test]
    fn test_lexical_trigger_boost_rescues_declared_fact_from_below_the_floor() {
        // One distractor matches three query words tightly - so strong that
        // the relevance floor cuts everything else out of the pool. The
        // preventer's own prose shares NO query word; only its footer carries
        // them. Control store: the exact same words as plain tags (identical
        // FTS strength) - only the fires-when FIELD may make the difference,
        // not the words' presence.
        let seed = |field: &str| {
            let mut store = EventStore::in_memory().unwrap();
            store
                .append_event("s", "l", "a", EventKind::FactCreated, "d-strong", None,
                    "DECISION: bladeren door een reeks preloadt de volgende twee previews, niet meer.")
                .unwrap();
            store
                .append_event("s", "l", "a", EventKind::FactCreated, "d-weak1", None,
                    "NOTE: de cache-map is veilig te legen; hij wordt lazy heropgebouwd.")
                .unwrap();
            store
                .append_event("s", "l", "a", EventKind::FactCreated, "d-weak2", None,
                    "NOTE: previews gebruiken de embedded JPEG uit het RAW-bestand.")
                .unwrap();
            store
                .append_event("s", "l", "a", EventKind::FactCreated, "prev", None,
                    &format!("de verkleiner houdt een strak geheugenbudget aan; grote sets gaan in delen\n\n[memory/decision | {field} | project: global]"))
                .unwrap();
            store
        };
        let query =
            "de previews openen traag; versnel de thumbnails met een cache zodat bladeren door een reeks vlot gaat";

        let tagged = seed("tags: x | fires-when: thumbnails cache previews versnellen");
        let hits = recall(&tagged, query, 3).unwrap();
        assert!(
            hits.iter().any(|h| h.entity_id == "prev"),
            "declared triggers rescue the fact past the relevance floor: {:?}",
            hits.iter().map(|h| &h.entity_id).collect::<Vec<_>>()
        );

        let control = seed("tags: thumbnails cache previews versnellen");
        let hits = recall(&control, query, 3).unwrap();
        assert!(
            hits.iter().all(|h| h.entity_id != "prev"),
            "same words as plain tags get no boost - the FIELD is the signal: {:?}",
            hits.iter().map(|h| &h.entity_id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_snippet_surfaces_content_past_a_heading() {
        // a doc-chunk body: first line is a breadcrumb, real content follows.
        // The old first-line-only display showed only the breadcrumb (P2).
        let body = "DOC > section > sub\nThe actual gotcha: do X not Y.\n\n[footer]";
        let s = snippet(body, 220, "gotcha");
        assert!(s.contains("The actual gotcha"), "content past the heading must surface: {}", s);
        assert!(!s.contains('\n'), "newlines collapsed to spaces");
        // truncation adds an ellipsis and respects the cap
        let long = "word ".repeat(300);
        let t = snippet(&long, 50, "word");
        assert!(t.ends_with("..."));
        assert!(t.chars().count() <= 53);
    }

    #[test]
    fn test_snippet_centers_on_deep_match() {
        // the query term sits well past `max`; the window must center on it.
        let body = format!("{}NEEDLE tail content here", "filler ".repeat(60));
        let s = snippet(&body, 60, "needle");
        assert!(s.contains("NEEDLE"), "deep match must be windowed into view: {}", s);
        assert!(s.starts_with("..."), "leading ellipsis when windowed past the start: {}", s);
        assert!(s.chars().count() <= 66, "respects the cap + ellipses: {}", s);
        // no query term -> head-truncation (no leading ellipsis)
        let h = snippet(&body, 60, "zzz");
        assert!(!h.starts_with("..."), "no match -> head truncation: {}", h);
        assert!(h.starts_with("filler"));
    }

    #[test]
    fn test_snippet_picks_densest_window_and_splits_wide_caps() {
        // Two term clusters far apart: "alpha" early, "alpha beta gamma" deep.
        // A first-match window would show the early lone "alpha" and miss the
        // dense cluster - the measured "right hit, details cut" drift mode.
        let body = format!(
            "alpha intro text {} the alpha beta gamma details live here {}",
            "filler ".repeat(80),
            "tail ".repeat(40)
        );
        let s = snippet(&body, 120, "alpha beta gamma");
        assert!(
            s.contains("beta gamma"),
            "the DENSEST window wins, not the first match: {}",
            s
        );
        // Wide cap + two disjoint clusters ("alpha beta" early, "gamma delta"
        // deep, too far apart for one window) -> two joined windows.
        let body2 = format!(
            "the alpha beta decision was made {} while gamma delta carries the limit values",
            "filler ".repeat(200)
        );
        let w = snippet(&body2, 600, "alpha beta gamma delta");
        assert!(
            w.contains("alpha beta") && w.contains("gamma delta"),
            "a wide cap spends its budget on BOTH clusters: {}",
            w
        );
        assert!(w.contains(" ... "), "disjoint windows are visibly joined: {}", w);
    }

    #[test]
    fn test_snippet_no_panic_on_case_expanding_unicode() {
        // 'İ' (U+0130) lowercases to TWO codepoints, so a match index measured in
        // the lowercased copy can exceed the original char count. Before the clamp
        // this panicked with "slice start > end" on the courier hook path.
        let body = format!("{} needle tail content here", "İ".repeat(300));
        let s = snippet(&body, 220, "needle"); // must not panic
        assert!(s.chars().count() <= 226, "still respects the cap + ellipses");
    }

    #[test]
    fn test_fts_query_build() {
        assert_eq!(fts_query("  "), None);
        assert_eq!(fts_query("a , . !"), None, "all tokens < 2 chars -> None");
        assert_eq!(fts_query("deploy watcher"), Some("\"deploy\" OR \"watcher\"".to_string()));
        // quotes in the prompt must not break the FTS literal
        assert_eq!(fts_query("say \"hi\""), Some("\"say\" OR \"hi\"".to_string()));
    }

    #[test]
    fn test_recall_returns_matching_head() {
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "the deploy watcher gotcha")
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "something about filament drying")
            .unwrap();

        let hits = recall(&store, "deploy watcher", 3).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entity_id, "e1");
        assert!(hits[0].body.contains("deploy watcher"));
    }

    #[test]
    fn test_recall_skips_superseded_rev() {
        // e1 is revised; the old rev's text must NOT be recalled once replaced.
        let mut store = EventStore::in_memory().unwrap();
        let v1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "original zephyr content")
            .unwrap();
        store
            .append_event(
                "s", "l", "a", EventKind::FactRevised, "e1", Some(&v1.this_hash), "updated content only",
            )
            .unwrap();

        // "zephyr" lives only in the replaced rev -> not a current head -> no hit
        let hits = recall(&store, "zephyr", 3).unwrap();
        assert!(hits.is_empty(), "a superseded rev must not surface in recall");

        // the current head's text still recalls
        let hits2 = recall(&store, "updated", 3).unwrap();
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].rev, /* head */ {
            let heads = compute_head_sets(&store.get_all_events().unwrap());
            heads["e1"].heads.iter().next().unwrap().clone()
        });
    }

    #[test]
    fn test_recall_frequently_revised_head_not_starved() {
        // Regression (audit MAJOR): an entity revised many times must still
        // recall its CURRENT head. The old fixed over-fetch window (limit*8)
        // let the superseded revs - which all match the same query and sort
        // ahead of the head on tied rank - crowd the head out, returning 0 hits.
        let mut store = EventStore::in_memory().unwrap();
        let mut parent = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "zeta keyword v0")
            .unwrap()
            .this_hash;
        for i in 1..=30 {
            parent = store
                .append_event(
                    "s", "l", "a", EventKind::FactRevised, "e1", Some(&parent),
                    &format!("zeta keyword v{}", i),
                )
                .unwrap()
                .this_hash;
        }
        // 31 revs for e1, all matching "zeta keyword"; only the last is a head.
        let hits = recall(&store, "zeta keyword", 3).unwrap();
        assert_eq!(hits.len(), 1, "the current head must not be starved by superseded revs");
        assert_eq!(hits[0].rev, parent, "the surviving hit is the current head");
        assert!(hits[0].body.contains("v30"));
    }

    #[test]
    fn test_covers_query_gate() {
        // multi-word query: >= 2 distinct terms must literally appear
        assert!(covers_query("the sync refactor plan", "ga verder met de sync refactor"));
        assert!(
            !covers_query("something about sync only", "ga verder met de sync refactor"),
            "one matching word of a multi-word query is a coincidence, not an answer"
        );
        // single-content-term query: that one term suffices (no self-silencing)
        assert!(covers_query("the deploy watcher gotcha", "deploy"));
        assert!(!covers_query("unrelated text", "deploy"));
        // stopword-only query -> no content terms -> never covers. The body
        // CONTAINING those stopwords is the review-found leak: content_tokens'
        // fallback made the stopwords themselves count as query terms.
        assert!(!covers_query("anything", "de het een"));
        assert!(
            !covers_query("dit is wat we doen als het faalt", "wat is dat dan"),
            "a body containing the query's stopwords must not cover a contentless query"
        );
        assert!(!has_content_terms("wat is dat dan"), "all-stopword prompt has no content");
        assert!(has_content_terms("wat is de sync refactor"), "a real term counts as content");
    }

    #[test]
    fn test_covers_query_folds_diacritics_like_fts() {
        // FTS unicode61 (remove_diacritics=1) matches "privé" against "prive";
        // the coverage gate must count that match instead of silencing it.
        assert!(covers_query(
            "sync faalt op prive paden",
            "waarom crasht de sync bij privé bestanden"
        ));
        assert!(covers_query("een privé café", "privé café"));
    }

    #[test]
    fn test_dedup_prefix_strips_metadata_footer() {
        let plain = "GOTCHA: never open the db over SMB";
        let typed = "GOTCHA: never open the db over SMB\n\n[memory/gotcha | tags: db | project: P]";
        assert_eq!(dedup_prefix(plain), dedup_prefix(typed), "footer must not defeat duplicate detection");
        let chunk = "port: 8080\n\n[repo file | P/conf.yml | chunk 1/1]";
        assert_eq!(dedup_prefix(chunk), dedup_prefix("port: 8080"));
        // a mid-body bracket line followed by more text is NOT a footer
        let not_footer = "text\n\n[note] more\ntext after";
        assert!(dedup_prefix(not_footer).contains("more"));
    }

    #[test]
    fn test_memories_only_recall_excludes_chunks() {
        let mut store = EventStore::in_memory().unwrap();
        // a code chunk and a memory, both matching "widget"
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "Proj:src/widget.rs#0", None, "widget widget widget code")
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "Proj:mem-1", None, "GOTCHA: the widget must never be blue")
            .unwrap();
        let scope = RecallScope::current(Some("Proj".to_string()));
        let all = recall_scoped(&store, "widget", 5, &scope).unwrap();
        assert_eq!(all.len(), 2, "normal recall sees both");
        let mems = recall_memories_scoped(&store, "widget", 5, &scope).unwrap();
        assert_eq!(mems.len(), 1, "memories-only recall drops the chunk");
        assert_eq!(mems[0].entity_id, "Proj:mem-1");
        assert_eq!(mems[0].fact_type, Some(crate::repo::FactType::Gotcha), "typed fact is classified");
    }

    #[test]
    fn test_matched_and_flag_strict_vs_fallback() {
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "deploy watcher restarts the app")
            .unwrap();
        // both terms present -> strict AND pass -> matched_and = true
        let strict = recall(&store, "deploy watcher", 3).unwrap();
        assert!(strict[0].matched_and, "a whole-question match is strong evidence");
        // only one term matches -> OR fallback -> matched_and = false
        let weak = recall(&store, "deploy zeppelin unrelated", 3).unwrap();
        assert!(!weak.is_empty());
        assert!(!weak[0].matched_and, "an OR-fallback hit is flagged as weak evidence");
    }

    #[test]
    fn test_recall_empty_query_and_no_match() {
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "alpha beta")
            .unwrap();
        assert!(recall(&store, "   ", 3).unwrap().is_empty());
        assert!(recall(&store, "nonexistenttokenxyz", 3).unwrap().is_empty());
        assert!(recall(&store, "alpha", 0).unwrap().is_empty(), "limit 0 -> no hits");
    }
}

#[cfg(all(test, feature = "semantic"))]
mod fused_tests {
    use super::*;
    use crate::embed::DIM;
    use crate::vectors::VectorStore;

    /// A unit vector with `+/-1` on axis 0 (so cosine with `unit()` is +/-1).
    fn axis(sign: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; DIM];
        v[0] = sign;
        v
    }

    #[test]
    fn test_fusion_promotes_semantic_match_over_stronger_bm25() {
        // e1 is the STRONGER bm25 match (keyword repeated); e2 is weaker lexically
        // but its vector points exactly with the query while e1's points opposite.
        let mut store = EventStore::in_memory().unwrap();
        let e1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "keyword keyword keyword alpha")
            .unwrap();
        let e2 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "keyword beta")
            .unwrap();

        // pure bm25 prefers the keyword-dense e1
        let bm = recall(&store, "keyword", 3).unwrap();
        assert_eq!(bm[0].entity_id, "e1", "bm25 alone ranks the keyword-dense doc first");

        let dir = tempfile::tempdir().unwrap();
        let mut vecs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vecs.upsert_batch(&[(e1.seq, axis(-1.0)), (e2.seq, axis(1.0))]).unwrap();

        // lambda > 1 lets the aligned-but-lexically-weaker doc win (bm_norm 0 +
        // 2*1 = 2 > bm_norm 1 + 2*0 = 1).
        let fused = recall_fused(&store, "keyword", &axis(1.0), &vecs, 3, 2.0).unwrap();
        assert_eq!(
            fused[0].entity_id, "e2",
            "fusion promotes the semantically-aligned doc above the stronger bm25 one"
        );
    }

    #[test]
    fn test_empty_sidecar_reduces_to_bm25_order() {
        // With NO stored vectors every candidate scores cosine 0, so the fused
        // order must equal the pure bm25 order (the floor / no-regression guard).
        let mut store = EventStore::in_memory().unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "shared token one one one").unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "shared token two").unwrap();

        let dir = tempfile::tempdir().unwrap();
        let vecs = VectorStore::open(&dir.path().join("v.db")).unwrap(); // empty
        let qvec = axis(1.0);

        let bm: Vec<String> = recall(&store, "shared token", 5).unwrap().into_iter().map(|h| h.entity_id).collect();
        let fused: Vec<String> = recall_fused(&store, "shared token", &qvec, &vecs, 5, 1.5).unwrap().into_iter().map(|h| h.entity_id).collect();
        assert_eq!(bm, fused, "an absent sidecar must reproduce the exact bm25 order");
    }

    #[test]
    fn test_dense_leg_surfaces_zero_overlap_paraphrase() {
        // The gold (e1) shares NO token with the query, so it is absent from the
        // bm25 pool - only the dense leg (high cosine) can surface it. This is the
        // capability a pool-rerank alone cannot provide.
        let mut store = EventStore::in_memory().unwrap();
        let e1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "resident model stays warm across turns")
            .unwrap();
        let e2 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "alpha bravo charlie noise")
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut vecs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vecs.upsert_batch(&[(e1.seq, axis(1.0)), (e2.seq, axis(-1.0))]).unwrap();

        // "alpha" matches ONLY e2 lexically; e1 is unreachable via bm25.
        assert!(recall(&store, "alpha", 5).unwrap().iter().all(|h| h.entity_id != "e1"),
            "sanity: bm25 alone cannot reach the zero-overlap gold");
        let hits = recall_fused(&store, "alpha", &axis(1.0), &vecs, 3, 2.0).unwrap();
        assert_eq!(hits[0].entity_id, "e1", "the dense leg surfaces a gold with zero lexical overlap");
    }

    #[test]
    fn test_query_routing_and_source_class() {
        // knowledge vocabulary routes to knowledge
        assert_eq!(route_query("wat was de beslissing over de sync batches"), QueryRoute::Knowledge);
        assert_eq!(route_query("why did we pick this rule"), QueryRoute::Knowledge);
        // identifier-shaped tokens route to code
        assert_eq!(route_query("where is compute_head_sets called"), QueryRoute::Code);
        assert_eq!(route_query("recall.rs fused scoring"), QueryRoute::Code);
        assert_eq!(route_query("src/courier.rs freshness"), QueryRoute::Code);
        // plain prose (or mixed signals) stays neutral
        assert_eq!(route_query("how does the deploy watcher work"), QueryRoute::Neutral);
        assert_eq!(route_query("why does lexical_head_pool cap heads"), QueryRoute::Neutral);
        // source class from the id shape
        assert_eq!(source_class("01KMEMORY"), SourceClass::Memory);
        assert_eq!(source_class("Proj:mem-abc"), SourceClass::Memory);
        assert_eq!(source_class("Proj:docs/setup.md#2"), SourceClass::Doc);
        assert_eq!(source_class("Proj:src/main.rs#0"), SourceClass::Code);
        // code-routed queries never get a delta, for any class
        assert_eq!(class_delta(QueryRoute::Code, SourceClass::Memory), 0.0);
        assert_eq!(class_delta(QueryRoute::Code, SourceClass::Doc), 0.0);
    }

    #[test]
    fn test_trigger_bonus_fires_only_on_declared_vocabulary() {
        let raw = "export resume progressbar";
        let q: Vec<String> =
            ["export", "resume", "progressbar"].iter().map(|s| s.to_string()).collect();
        let tagged = "the export rule\n\n[memory/gotcha | tags: x | fires-when: export progressbar | project: P]";
        let untagged = "the export rule about export and progressbar, mentioned inline";
        let full = trigger_bonus(tagged, raw, &q);
        assert!(
            (full - TRIGGER_WEIGHT * 2.0 / 3.0).abs() < 1e-9,
            "two matched generic terms = 2 evidence points: {full}"
        );
        assert_eq!(trigger_bonus(untagged, raw, &q), 0.0, "no fires-when field = no bonus, ever");
        // absolute evidence: one matched term scores the same regardless of
        // how long the declared list is (ratio no longer rewards short lists)
        let one = trigger_bonus(
            "r\n\n[memory/note | tags: | fires-when: export tarball | project: P]",
            raw,
            &q,
        );
        assert!((one - TRIGGER_WEIGHT / 3.0).abs() < 1e-9, "one evidence point: {one}");
        // an unrelated query never fires
        let other: Vec<String> = ["courier", "snippet"].iter().map(|s| s.to_string()).collect();
        assert_eq!(trigger_bonus(tagged, "courier snippet", &other), 0.0);
    }

    #[test]
    fn test_is_identifier_shaped_detects_structure_not_plain_words() {
        for s in ["foo::bar", "server/lib/be-postal.json", "snake_case_name", "matrixcache.get",
                  "kebab-case", "fooBar", "MatrixCache"] {
            assert!(is_identifier_shaped(s), "{s} must read as identifier-shaped");
        }
        for s in ["docker", "Docker", "ALLCAPS", "hi", "gewoon"] {
            assert!(!is_identifier_shaped(s), "{s} must read as a plain word");
        }
    }

    #[test]
    fn test_trigger_bonus_matches_path_trigger_via_substring() {
        // Regression: a path-shaped trigger term is ONE whitespace token but the
        // query side is split on every non-alphanumeric char, so exact token
        // equality could never fire - provably 0 before the containment mode.
        let tagged = "postal dataset rule\n\n[memory/gotcha | tags: | fires-when: server/lib/be-postal.json | project: P]";
        let raw = "werk server/lib/be-postal.json bij met de nieuwe fusiegemeenten";
        let q: Vec<String> = tokens(raw);
        let bonus = trigger_bonus(tagged, raw, &q);
        assert!(
            (bonus - TRIGGER_WEIGHT * 2.0 / 3.0).abs() < 1e-9,
            "one identifier match = 2 evidence points: {bonus}"
        );
        // and not on an unrelated prompt that merely shares fragments
        let other = "de server leest json config bij het opstarten";
        assert_eq!(trigger_bonus(tagged, other, &tokens(other)), 0.0);
    }

    #[test]
    fn test_trigger_bonus_docker_cp_no_longer_overfires_on_generic_word_alone() {
        // Regression for the denominator bug: 'cp' (2 chars) used to be DROPPED
        // from the list, collapsing 'docker cp deploy' into a 2-term list and
        // inflating the ratio. It now stays in the denominator as a non-match.
        let tagged = "copy rule\n\n[memory/gotcha | tags: | fires-when: docker cp deploy | project: P]";
        let raw = "schrijf uitleg over docker containers";
        let bonus = trigger_bonus(tagged, raw, &tokens(raw));
        assert!(
            (bonus - TRIGGER_WEIGHT / 3.0).abs() < 1e-9,
            "docker alone = 1 of 3 declared terms, not full weight: {bonus}"
        );
    }

    #[test]
    fn test_trigger_bonus_min_length_floor_blocks_short_identifier_fragments() {
        // 'a.b' is identifier-shaped but below MIN_IDENTIFIER_PHRASE_LEN: too
        // short for containment to be trustworthy evidence.
        let tagged = "short rule\n\n[memory/note | tags: | fires-when: a.b | project: P]";
        let raw = "config a.b staat in de repo";
        assert_eq!(trigger_bonus(tagged, raw, &tokens(raw)), 0.0);
    }

    #[test]
    fn test_coverage_bonus_prefers_full_tight_matches() {
        let q = vec!["sync".to_string(), "batches".to_string()];
        let none: Vec<String> = vec![];
        let full_tight = coverage_bonus("the sync refactor keeps batches small", &q, &none);
        let full_loose = coverage_bonus(
            &format!("sync {} batches", "filler ".repeat(60)),
            &q,
            &none,
        );
        let partial = coverage_bonus("sync sync sync everywhere sync", &q, &none);
        assert!(full_tight > full_loose, "tight window beats loose: {full_tight} vs {full_loose}");
        assert!(full_loose > partial, "full coverage beats repeated single term: {full_loose} vs {partial}");
        assert_eq!(coverage_bonus("nothing relevant", &q, &none), 0.0);
    }

    #[test]
    fn test_identifier_bonus_rewards_exact_phrase_and_respects_floor() {
        let q = vec!["matrixcache".to_string(), "ensure".to_string()];
        let phrases = identifier_phrases("how does MatrixCache::ensure work");
        assert_eq!(phrases, vec!["matrixcache::ensure".to_string()], "one folded phrase extracted");
        let defining = "fn ensure() rebuilds; matrixcache::ensure reloads the sidecar";
        let distractor = "matrixcache matrixcache matrixcache ensure rebuild everywhere";
        let with_phrase = coverage_bonus(defining, &q, &phrases);
        let without_phrase = coverage_bonus(distractor, &q, &phrases);
        assert!(
            with_phrase > without_phrase,
            "the body carrying the exact phrase must outscore fragments: {with_phrase} vs {without_phrase}"
        );
        // floor: short identifier fragments never become phrases
        assert!(identifier_phrases("check a.b now").is_empty(), "below-floor fragment filtered");
    }

    #[test]
    fn test_knowledge_query_class_prior_breaks_the_tie_memory_first() {
        // A memory and a code chunk with lexically EQUIVALENT bodies (same terms,
        // same length; prefixes differ so near-dup collapse keeps both): bm25
        // scores them identically, so the outcome isolates the class prior.
        let mut store = EventStore::in_memory().unwrap();
        let chunk = store
            .append_event(
                "s", "l", "a", EventKind::FactCreated, "P:src/a.rs#0", None,
                "one alpha sync batches replication text",
            )
            .unwrap();
        store
            .append_event(
                "s", "l", "a", EventKind::FactCreated, "P:mem-dec", None,
                "two alpha sync batches replication text",
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vecs = VectorStore::open(&dir.path().join("v.db")).unwrap(); // no vectors: bm25-only fusion

        // Knowledge-routed: the class prior lifts the memory over the equal chunk.
        let hits = recall_fused(&store, "wat was de beslissing over sync batches", &axis(1.0), &vecs, 3, 1.5).unwrap();
        assert_eq!(
            hits[0].entity_id, "P:mem-dec",
            "on equal lexical evidence a knowledge query prefers the memory; got: {:?}",
            hits.iter().map(|h| h.entity_id.as_str()).collect::<Vec<_>>()
        );

        // Code-routed: NO delta for any class - equal scores fall back to the
        // deterministic seq tie-break, i.e. the chunk appended first. This is
        // the code-categories-unchanged gate in miniature.
        let code_hits = recall_fused(&store, "sync_batches() alpha replication", &axis(1.0), &vecs, 3, 1.5).unwrap();
        assert_eq!(
            code_hits[0].rev, chunk.this_hash,
            "a code-routed query applies no prior (pure bm25 + tie-break); got: {:?}",
            code_hits.iter().map(|h| h.entity_id.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_fused_keeps_strict_and_hit_outside_or_pool() {
        // Review finding: a head matching EVERY content term can bm25-rank below
        // FUSION_POOL single-term high-scorers on the OR query; without a dense
        // vector it then used to vanish from the candidate set entirely, while
        // pure bm25 (AND-first) would have returned it FIRST. The AND pool must
        // feed candidates, not only the matched_and evidence set.
        let mut store = EventStore::in_memory().unwrap();
        // Flood the OR pool: FUSION_POOL+ single-term spam heads with high tf in
        // tiny bodies (maximal bm25), half on each query term.
        for i in 0..(FUSION_POOL + 20) {
            let term = if i % 2 == 0 { "alpha" } else { "bravo" };
            let body = format!("{t} {t} {t} {t} {t} {t} {t} {t}", t = term);
            store
                .append_event("s", "l", "a", EventKind::FactCreated, &format!("spam{}", i), None, &body)
                .unwrap();
        }
        // The gold: the ONLY head containing BOTH terms, once each, in a long
        // body (low bm25 on the OR query by construction).
        let gold_body = "the decision was that alpha and bravo stay coupled through the \
                         batching layer with every event under the configured ceiling for \
                         replication safety and reconcile order across machines";
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "gold", None, gold_body)
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let vecs = VectorStore::open(&dir.path().join("v.db")).unwrap(); // no vectors at all

        let hits = recall_fused(&store, "alpha bravo", &axis(1.0), &vecs, 3, 1.5).unwrap();
        assert!(
            hits.iter().any(|h| h.entity_id == "gold"),
            "the strict-AND gold must stay a candidate even outside the OR pool; got: {:?}",
            hits.iter().map(|h| h.entity_id.as_str()).collect::<Vec<_>>()
        );
        let gold = hits.iter().find(|h| h.entity_id == "gold").unwrap();
        assert!(gold.matched_and, "an AND-pass hit carries matched_and evidence");
    }

    #[test]
    fn test_dense_evidence_not_limited_to_top_m() {
        // Review finding: a lexical candidate with cosine >= DENSE_EVIDENCE_MIN
        // used to get matched_and=false when DENSE_TOPM other vectors ranked
        // higher - the evidence check ran over the truncated dense list instead
        // of the candidates' own cosines.
        let mut store = EventStore::in_memory().unwrap();
        // DENSE_TOPM boilerplate heads, each with a vector at cosine ~0.6.
        let mut batch: Vec<(i64, Vec<f32>)> = Vec::new();
        let mut boiler = vec![0.0f32; DIM];
        boiler[0] = 0.6;
        boiler[1] = 0.8; // unit norm: 0.36 + 0.64 = 1
        for i in 0..DENSE_TOPM {
            let ev = store
                .append_event("s", "l", "a", EventKind::FactCreated, &format!("b{}", i), None, &format!("boilerplate filler {}", i))
                .unwrap();
            batch.push((ev.seq, boiler.clone()));
        }
        // The gold: matches ONE query term lexically ("sync"), cosine 0.5 - real
        // semantic support (>= DENSE_EVIDENCE_MIN) but below all the boilerplate.
        let ev = store
            .append_event("s", "l", "a", EventKind::FactCreated, "gold", None, "sync ceiling stays at one hundred")
            .unwrap();
        let mut gv = vec![0.0f32; DIM];
        gv[0] = 0.5;
        gv[1] = (1.0f32 - 0.25).sqrt();
        batch.push((ev.seq, gv));

        let dir = tempfile::tempdir().unwrap();
        let mut vecs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vecs.upsert_batch(&batch).unwrap();

        // Query "sync bug": gold matches only "sync" literally, so it needs its
        // cosine as evidence to pass the courier's coverage gate downstream.
        let hits = recall_fused(&store, "sync bug", &axis(1.0), &vecs, 5, 1.5).unwrap();
        let gold = hits
            .iter()
            .find(|h| h.entity_id == "gold")
            .expect("gold matches the only content term and must surface");
        assert!(
            gold.matched_and,
            "cosine {} >= DENSE_EVIDENCE_MIN must grant evidence even below the dense top-M",
            0.5
        );
    }

    #[test]
    fn test_fused_survives_nan_vector() {
        // A corrupt-but-aligned sidecar row can decode to NaN. dot() must
        // neutralize it so the sort comparator stays a total order (no panic, no
        // garbage top-ranking).
        let mut store = EventStore::in_memory().unwrap();
        let e1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "keyword one")
            .unwrap();
        let e2 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "keyword two")
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut vecs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vecs.upsert_batch(&[(e1.seq, vec![f32::NAN; DIM]), (e2.seq, axis(1.0))]).unwrap();

        let hits = recall_fused(&store, "keyword", &axis(1.0), &vecs, 3, 1.5).unwrap();
        assert!(!hits.is_empty(), "a NaN vector must neither panic nor empty the result");
    }

    #[test]
    fn test_fused_does_not_starve_frequently_revised_head() {
        // The fused lexical leg must keep the bm25 anti-starvation guarantee: an
        // entity with more matching superseded revs than the pool cap must still
        // recall its CURRENT head (a flat LIMIT over raw rows would evict it).
        let mut store = EventStore::in_memory().unwrap();
        let mut parent = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "zeta keyword v0")
            .unwrap()
            .this_hash;
        for i in 1..=(FUSION_POOL + 10) {
            parent = store
                .append_event("s", "l", "a", EventKind::FactRevised, "e1", Some(&parent), &format!("zeta keyword v{}", i))
                .unwrap()
                .this_hash;
        }
        let dir = tempfile::tempdir().unwrap();
        let vecs = VectorStore::open(&dir.path().join("v.db")).unwrap(); // empty -> pure lexical
        let hits = recall_fused(&store, "zeta keyword", &axis(1.0), &vecs, 3, 1.5).unwrap();
        assert_eq!(hits.len(), 1, "the current head must not be starved by superseded revs");
        assert_eq!(hits[0].rev, parent, "the surviving hit is the current head");
        assert!(hits[0].body.contains(&format!("v{}", FUSION_POOL + 10)));
    }

    #[test]
    fn test_fused_matched_and_reflects_real_evidence() {
        // A one-word OR coincidence with ~0 cosine must NOT claim matched_and
        // (else the courier's silence gate is bypassed on semantic builds);
        // an AND match or a high-cosine dense hit MUST claim it.
        let mut store = EventStore::in_memory().unwrap();
        let weak = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "the sync module ships batches")
            .unwrap();
        let dense_gold = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "resident model stays warm")
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut vecs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        // e1: no semantic relation to the query; e2: perfectly aligned
        vecs.upsert_batch(&[(weak.seq, axis(-1.0)), (dense_gold.seq, axis(1.0))]).unwrap();

        // multi-word query where only "sync" matches e1 lexically, nothing matches e2
        let hits = recall_fused(&store, "ga verder met de sync refactor", &axis(1.0), &vecs, 5, 1.5).unwrap();
        let h1 = hits.iter().find(|h| h.entity_id == "e1").expect("lexical hit present");
        assert!(!h1.matched_and, "a one-word OR coincidence with no cosine support is weak evidence");
        let h2 = hits.iter().find(|h| h.entity_id == "e2").expect("dense hit present");
        assert!(h2.matched_and, "a high-cosine dense hit carries real semantic evidence");

        // whole-question lexical match -> strong via the AND set (zero query
        // vector: cosine contributes nothing; the AND pass carries it)
        let both = recall_fused(&store, "sync ships batches", &axis(0.0), &vecs, 5, 1.5).unwrap();
        let h1 = both.iter().find(|h| h.entity_id == "e1").expect("hit present");
        assert!(h1.matched_and, "an AND-pass match is strong evidence even without vectors");
    }

    #[test]
    fn test_dense_leg_head_filter_prevents_slot_starvation() {
        // Regression (dense-leg head-filter bugfix): the dense candidate filter
        // checked scope but NOT head-membership, so the near-identical vectors of
        // a frequently-revised entity's superseded revs filled all DENSE_TOPM
        // slots and were then discarded in finalize_heads - evicting a
        // zero-lexical-overlap gold the dense leg exists to surface.
        let mut store = EventStore::in_memory().unwrap();
        let mut rows: Vec<(i64, Vec<f32>)> = Vec::new();
        let mut parent = {
            let ev = store
                .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "keyword v0")
                .unwrap();
            rows.push((ev.seq, axis(1.0))); // cosine 1.0 with the query
            ev.this_hash
        };
        for i in 1..=(DENSE_TOPM + 10) {
            let ev = store
                .append_event("s", "l", "a", EventKind::FactRevised, "e1", Some(&parent), &format!("keyword v{}", i))
                .unwrap();
            rows.push((ev.seq, axis(1.0)));
            parent = ev.this_hash;
        }
        // the gold: shares NO token with the query, slightly weaker cosine, so it
        // only surfaces if superseded revs do not eat the dense slots.
        let gold = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "resident model stays warm")
            .unwrap();
        let mut gv = vec![0.0f32; DIM];
        gv[0] = 0.9;
        rows.push((gold.seq, gv));

        let dir = tempfile::tempdir().unwrap();
        let mut vecs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vecs.upsert_batch(&rows).unwrap();

        // "zzz" matches nothing lexically: candidates come from the dense leg only.
        let hits = recall_fused(&store, "zzz unmatched", &axis(1.0), &vecs, 5, 1.5).unwrap();
        assert!(
            hits.iter().any(|h| h.entity_id == "e2"),
            "the gold must not be starved out of the dense top-M by superseded revs: {:?}",
            hits.iter().map(|h| &h.entity_id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_fused_skips_superseded_rev_and_guards() {
        let mut store = EventStore::in_memory().unwrap();
        let v1 = store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "zephyr keyword original")
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactRevised, "e1", Some(&v1.this_hash), "keyword updated body")
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut vecs = VectorStore::open(&dir.path().join("v.db")).unwrap();
        vecs.upsert_batch(&[(v1.seq, axis(1.0))]).unwrap();
        let qvec = axis(1.0);

        // "zephyr" lived only in the superseded rev -> not a current head -> no hit
        assert!(recall_fused(&store, "zephyr", &qvec, &vecs, 3, 1.5).unwrap().is_empty());
        // guards: limit 0 and empty query
        assert!(recall_fused(&store, "keyword", &qvec, &vecs, 0, 1.5).unwrap().is_empty());
        assert!(recall_fused(&store, "   ", &qvec, &vecs, 3, 1.5).unwrap().is_empty());
    }
}
