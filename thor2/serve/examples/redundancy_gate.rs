//! R8's own redundancy gate: the script CONTRACT.md requires and that has
//! never actually been run since serving changed. Quoted verbatim from
//! `CONTRACT.md`:
//!
//!   ## R8 - The redundancy gate runs before anything ships
//!
//!   Measured: of 18 rules the file gate served, 14 were already in the repo
//!   it served them to (78%). The action gate scored 4 of 57 (10%). A gate
//!   that repeats the repo pays tokens for redundancy.
//!
//!   Test: the redundancy number is produced by a script over the whole item
//!   corpus, per document, stopwords removed, and it is reported with every
//!   serving change.
//!
//! This is that script. Read-only: REDUNDANCY_DB must point at a COPY of a
//! store, never the live one, opened through `EventStore::open_existing` -
//! the same constructor `stale_anchors.rs` uses and for the same reason (see
//! that file's own doc comment): it does no schema work and no FTS heal on
//! open, the closest thing this crate exposes to a read-only open. Every
//! store read after that (`serve::live::live_items`) is a SELECT-only reader
//! too. REDUNDANCY_ROOT must point at a checkout (or a copy of one) of the
//! repository the store's Path/Dir anchors were originally written against;
//! nothing under it is ever written to, only `std::fs::read_dir`/`std::fs::
//! read` are used. Neither variable falls back to a default path - either
//! one missing and this exits before opening anything, no store open, no
//! directory walk, no file read.
//!
//! SCOPE. Every live item whose kind can fire (`model::item::Kind::can_fire`
//! - Rule or Orientation) is a candidate. Only its Path/Dir Target bindings
//! matter here: a Symbol, Command, Project, Route or Host binding names
//! something this tool has no file to read, and is not counted at all, not
//! even as skipped (`stale_anchors.rs` makes the identical scope decision
//! for the identical reason). An item with more than one Path/Dir anchor
//! that resolves contributes one SCORED ROW PER RESOLVED ANCHOR, not one row
//! per item - the same per-anchor granularity `stale_anchors.rs` already
//! uses for its own sample rows. The distribution and median this tool
//! prints are therefore over (item, document) pairs, never claimed to be
//! over unique items, and the output says so.
//!
//! An anchor that does not resolve under REDUNDANCY_ROOT is skipped and
//! counted separately - resolving anchors is `stale_anchors.rs`'s job, not
//! this one's, and this tool never re-decides that question differently:
//! the tail-match rule below (`resolve_one`) is the exact rule `stale_
//! anchors.rs::resolves` uses, so the two tools can never disagree about
//! whether an anchor resolves, only about what to do once it has.
//!
//! DOCUMENT RESOLUTION - what "the document it is anchored to" means.
//!   - A Path anchor's document is the one resolved file's own content, read
//!     as bytes and lossily decoded as UTF-8 (a stray non-UTF-8 byte becomes
//!     U+FFFD rather than failing the whole file).
//!   - A Dir anchor's document is the CONCATENATION of every regular file
//!     found DIRECTLY inside the resolved directory - one level only,
//!     deliberately never recursive into sub-directories. JUDGEMENT CALL:
//!     this project's own earlier redundancy work names "scoring against
//!     the merged repo" as a real mistake - words scattered over many files
//!     are not "the repo saying it". The same reasoning applies one level
//!     down: turning a whole sub-tree into one document for a Dir anchor
//!     would repeat that mistake at smaller scale. The cost is stated
//!     plainly: a Dir anchor whose real subject lives in a sub-directory
//!     reads as though that content were not there - this can only ever
//!     UNDER-count overlap for a Dir-anchored item, never over-count it.
//!   - An anchor whose tail matches more than one real file or directory
//!     (a short or bare-filename anchor especially, see `stale_anchors.rs`'s
//!     own doc comment on this exact ambiguity) resolves to the SHORTEST
//!     real match (fewest path components); a tie is broken on the
//!     lexicographically smaller normalised path, so the pick is fully
//!     deterministic and never depends on filesystem walk order. This is a
//!     stated limitation, not a hidden one: for a genuinely ambiguous short
//!     anchor, the document this tool reads may not be the one the item's
//!     author meant.
//!   - A resolved Path that cannot be read (permissions, a size over
//!     MAX_DOCUMENT_BYTES) or a resolved Dir with no regular file directly
//!     inside it produces no document; the item is skipped and counted
//!     separately, never scored with a fabricated 0.0.
//!
//! MEASUREMENT - the comparison itself, spelled out because a single number
//! here is exactly the kind of claim this project keeps having to walk back.
//!   - A CONTENT WORD is a maximal run of ASCII letters, digits or
//!     underscore, at least MIN_TOKEN_LEN (2) characters long. Every other
//!     character (space, '.', ',', ':', '/', '\', '-', parentheses, quotes,
//!     and any non-ASCII character) is a separator, never part of a token.
//!     Underscore is kept INSIDE a token on purpose - `event_store` stays
//!     one word, not "event" and "store" separately - because snake_case is
//!     this codebase's own identifier convention, and splitting it apart
//!     would match each half against unrelated uses of the same short
//!     English word elsewhere. A hyphen is NOT kept inside a token (unlike
//!     underscore): "same-topic-cluster" splits into three words, the
//!     ordinary English convention for a hyphenated phrase.
//!   - CASE IS PRESERVED, not folded. "Kind" (the Rust type this workspace
//!     defines) and "kind" (the ordinary English word) are counted as two
//!     DIFFERENT content words, on purpose: lowercasing would conflate an
//!     exact identifier with an unrelated word and inflate overlap in
//!     exactly the direction this project cannot afford to overclaim in.
//!     The stated cost of this choice: two mentions of the same ordinary
//!     word that differ only by sentence-initial capitalisation ("The" vs
//!     "the") count as different tokens and will NOT be counted as
//!     overlapping. That is a conservative error - it can only ever UNDER-
//!     count overlap for ordinary prose, never over-count it - so the
//!     number this tool reports, if it is wrong, is wrong in the safer
//!     direction.
//!   - STOPWORD removal IS case-insensitive (a token is matched against the
//!     stopword list after lowercasing a throwaway copy of it), because a
//!     stopword's whole job is to be dropped regardless of where it sits in
//!     a sentence - "The" at a sentence start is exactly as empty as "the"
//!     mid-sentence. The token that SURVIVES keeps its original case; only
//!     the yes/no decision to drop it is case-blind. `STOPWORDS` (below) is
//!     this tool's own compact list of common English function words -
//!     articles, conjunctions, prepositions, pronouns, auxiliary/modal
//!     verbs, and a handful of generic hedge words - chosen because this
//!     workspace's own items and documents are written in English (see this
//!     project's own CLAUDE.md and CONTRACT.md itself). It is deliberately
//!     NOT bilingual: a Dutch word occurring in an item or document leaks
//!     through as if it were a content word, which can only ever LOWER the
//!     measured overlap for that pair (an unmatched stray function word
//!     counted on one side), never raise it - the same safe direction as
//!     the case decision above.
//!   - Each side (the item's text, the document's content) is reduced to a
//!     SET of DISTINCT content words, not a bag counting repeats - whether a
//!     word appears once or fifty times in the document, it either is or is
//!     not something the document says.
//!   - The OVERLAP FRACTION for one (item, document) pair is
//!     |item words that also appear in the document's word set| divided by
//!     |the item's own distinct content words|. An item with zero content
//!     words after stopword removal has no defined fraction (0/0) and is
//!     skipped, counted separately - never reported as a fabricated 0.0.
//!
//! REPORTING. A distribution of how many (item, document) pairs fall into
//! each overlap band, the median, and separately the count at or above ONE
//! named threshold - stated plainly in the output as this report's own
//! judgement call, not a measured truth, exactly as R8's own test demands
//! nothing be presented as. Then the most redundant pairs, at most 20, each
//! with its id, the resolved document (root-relative), the fraction, and
//! the first 80 characters of the item's own text, so a person can check
//! the number against the words themselves. Nothing else: no
//! recommendation, no verdict - only the one closing line this tool always
//! prints about what word overlap does and does not mean.
//!
//! Run (from the `thor2` workspace root):
//!   REDUNDANCY_DB=<path to a COPY of a store> \
//!   REDUNDANCY_ROOT=<path to a checkout the anchors were written against> \
//!   cargo run -p serve --example redundancy_gate

