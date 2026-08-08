//! Path comparison, case handling and target-value normalisation - exactly
//! ONE function, used everywhere a target value must be compared against
//! something else (the item's own text, a bare-role-name check).
//!
//! On 2026-07-30 two places in 1.0 built the same key differently and two
//! checks disagreed about what "the same target" meant. `model/tests/
//! single_normalization.rs` greps the whole workspace for a second
//! definition of this function and fails if one appears.

/// Normalise a target value for comparison: trim surrounding whitespace,
/// unify path separators to `/`, and lowercase. Idempotent - normalising an
/// already-normalised value returns it unchanged.
pub fn normalize_target(value: &str) -> String {
    value.trim().replace('\\', "/").to_lowercase()
}

/// Words that NAME the absence of a project rather than naming a project. An
/// item carrying one of these belongs to the global tier, which this model
/// spells `None` - never a project that happens to be called "global".
const NOT_A_PROJECT: &[&str] = &["global", "none", "null", "-"];

/// Normalise an item's project: the global tier is `None`, and every spelling
/// that MEANS the global tier collapses onto it. Trims, and treats an empty
/// or whitespace-only value as absent.
///
/// THE DEFECT THIS CLOSES, caught before it shipped (2026-08-03). Target
/// identity had a normalisation from the start; project identity had none, so
/// whatever a source happened to write became the value. 1.0 spells the
/// global tier as the literal word `global` in its footer, and the migration
/// carried that word through as though it were a project name: 74 of the
/// owner's global rules ended up owned by a project called "global", and two
/// more by "null".
///
/// While no surface scoped by project this stayed invisible - those rules
/// fired everywhere, which happened to be right. The moment the moment
/// surface started scoping (`serve::project::applies_to`) they would have
/// fired NOWHERE, and they are the cross-project rules: the ones that matter
/// most and that belong to no single project by definition. A silent, total
/// loss of the global tier, produced by adding a correct filter on top of an
/// unnormalised value.
///
/// R6, not a patch: identity is data, normalised in exactly one place. The
/// write gate calls this, so the value cannot enter the store again.
pub fn normalize_project(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() || NOT_A_PROJECT.contains(&trimmed.to_lowercase().as_str()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// The last segment of an already-normalised target value: the part a sentence
/// realistically names. People write "swap-binary.ps1", never
/// "c:/users/x/thor-sync/swap-binary.ps1". Comparison lives in this module so
/// there is still exactly one place that decides what "the same target" means.
pub fn last_segment(normalized_value: &str) -> &str {
    normalized_value
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(normalized_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unifies_separators_and_case() {
        assert_eq!(normalize_target(r"Src\Main.rs"), "src/main.rs");
        assert_eq!(normalize_target("src/Main.RS"), "src/main.rs");
    }

    #[test]
    fn is_idempotent() {
        let once = normalize_target(r"  C:\Repo\Foo.rs  ");
        let twice = normalize_target(&once);
        assert_eq!(once, twice);
    }
}

/// Same target, decided in the ONE place normalisation lives
/// (`model::normalize`): equal once normalised, or equal by their last path
/// segment (an item bound to a full path still fires when the input names it
/// by its bare file/command name, and vice versa - see `normalize::last_segment`'s
/// own doc comment, which describes exactly this comparison).
pub fn target_matches(item_kind: crate::item::TargetKind, item_value: &str, in_kind: crate::item::TargetKind, in_value: &str) -> bool {
    if item_kind != in_kind {
        return false;
    }
    let a = normalize_target(item_value);
    let b = normalize_target(in_value);
    a == b || last_segment(&a) == last_segment(&b)
}
