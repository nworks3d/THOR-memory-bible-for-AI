//! Surface 3: PER PROMPT. Silent by construction unless the prompt itself
//! resolves to a moment or names a target some live item declares -
//! CONTRACT.md's "STIL, tenzij de prompt zelf oplost naar een moment of een
//! doel noemt dat een item declareert". There is no ranked pool to fall back
//! to here: this module's only job is turning a raw prompt's text into the
//! `ServeInput` that `rank::select` decides everything else from, and
//! `select` already requires an actual binding match (and never matches
//! `Always` - see `rank::binding_matches`), so a prompt that resolves to
//! nothing simply selects nothing. Zero candidates in means zero items out;
//! there is no separate "nothing resolved" branch to get wrong.
//!
//! FLAGGED CHOICE (not measured, unlike the write gate's corpus-backed
//! rules): "the prompt names a moment" is decided by running the exact same
//! closed detectors a real command/file path already use
//! (`intent::from_command`, `intent::from_path`) over the prompt's raw text.
//! "The prompt names a target" is decided by checking, for every live item's
//! own declared `Target` value, whether that value's last path segment (the
//! part a sentence realistically names - see `normalize::last_segment`'s own
//! doc comment) appears as a whole token in the prompt. Both are testable and
//! tested below; neither has been measured against real prompts the way
//! `model::gate`'s ground 7 removal was measured against 2962 real items.

use crate::input::ServeInput;
use crate::live::LiveItem;
use model::item::Binding;
use model::normalize::{last_segment, normalize_target};

/// Split on anything that is not a path/word character, so a target's last
/// segment ("swap-binary.ps1", "main.rs") is found only as a WHOLE token in
/// the prompt - never as a slice of a longer word (a prompt about
/// "personas" must never be treated as naming a target named "rs"). Each
/// token is reduced to ITS OWN last segment before comparing (same
/// `normalize::last_segment` doctrine used everywhere else in this
/// workspace), so a prompt naming the target by its full path ("src/main.rs")
/// still resolves a binding declared by bare name alone ("main.rs").
fn contains_word(haystack_normalized: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack_normalized.split(is_break).any(|token| last_segment(token) == needle)
}

/// The one place this file decides where a token ends, so the prompt and the
/// needle can never be split by different rules - which is how a comparison
/// silently stops matching anything.
fn is_break(c: char) -> bool {
    !(c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '/')
}

/// Is this anchor named in the prompt, as one word or as a run of words?
///
/// THE DEFECT THIS CLOSES, measured on the owner's live store 2026-08-16: 305
/// fireable items - 21.8% of them - carry an anchor whose value contains a
/// space ("Aluminium 6061", "10T pinion", "git reset --hard"). `contains_word`
/// compares each token of the prompt against the WHOLE needle, and a single
/// token can never equal a two-word string, so every one of those was
/// unreachable here. Not stale, not crowded out: unreachable by construction,
/// and invisible to every count because their anchors resolve fine.
///
/// `rank::binding_matches` was taught about multi-word anchors on 2026-08-14.
/// This surface never was. This is that same repair, finished.
///
/// A single-word needle takes the old path unchanged, so nothing that resolved
/// before can resolve differently now - the only outcomes this adds are matches
/// where there were none.
fn contains_phrase(haystack_normalized: &str, needle: &str) -> bool {
    let needle_tokens: Vec<&str> = needle.split(is_break).filter(|t| !t.is_empty()).collect();
    match needle_tokens.len() {
        0 => false,
        1 => contains_word(haystack_normalized, needle_tokens[0]),
        n => {
            let hay: Vec<&str> =
                haystack_normalized.split(is_break).filter(|t| !t.is_empty()).collect();
            // Consecutive, in order: "aluminium 6061" is named by "voor aluminium
            // 6061 geldt", never by "aluminium is niet 6061". A bag of words here
            // would fire on any prompt that happens to use both, which is exactly
            // the widening this workspace refuses.
            hay.windows(n).any(|window| {
                window.iter().zip(&needle_tokens).all(|(t, w)| last_segment(t) == last_segment(w))
            })
        }
    }
}