use model::item::{Binding, TargetKind};
use model::normalize::normalize_target;
use serve::live::live_items;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

// ------------------------------------------------------------ measurement

/// See this file's own "MEASUREMENT" doc-comment section for what counts as
/// a content word and why 2 was chosen: a single character is dropped as
/// noise (and is almost always a stopword anyway), while this corpus has
/// real 2-character content words worth keeping ("db", "io", "id", "rs").
const MIN_TOKEN_LEN: usize = 2;

/// This tool's own compact English stopword list - see "MEASUREMENT" above
/// for why it exists, why it is matched case-insensitively, and why it is
/// English-only rather than bilingual.
const STOPWORDS: &[&str] = &[
    // articles
    "a", "an", "the", //
    // conjunctions
    "and", "or", "but", "if", "then", "else", "because", "so", "than", "as", "nor", "yet",
    // prepositions
    "of", "in", "on", "at", "by", "for", "with", "about", "against", "between", "into", "through", "during", "before",
    "after", "above", "below", "to", "from", "up", "down", "over", "under", "again", "further", "once", "out", "off",
    "per", "via", //
    // pronouns
    "i", "me", "my", "we", "our", "you", "your", "he", "him", "his", "she", "her", "it", "its", "they", "them",
    "their", "this", "that", "these", "those", "who", "whom", "whose", "which", "what", //
    // auxiliary / modal / copula verbs
    "is", "am", "are", "was", "were", "be", "been", "being", "have", "has", "had", "having", "do", "does", "did",
    "doing", "will", "would", "shall", "should", "may", "might", "must", "can", "could", //
    // negation
    "not", "no", //
    // generic hedges / high-frequency filler
    "very", "just", "too", "own", "same", "such", "only", "also", "more", "most", "other", "some", "any", "each",
    "both", "few", "there", "here", "when", "where", "why", "how", "all",
];

