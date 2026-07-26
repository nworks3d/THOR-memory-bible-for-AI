//! One home for the surface vocabularies.
//!
//! Four surfaces grew four private word lists - the courier's triviality gate,
//! recall's stopwords + knowledge routing, the structure-card detection, the
//! guard's command-noise filter - and none of them could see the others. The
//! measured cost of that blindness: "blast radius" lived in the guard's
//! philosophy for weeks while the structure card missed every blast-radius
//! question until 9eff76a added the two words by hand. Central residence does
//! not merge the lists (each has a distinct, measured role - merging them
//! WOULD change behavior), it makes the next gap visible: a word class added
//! to one list is reviewed against its siblings in the same file.
//!
//! Every list moved here verbatim; behavior is byte-identical by construction
//! and pinned by the committed drift eval + the suites of the moved-from
//! modules.

/// Words that, when they make up the WHOLE prompt, mean "no recall worth
/// doing" (acks / git verbs / greetings). Ported 1:1 from hook_recall.ps1 so
/// THOR's gating matches the live mimir hook it ran beside. Used by the
/// courier's is_all_trivial gate ONLY - these are conversation words, not
/// search stopwords (see STOPWORDS for that role).
pub const TRIVIAL_WORDS: &[&str] = &[
    "ok", "oke", "okay", "k", "kk", "thanks", "thx", "ty", "bedankt", "dank", "dankje", "ja",
    "jawel", "jep", "yes", "yep", "yup", "nee", "neen", "no", "nope", "nop", "commit", "push",
    "pull", "merge", "stage", "staged", "rebase", "doe", "maar", "dit", "dat", "het", "graag",
    "please", "svp", "aub", "mooi", "top", "goed", "prima", "perfect", "klopt", "super", "fijn",
    "nice", "great", "good",
];

/// Function words (EN + NL) that carry no search evidence: recall drops them
/// from queries and the coverage gate refuses to count them as content.
///
/// Public so the eval harness scores with the same list rather than a second
/// copy that drifts. It is not a substitute for a frequency cut and a
/// frequency cut is not a substitute for it: measured on a 5,586-head store,
/// 19 of these sit below 10% document frequency ("what" 9.1%, "which" 6.9%,
/// "zijn" 6.2%) and would still count as evidence, while "repo" at 89% needs
/// the frequency cut to be caught.
/// (The moved-from list carried literal duplicates - "did", "was", and the
/// EN/NL homographs - which `contains` made harmless; deduped here where the
/// no-duplicates test below can keep it that way. Same member set, so every
/// lookup answers identically.)
pub const STOPWORDS: &[&str] = &[
    // English
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "at", "for", "with", "is", "are", "was",
    "were", "be", "been", "do", "did", "does", "how", "what", "why", "when", "where", "which",
    "that", "this", "it", "we", "you", "about", "from", "have", "has", "had", "not", "no", "our",
    "my", "your", "as", "by", "so", "if", "up", "out",
    // Dutch (words shared with English - "was", "of", "die"-class - are listed once above)
    "de", "het", "een", "en", "van", "voor", "met", "zijn", "waren", "hoe", "wat",
    "waarom", "wanneer", "waar", "welke", "dat", "dit", "ook", "er", "al", "nog", "dan", "dus",
    "maar", "die", "naar", "niet", "geen", "ons", "mijn", "jij", "over", "om", "te", "op", "aan",
];

/// Decision/constraint vocabulary (EN + NL), matched on word boundaries over
/// the lowercased query: recall's fused ranker routes a query carrying one of
/// these to the knowledge prior. Deliberately narrow: generic verbs ("use",
/// "werkt") must not route ordinary code questions to knowledge.
pub const KNOWLEDGE_WORDS: &[&str] = &[
    "beslissing", "besloten", "besluit", "beslist", "afspraak", "afgesproken", "regel",
    "voorkeur", "werkvoorkeur", "gotcha", "waarom", "conventie", "beleid", "werkwijze",
    "decision", "decided", "agreed", "agreement", "rule", "preference", "convention",
    "policy", "why", "rationale", "constraint",
];

/// The vocabulary of ASKING ABOUT shape - "calls", "callers", "defined",
/// "impact", "blast radius" - the structure card's first gate. Not generic
/// code words: the second gate (sidecar resolution) does the real filtering,
/// this one only keeps the card off plain knowledge questions. The two
/// blast-radius words arrived via a measured miss (a 40-question battery in
/// which every detection failure was that one phrasing) - the incident that
/// argued for this module.
pub const STRUCTURE_WORDS: &[&str] = &[
    // EN
    "call", "calls", "called", "caller", "callers", "uses", "used", "usage", "define",
    "defines", "defined", "definition", "declared", "declaration", "implemented",
    "implements", "impact", "structure", "where", "blast", "radius",
    // NL
    "aanroept", "aangeroepen", "roept", "gebruikt", "definieert", "gedefinieerd",
    "geimplementeerd", "waar", "structuur",
];