/// Build the `ServeInput` a raw prompt resolves to. Moments: the union of
/// what `intent::from_command` and `intent::from_path` detect in the raw
/// text (a prompt that literally instructs a destructive command must
/// resolve the same way running it would). Targets: one entry per live
/// item's own `Target` binding whose last segment is named, verbatim, as a
/// token in the prompt - never a fuzzy/ranked closeness match, only "is this
/// exact thing named or not".
pub fn resolve(prompt: &str, candidates: &[LiveItem]) -> ServeInput {
    let mut input = ServeInput::default();
    if prompt.trim().is_empty() {
        return input;
    }

    for signal in intent::from_command(prompt) {
        if !input.moments.contains(&signal.action) {
            input.moments.push(signal.action);
        }
    }
    for signal in intent::from_path(prompt) {
        if !input.moments.contains(&signal.action) {
            input.moments.push(signal.action);
        }
    }

    let normalized_prompt = normalize_target(prompt);
    for c in candidates {
        for binding in &c.item.bindings {
            if let Binding::Target { kind, value } = binding {
                let seg = last_segment(&normalize_target(value)).to_string();
                if contains_phrase(&normalized_prompt, &seg) {
                    let entry = (*kind, value.clone());
                    if !input.targets.contains(&entry) {
                        input.targets.push(entry);
                    }
                }
            }
        }
    }

    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use intent::Action;
    use model::item::{Item, Kind, TargetKind};

    fn target_item(id: &str, kind: TargetKind, value: &str) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Orientation,
                text: format!("about {id}"),
                bindings: vec![Binding::Target { kind, value: value.to_string() }],
                severity: None,
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    /// THE DEFECT THIS CLOSES, measured 2026-08-16: 305 fireable items carry an
    /// anchor with a space in it, and this surface compared each token of the
    /// prompt against the whole value - so not one of them could ever resolve
    /// here, however plainly the prompt named it.
    #[test]
    fn a_multi_word_anchor_resolves_when_the_prompt_names_it() {
        let items = vec![target_item("legering", TargetKind::Command, "Aluminium 6061")];
        let input = resolve("welke boutmaat neem ik voor aluminium 6061", &items);
        assert_eq!(input.targets.len(), 1, "the prompt names it word for word");
    }

    /// Consecutive and in order, never a bag of words: a prompt that merely uses
    /// both words is not naming the thing. Widening this to "any word" would
    /// fire every anchor whose first word is common.
    #[test]
    fn the_same_words_apart_do_not_resolve_it() {
        let items = vec![target_item("legering", TargetKind::Command, "Aluminium 6061")];
        let input = resolve("aluminium is hier niet hetzelfde als 6061", &items);
        assert!(input.targets.is_empty(), "the words are present but the phrase is not");
    }

    /// The property that makes this safe to ship: a single-word anchor takes
    /// exactly the path it always took, so nothing that resolved before can
    /// resolve differently now.
    #[test]
    fn a_single_word_anchor_is_unchanged() {
        let items = vec![target_item("bestand", TargetKind::Path, "src/main.rs")];
        assert_eq!(resolve("kijk eens in main.rs", &items).targets.len(), 1);
        assert!(resolve("dit gaat over personas", &items).targets.is_empty());
    }

    #[test]
    fn an_empty_prompt_resolves_to_nothing() {
        let input = resolve("", &[]);
        assert!(input.is_empty());
    }

    #[test]
    fn a_whitespace_only_prompt_resolves_to_nothing() {
        let input = resolve("   \n\t  ", &[]);
        assert!(input.is_empty());
    }

    #[test]
    fn an_ordinary_question_resolves_to_nothing() {
        let input = resolve("What is the weather like today, in general?", &[]);
        assert!(input.is_empty());
    }

    #[test]
    fn a_prompt_that_names_a_destructive_command_resolves_its_moment() {
        let input = resolve("please run git push --force origin main for me", &[]);
        assert!(input.moments.contains(&Action::Push));
    }

    #[test]
    fn a_prompt_naming_a_declared_targets_last_segment_resolves_that_target() {
        let candidates = vec![target_item("t1", TargetKind::Path, r"C:\repo\swap-binary.ps1")];
        let input = resolve("can you check swap-binary.ps1 before we continue", &candidates);
        assert!(
            input.targets.contains(&(TargetKind::Path, r"C:\repo\swap-binary.ps1".to_string())),
            "expected the declared target to resolve, got {:?}",
            input.targets
        );
    }

    #[test]
    fn a_prompt_that_only_partially_contains_a_targets_name_does_not_match() {
        // "personas.rs" must never be treated as naming a target whose last
        // segment is "rs" alone - this proves the match is whole-token, not
        // substring.
        let candidates = vec![target_item("t2", TargetKind::Symbol, "rs")];
        let input = resolve("let's talk about user personas.rs handling", &candidates);
        assert!(input.targets.is_empty(), "a partial word match must never resolve a target: {:?}", input.targets);
    }

    #[test]
    fn a_prompt_naming_no_declared_target_resolves_no_targets() {
        let candidates = vec![target_item("t3", TargetKind::Path, "src/main.rs")];
        let input = resolve("let's discuss the roadmap for next quarter", &candidates);
        assert!(input.targets.is_empty());
    }

    #[test]
    fn the_same_target_is_never_added_twice() {
        let candidates =
            vec![target_item("t4", TargetKind::Path, "src/main.rs"), target_item("t5", TargetKind::Path, "src/main.rs")];
        let input = resolve("look at src/main.rs please", &candidates);
        assert_eq!(input.targets.len(), 1);
    }
}