/// Case-insensitive stopword check - see "MEASUREMENT": the decision to drop
/// a token is case-blind, but the token itself (if kept) is never modified.
fn is_stopword(token: &str) -> bool {
    let lower = token.to_lowercase();
    STOPWORDS.contains(&lower.as_str())
}

/// Reduce `text` to its distinct content words. See this file's own
/// "MEASUREMENT" doc-comment section for the exact rule and every judgement
/// call inside it (token shape, minimum length, case, underscore vs hyphen,
/// stopword removal).
fn content_words(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            consider_token(&mut out, &current);
            current.clear();
        }
    }
    if !current.is_empty() {
        consider_token(&mut out, &current);
    }
    out
}

fn consider_token(out: &mut HashSet<String>, token: &str) {
    if token.chars().count() < MIN_TOKEN_LEN || is_stopword(token) {
        return;
    }
    out.insert(token.to_string());
}

/// |item words also in the document| / |item's own distinct content words|.
/// `None` when `item_words` is empty - an undefined ratio, never reported as
/// a fabricated 0.0. See "MEASUREMENT" above.
fn overlap_fraction(item_words: &HashSet<String>, doc_words: &HashSet<String>) -> Option<f64> {
    if item_words.is_empty() {
        return None;
    }
    let shared = item_words.intersection(doc_words).count();
    Some(shared as f64 / item_words.len() as f64)
}

// ------------------------------------------------------------- filesystem

/// One real file or directory found under the walked root: its normalised,
/// tail-matchable component sequence (built exactly like `stale_anchors.rs`'s
/// own `components()` helper, so the two tools can never disagree about
/// what "the same target" means) alongside the real path this tool actually
/// reads from.
struct Entry {
    components: Vec<String>,
    real_path: PathBuf,
}

struct Listing {
    files: Vec<Entry>,
    files_by_last: HashMap<String, Vec<usize>>,
    dirs: Vec<Entry>,
    dirs_by_last: HashMap<String, Vec<usize>>,
}

fn components(value: &str) -> Vec<String> {
    normalize_target(value).split('/').filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn index_by_last(entries: &[Entry]) -> HashMap<String, Vec<usize>> {
    let mut map: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if let Some(last) = e.components.last() {
            map.entry(last.clone()).or_default().push(i);
        }
    }
    map
}