/// Words never tried as a symbol, however identifier-shaped the tokenizer
/// finds them: question glue around the structure vocabulary.
pub const NOT_A_SYMBOL: &[&str] = &[
    "the", "this", "that", "what", "which", "who", "how", "does", "are", "is", "in", "of",
    "for", "from", "and", "function", "functions", "method", "methods", "symbol", "file",
    "code", "wat", "wie", "hoe", "welke", "functie", "functies", "bestand", "waarom",
];

/// Shell verbs and generic words too common to identify a constraint: a
/// salient command token must clear these AND the recall stopword lists.
/// Precision over recall by design - the guard's command advisory interrupts,
/// so a false fire costs trust.
pub const COMMAND_NOISE: &[&str] = &[
    "sudo", "bash", "powershell", "cmd", "echo", "cat", "type", "grep", "find", "ls", "dir",
    "cd", "cp", "mv", "rm", "mkdir", "touch", "head", "tail", "curl", "wget", "python", "node",
    "npm", "cargo", "git", "docker", "compose", "build", "run", "test", "install", "update",
    "status", "start", "stop", "restart", "list", "show", "get", "set", "add", "remove", "push",
    "pull", "commit", "checkout", "branch", "log", "diff", "clone", "fetch", "merge", "config",
    "select", "where", "print", "write", "read", "file", "files", "output", "input", "true",
    "false", "null", "name", "force", "quiet", "verbose", "version", "help",
];

/// Extensions whose chunks are prose, not code. Everything else under a chunk
/// id counts as code (the conservative default: code never gets a knowledge
/// boost, and the guard never serves it).
pub const DOC_EXTS: &[&str] = &["md", "markdown", "txt", "rst", "adoc", "org"];

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[(&str, &[&str])] = &[
        ("TRIVIAL_WORDS", TRIVIAL_WORDS),
        ("STOPWORDS", STOPWORDS),
        ("KNOWLEDGE_WORDS", KNOWLEDGE_WORDS),
        ("STRUCTURE_WORDS", STRUCTURE_WORDS),
        ("NOT_A_SYMBOL", NOT_A_SYMBOL),
        ("COMMAND_NOISE", COMMAND_NOISE),
        ("DOC_EXTS", DOC_EXTS),
    ];

    /// `contains` made duplicates harmless but they hide real edit mistakes
    /// (the moved-in STOPWORDS carried "did" and "was" twice for weeks).
    #[test]
    fn no_list_carries_duplicates() {
        for (name, list) in ALL {
            let mut seen = std::collections::HashSet::new();
            for w in *list {
                assert!(seen.insert(*w), "{} carries '{}' twice", name, w);
            }
        }
    }

    /// Every consumer lowercases its haystack before the lookup, so an entry
    /// with an uppercase letter can never match anything - the list-shaped
    /// twin of the dead-anchor class found in the store (a field written in a
    /// shape its own matcher cannot hit).
    #[test]
    fn every_entry_is_lowercase_or_it_is_dead() {
        for (name, list) in ALL {
            for w in *list {
                assert_eq!(*w, w.to_lowercase(), "{} entry '{}' can never match", name, w);
            }
        }
    }

    /// The blast-radius incident, pinned: a 40-question battery generated from
    /// the symbol sidecar found every detection miss was one phrasing whose
    /// words the structure vocabulary lacked. These are the asking-forms that
    /// battery measured (EN + NL); each must pass the card's vocabulary gate.
    #[test]
    fn structure_gate_covers_the_measured_asking_forms() {
        let forms = [
            "who calls process_order",
            "which functions call process_order",
            "where is process_order used",
            "where is process_order defined",
            "what is the impact of changing process_order",
            "what is the blast radius if I change process_order",
            "wie roept process_order aan",
            "waar wordt process_order gebruikt",
            "waar is process_order gedefinieerd",
        ];
        for form in forms {
            let lower = form.to_lowercase();
            let hit = lower
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| !w.is_empty())
                .any(|w| STRUCTURE_WORDS.contains(&w));
            assert!(hit, "structure gate misses the asking-form: {:?}", form);
        }
    }
}