/// Walk one directory's entries, recursing into real sub-directories. A
/// directory this cannot even read (permissions, a path past Windows'
/// legacy MAX_PATH) is treated as empty rather than aborting the run - the
/// same fails-open stance `stale_anchors.rs::walk` takes. Symlinks,
/// junctions and other reparse points are neither specially followed nor
/// specially excluded: whatever `DirEntry::file_type()` reports on this
/// platform decides, and anything it reports as neither a file nor a
/// directory is silently not indexed, same as that file.
fn walk(dir: &Path, rel: &Path, files: &mut Vec<Entry>, dirs: &mut Vec<Entry>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        let name = entry.file_name();
        let child_rel = if rel.as_os_str().is_empty() { PathBuf::from(&name) } else { rel.join(&name) };
        let real_path = entry.path();
        if file_type.is_dir() {
            dirs.push(Entry { components: components(&child_rel.to_string_lossy()), real_path: real_path.clone() });
            walk(&real_path, &child_rel, files, dirs);
        } else if file_type.is_file() {
            files.push(Entry { components: components(&child_rel.to_string_lossy()), real_path });
        }
        // Neither: a symlink/junction/reparse point (or something else
        // file_type() cannot classify) - silently not indexed, see above.
    }
}

fn build_listing(root: &Path) -> Listing {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    walk(root, Path::new(""), &mut files, &mut dirs);
    let files_by_last = index_by_last(&files);
    let dirs_by_last = index_by_last(&dirs);
    Listing { files, files_by_last, dirs, dirs_by_last }
}

/// Resolve one anchor to the single real entry this tool treats as "the
/// document it is anchored to". Uses the exact same tail-match rule
/// `stale_anchors.rs::resolves` uses (a real path at least as long as the
/// anchor, whose trailing components equal the anchor's own exactly), so
/// the two tools can never disagree about whether an anchor resolves. See
/// this file's own "DOCUMENT RESOLUTION" doc-comment section for the
/// tie-break rule used when more than one real entry matches.
fn resolve_one<'a>(listing: &'a Listing, kind: TargetKind, anchor: &[String]) -> Option<&'a Entry> {
    let last = anchor.last()?;
    let (by_last, entries) = match kind {
        TargetKind::Path => (&listing.files_by_last, &listing.files),
        TargetKind::Dir => (&listing.dirs_by_last, &listing.dirs),
        _ => unreachable!("resolve_one is only ever called with a Path or Dir anchor"),
    };
    let candidates = by_last.get(last)?;
    let mut matches: Vec<&Entry> = candidates
        .iter()
        .map(|&i| &entries[i])
        .filter(|e| e.components.len() >= anchor.len() && e.components[e.components.len() - anchor.len()..] == *anchor)
        .collect();
    if matches.is_empty() {
        return None;
    }
    // JUDGEMENT CALL - see "DOCUMENT RESOLUTION": shortest real path wins,
    // ties broken on the joined path text, so the pick never depends on
    // filesystem walk order.
    matches.sort_by(|a, b| {
        a.components.len().cmp(&b.components.len()).then_with(|| a.components.join("/").cmp(&b.components.join("/")))
    });
    matches.into_iter().next()
}

/// A self-chosen safety bound on how much of one file this tool will read
/// into memory as prose - not a measured value, named here so it is never
/// mistaken for one. Guards only against a pathologically large file (a
/// vendored blob, a generated artifact); no ordinary document in a checkout
/// should ever come close to it.
const MAX_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;

fn read_capped(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_DOCUMENT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The content this tool treats as "the document" a resolved anchor points
/// to - see this file's own "DOCUMENT RESOLUTION" doc-comment section for
/// what a Path vs a Dir anchor's document means and every judgement call
/// behind that choice. `None` when no content can be produced at all (an
/// unreadable/undecodable/oversized file, or a directory with no regular
/// file directly inside it) - the caller skips and counts this, never
/// scores it as an empty document.
fn document_content(kind: TargetKind, entry: &Entry) -> Option<String> {
    match kind {
        TargetKind::Path => read_capped(&entry.real_path),
        TargetKind::Dir => {
            let Ok(read) = std::fs::read_dir(&entry.real_path) else { return None };
            let mut files: Vec<PathBuf> =
                read.flatten().filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false)).map(|e| e.path()).collect();
            files.sort(); // deterministic order, never raw directory-listing order
            let mut combined = String::new();
            for path in &files {
                if let Some(text) = read_capped(path) {
                    combined.push_str(&text);
                    combined.push('\n');
                }
            }
            if combined.is_empty() {
                None
            } else {
                Some(combined)
            }
        }
        _ => unreachable!("document_content is only ever called with a Path or Dir anchor"),
    }
}

// ----------------------------------------------------------------- output

/// One scored (item, resolved document) pair - see this file's own doc
/// comment, "SCOPE", for why this is per RESOLVED ANCHOR, not per item.
struct Scored {
    id: String,
    document: String,
    fraction: f64,
    text: String,
}

/// This report's own named judgement call (see "REPORTING" above): not a
/// measured value, and never presented as one in the output.
const HIGH_OVERLAP_THRESHOLD: f64 = 0.70;

/// See "REPORTING": five bands of width 0.2, half-open except the last,
/// which is closed at 1.0 so a perfect-overlap item has somewhere to land.
fn band_label(fraction: f64) -> &'static str {
    if fraction < 0.2 {
        "0.0-0.2"
    } else if fraction < 0.4 {
        "0.2-0.4"
    } else if fraction < 0.6 {
        "0.4-0.6"
    } else if fraction < 0.8 {
        "0.6-0.8"
    } else {
        "0.8-1.0"
    }
}

const BAND_ORDER: [&str; 5] = ["0.0-0.2", "0.2-0.4", "0.4-0.6", "0.6-0.8", "0.8-1.0"];

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).expect("an overlap fraction is a count ratio in [0,1], never NaN"));
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[mid - 1] + values[mid]) / 2.0)
    } else {
        Some(values[mid])
    }
}

fn main() -> anyhow::Result<()> {
    let db = PathBuf::from(std::env::var("REDUNDANCY_DB").expect(
        "set REDUNDANCY_DB to the path of a COPY of a store (never the live thor.db) - \
         this tool refuses to run without one and never falls back to a default path",
    ));
    let root = PathBuf::from(std::env::var("REDUNDANCY_ROOT").expect(
        "set REDUNDANCY_ROOT to the filesystem root anchor values should be resolved \
         against - this tool refuses to run without one and never falls back to a default path",
    ));

    let store = EventStore::open_existing(&db)?;
    let listing = build_listing(&root);

    let mut skipped_no_resolve = 0usize;
    let mut skipped_no_document = 0usize;
    let mut skipped_empty_item = 0usize;
    let mut scored: Vec<Scored> = Vec::new();

    for live in live_items(&store) {
        if !live.item.kind.can_fire() {
            continue; // only Rule/Orientation can ever carry an anchor that matters here
        }
        let anchors: Vec<(TargetKind, String)> = live
            .item
            .bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Target { kind, value } if matches!(kind, TargetKind::Path | TargetKind::Dir) => {
                    Some((*kind, value.clone()))
                }
                _ => None,
            })
            .collect();

        for (kind, value) in anchors {
            let anchor = components(&value);
            let Some(entry) = resolve_one(&listing, kind, &anchor) else {
                skipped_no_resolve += 1;
                continue;
            };
            let Some(doc_text) = document_content(kind, entry) else {
                skipped_no_document += 1;
                continue;
            };
            let item_words = content_words(&live.item.text);
            let doc_words = content_words(&doc_text);
            let Some(fraction) = overlap_fraction(&item_words, &doc_words) else {
                skipped_empty_item += 1;
                continue;
            };
            let document =
                entry.real_path.strip_prefix(&root).unwrap_or(&entry.real_path).to_string_lossy().replace('\\', "/");
            scored.push(Scored { id: live.id.clone(), document, fraction, text: live.item.text.clone() });
        }
    }

    println!("REDUNDANCY_DB: {}", db.display());
    println!("REDUNDANCY_ROOT: {}", root.display());
    println!("indexed under REDUNDANCY_ROOT: {} files, {} directories", listing.files.len(), listing.dirs.len());
    println!();
    println!(
        "method: each side is reduced to a SET of distinct content words - a maximal run of \
         ASCII letters, digits or underscore, at least {MIN_TOKEN_LEN} characters, with a \
         {}-word English stopword list removed by case-insensitive match; a surviving word \
         KEEPS its original case (\"Kind\" the type and \"kind\" the word count as different \
         content words on purpose - see this file's own doc comment, MEASUREMENT). Every other \
         character, including hyphen, is a separator; underscore alone stays inside a token, so \
         snake_case identifiers stay one word. The overlap fraction for one (item, document) \
         pair is |item words that also appear in the document| / |the item's own distinct \
         content words|.",
        STOPWORDS.len()
    );
    println!();
    println!(
        "document resolution: a Path anchor's document is that one resolved file's content. A \
         Dir anchor's document is the concatenation of every regular file found DIRECTLY inside \
         the resolved directory, one level only, never recursive - a deliberate choice to never \
         merge a whole sub-tree into one document (see this file's own doc comment, DOCUMENT \
         RESOLUTION). An anchor matching more than one real file or directory resolves to the \
         shortest real match, ties broken on path text, for a deterministic pick."
    );
    println!();
    println!("scope: Rule/Orientation items only, Path/Dir anchors only; one row per RESOLVED ANCHOR, not per item.");
    println!(
        "anchors skipped, did not resolve under REDUNDANCY_ROOT (resolving anchors is stale_anchors.rs's job, not this one's): {skipped_no_resolve}"
    );
    println!(
        "anchors skipped, resolved but no document content could be produced (unreadable, undecodable, oversized, or an empty directory): {skipped_no_document}"
    );
    println!(
        "anchors skipped, item has no content words to compare (an undefined ratio, never reported as 0.0): {skipped_empty_item}"
    );
    println!();
    println!("scored (item, document) pairs: {}", scored.len());

    if scored.is_empty() {
        println!();
        println!(
            "word overlap counts shared vocabulary, not shared meaning: a rule can restate a \
             document's exact words and still add a judgement the document never makes, and a \
             rule can say the same thing in different words and score near zero here. This run \
             scored nothing, so no distribution, threshold count or list follows."
        );
        return Ok(());
    }

    let mut band_counts: HashMap<&str, usize> = HashMap::new();
    for s in &scored {
        *band_counts.entry(band_label(s.fraction)).or_insert(0) += 1;
    }
    println!();
    println!("distribution by overlap fraction band:");
    for label in BAND_ORDER {
        println!("  {label}: {}", band_counts.get(label).copied().unwrap_or(0));
    }
    let med = median(scored.iter().map(|s| s.fraction).collect()).expect("scored is non-empty, checked above");
    println!("median overlap fraction: {med:.3}");

    let above = scored.iter().filter(|s| s.fraction >= HIGH_OVERLAP_THRESHOLD).count();
    println!();
    println!(
        "count at or above {HIGH_OVERLAP_THRESHOLD:.2} (a judgement call made for this report, not a measured truth): {above} of {}",
        scored.len()
    );

    let mut ranked: Vec<&Scored> = scored.iter().collect();
    ranked.sort_by(|a, b| b.fraction.partial_cmp(&a.fraction).expect("fractions are never NaN").then_with(|| a.id.cmp(&b.id)));
    println!();
    println!("most redundant (item, document) pairs, at most 20:");
    for s in ranked.into_iter().take(20) {
        let snippet: String = s.text.chars().take(80).collect();
        println!("id={} document={} overlap={:.3}", s.id, s.document, s.fraction);
        println!("  text: {snippet}");
    }

    println!();
    println!(
        "word overlap counts shared vocabulary, not shared meaning: a rule can restate a \
         document's exact words and still add a judgement or an instruction the document never \
         makes, and a rule can say the same thing as a document in entirely different words and \
         score near zero here. Read this as a vocabulary signal, not a redundancy verdict."
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind, TargetKind};
    use model::store;

    // ------------------------------------------------------ content_words

    #[test]
    fn stopwords_are_removed_regardless_of_case() {
        let words = content_words("The rule is that a gate must never fail silently");
        assert!(!words.contains("the"));
        assert!(!words.contains("The"));
        assert!(!words.contains("is"));
        assert!(!words.contains("that"));
        assert!(!words.contains("a"));
        assert!(!words.contains("must"));
        assert!(words.contains("never"), "\"never\" is content, not a stopword, and must survive");
        assert!(words.contains("rule"));
        assert!(words.contains("gate"));
        assert!(words.contains("fail"));
        assert!(words.contains("silently"));
    }

    #[test]
    fn case_is_preserved_not_folded() {
        let words = content_words("Kind kind KIND");
        assert!(words.contains("Kind"));
        assert!(words.contains("kind"));
        assert!(words.contains("KIND"));
        assert_eq!(words.len(), 3, "three distinct casings must stay three distinct content words");
    }

    #[test]
    fn underscore_stays_inside_a_token_but_hyphen_splits() {
        // "gate-topic-cluster", not "same-topic-cluster": "same" is itself one
        // of this tool's own stopwords (see STOPWORDS, "generic hedges"), so
        // it would be filtered regardless of the hyphen question this test
        // exists to pin - picking a probe word that is also a stopword would
        // test the wrong thing.
        let words = content_words("event_store gate-topic-cluster");
        assert!(words.contains("event_store"), "snake_case must survive as one token");
        assert!(!words.contains("gate-topic-cluster"));
        assert!(words.contains("gate"));
        assert!(words.contains("topic"));
        assert!(words.contains("cluster"));
    }

    #[test]
    fn single_character_tokens_are_dropped() {
        let words = content_words("a b c gate");
        assert!(!words.contains("b"));
        assert!(!words.contains("c"));
        assert!(words.contains("gate"));
    }

    #[test]
    fn punctuation_never_survives_inside_a_token() {
        let words = content_words("model::item::Kind, gate.rs (see README).");
        assert!(words.contains("model"));
        assert!(words.contains("item"));
        assert!(words.contains("Kind"));
        assert!(words.contains("gate"));
        assert!(words.contains("rs"));
        assert!(words.contains("README"));
        assert!(!words.iter().any(|w| w.contains(':') || w.contains('.') || w.contains('(') || w.contains(')')));
    }

    #[test]
    fn repetition_does_not_change_the_word_set() {
        let once = content_words("gate gate gate");
        assert_eq!(once.len(), 1);
    }

    // -------------------------------------------------------- overlap_fraction

    #[test]
    fn overlap_fraction_is_shared_over_the_items_own_count() {
        let item: HashSet<String> = ["gate", "fail", "silently"].iter().map(|s| s.to_string()).collect();
        let doc: HashSet<String> = ["gate", "fail", "loudly"].iter().map(|s| s.to_string()).collect();
        assert_eq!(overlap_fraction(&item, &doc), Some(2.0 / 3.0));
    }

    #[test]
    fn overlap_fraction_is_none_for_an_empty_item_word_set() {
        let item: HashSet<String> = HashSet::new();
        let doc: HashSet<String> = ["gate"].iter().map(|s| s.to_string()).collect();
        assert_eq!(overlap_fraction(&item, &doc), None, "0/0 must never be reported as a fabricated 0.0");
    }

    #[test]
    fn overlap_fraction_handles_an_empty_document_as_a_real_zero() {
        let item: HashSet<String> = ["gate"].iter().map(|s| s.to_string()).collect();
        let doc: HashSet<String> = HashSet::new();
        assert_eq!(overlap_fraction(&item, &doc), Some(0.0), "nothing in an empty document is a real answer, not an error");
    }

    // ---------------------------------------------------------------- median

    #[test]
    fn median_of_odd_count_is_the_middle_value() {
        assert_eq!(median(vec![0.1, 0.5, 0.9]), Some(0.5));
    }

    #[test]
    fn median_of_even_count_is_the_average_of_the_two_middle_values() {
        assert_eq!(median(vec![0.2, 0.4, 0.6, 0.8]), Some(0.5));
    }

    #[test]
    fn median_of_empty_is_none() {
        assert_eq!(median(vec![]), None);
    }

    // ------------------------------------------------------------ band_label

    #[test]
    fn band_boundaries_are_half_open_except_the_last() {
        assert_eq!(band_label(0.0), "0.0-0.2");
        assert_eq!(band_label(0.199), "0.0-0.2");
        assert_eq!(band_label(0.2), "0.2-0.4");
        assert_eq!(band_label(0.79999), "0.6-0.8");
        assert_eq!(band_label(0.8), "0.8-1.0");
        assert_eq!(band_label(1.0), "0.8-1.0");
    }

    // --------------------------------------------------- filesystem/resolve

    #[test]
    fn resolve_one_prefers_the_shortest_real_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        std::fs::write(tmp.path().join("gate.rs"), "short match").unwrap();
        std::fs::write(tmp.path().join("a/b/gate.rs"), "long match").unwrap();
        let listing = build_listing(tmp.path());
        let anchor = components("gate.rs");
        let entry = resolve_one(&listing, TargetKind::Path, &anchor).expect("must resolve");
        assert_eq!(entry.components, vec!["gate.rs".to_string()], "the shortest real match must win");
    }

    #[test]
    fn resolve_one_breaks_a_same_length_tie_on_path_text() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b")).unwrap();
        std::fs::write(tmp.path().join("a/gate.rs"), "one").unwrap();
        std::fs::write(tmp.path().join("b/gate.rs"), "two").unwrap();
        let listing = build_listing(tmp.path());
        let anchor = components("gate.rs");
        let entry = resolve_one(&listing, TargetKind::Path, &anchor).expect("must resolve");
        assert_eq!(
            entry.components,
            vec!["a".to_string(), "gate.rs".to_string()],
            "the lexicographically smaller path must win a same-length tie"
        );
    }

    #[test]
    fn resolve_one_is_none_when_nothing_matches() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("other.rs"), "content").unwrap();
        let listing = build_listing(tmp.path());
        let anchor = components("gate.rs");
        assert!(resolve_one(&listing, TargetKind::Path, &anchor).is_none());
    }

    // ----------------------------------------------------- document_content

    #[test]
    fn a_path_documents_content_is_the_files_own_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("doc.md"), "the rule lives here").unwrap();
        let listing = build_listing(tmp.path());
        let entry = resolve_one(&listing, TargetKind::Path, &components("doc.md")).unwrap();
        assert_eq!(document_content(TargetKind::Path, entry), Some("the rule lives here".to_string()));
    }

    #[test]
    fn a_dir_documents_content_is_direct_files_only_never_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("docs/nested")).unwrap();
        std::fs::write(tmp.path().join("docs/top.md"), "top level content").unwrap();
        std::fs::write(tmp.path().join("docs/nested/deep.md"), "buried content").unwrap();
        let listing = build_listing(tmp.path());
        let entry = resolve_one(&listing, TargetKind::Dir, &components("docs")).unwrap();
        let doc = document_content(TargetKind::Dir, entry).unwrap();
        assert!(doc.contains("top level content"));
        assert!(!doc.contains("buried content"), "a Dir document must never pull in a sub-directory's content");
    }

    #[test]
    fn a_dir_with_no_direct_files_has_no_document() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("empty/nested")).unwrap();
        std::fs::write(tmp.path().join("empty/nested/deep.md"), "buried content").unwrap();
        let listing = build_listing(tmp.path());
        let entry = resolve_one(&listing, TargetKind::Dir, &components("empty")).unwrap();
        assert_eq!(document_content(TargetKind::Dir, entry), None);
    }

    // --------------------------------------------------------- end to end

    #[test]
    fn a_declared_rule_scores_against_its_own_anchored_files_real_content() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("STYLE.md"), "never use an em dash in committed prose").unwrap();

        let mut db = EventStore::in_memory().unwrap();
        let item = Item {
            id: "r1".to_string(),
            kind: Kind::Rule,
            text: "never use an em dash".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "STYLE.md".to_string() }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("an em dash lands in committed prose and survives review".to_string()),
            check: None,
        };
        store::declare(&mut db, "s", "l", "a", &item).unwrap();

        let listing = build_listing(tmp.path());
        let live = live_items(&db);
        assert_eq!(live.len(), 1);
        let anchor = components("STYLE.md");
        let entry = resolve_one(&listing, TargetKind::Path, &anchor).expect("STYLE.md must resolve");
        let doc_text = document_content(TargetKind::Path, entry).expect("STYLE.md has content");
        let fraction = overlap_fraction(&content_words(&live[0].item.text), &content_words(&doc_text)).unwrap();
        // item content words: never, use, em, dash (4). All four appear in the
        // document's own text, so this must resolve to a full 1.0 - not a
        // measured value, a fixture built by hand to pin the formula end to
        // end.
        assert_eq!(fraction, 1.0);
    }
}
