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

    /// THE DEFECT THIS PREVENTS, measured on the owner's real store: a fact
    /// describing an offline backup folder had fired 51 times against the
    /// local git checkout that happened to end in the same directory name.
    /// Comparing only the final segment collapsed every path to its filename.
    #[test]
    fn a_backup_folder_does_not_match_a_checkout_of_the_same_name() {
        use crate::item::TargetKind::Path;
        assert!(!target_matches(
            Path,
            r"\server\homes\owner\projects\Some-Repo",
            Path,
            "C:/Users/someone/dev/Some-Repo"
        ));
        // Two different files that merely share a filename: also no longer a
        // match, which is the same collapse one level down.
        assert!(!target_matches(Path, "a/orders.js", Path, "b/orders.js"));
        assert!(!target_matches(Path, "server/lib/orders.js", Path, "worker/lib/orders.js"));
    }

    /// THE LIMIT, pinned rather than hidden. A repo-relative anchor means
    /// "<root>/server/lib/orders.js", but this function never sees the root,
    /// so it cannot tell that apart from a nested copy at
    /// "<root>/worker/server/lib/orders.js" - the nested one genuinely ends
    /// with the anchor. Deciding it would mean threading the project root into
    /// every comparison, which is a real change and not a tidy-up.
    ///
    /// It is strictly better than what it replaced, which matched on the
    /// filename alone. If this ever starts returning false, someone taught the
    /// matcher about roots - update this test on purpose.
    #[test]
    fn a_nested_copy_of_the_same_relative_path_still_matches() {
        use crate::item::TargetKind::Path;
        assert!(target_matches(Path, "server/lib/orders.js", Path, "worker/server/lib/orders.js"));
    }

    /// What must keep working, and why the comparison cannot simply be
    /// equality: an item is anchored at a repo-relative path while the input
    /// carries the absolute one.
    #[test]
    fn a_relative_anchor_still_matches_the_absolute_path_it_names() {
        use crate::item::TargetKind::Path;
        assert!(target_matches(Path, "server/lib/orders.js", Path, "C:/Users/x/dev/repo/server/lib/orders.js"));
        assert!(target_matches(Path, r"server\lib\orders.js", Path, "C:/USERS/x/Server/Lib/Orders.js"));
        // Either direction, since which side is longer is not fixed.
        assert!(target_matches(Path, "C:/x/y/server/lib/orders.js", Path, "server/lib/orders.js"));
        // A single-segment anchor keeps its old reach; the write gate is what
        // refuses new items anchored on a bare role name.
        assert!(target_matches(Path, "orders.js", Path, "C:/x/server/lib/orders.js"));
    }

    /// The separator requirement, which is what stops "ends with this path"
    /// from quietly becoming "shares these last letters".
    #[test]
    fn a_shared_filename_ending_is_not_a_path_suffix() {
        use crate::item::TargetKind::Path;
        assert!(!target_matches(Path, "orders.js", Path, "C:/x/my-orders.js"));
        assert!(!target_matches(Path, "lib/orders.js", Path, "C:/x/sublib/orders.js"));
    }

    #[test]
    fn is_idempotent() {
        let once = normalize_target(r"  C:\Repo\Foo.rs  ");
        let twice = normalize_target(&once);
        assert_eq!(once, twice);
    }
}

/// Same target, decided in the ONE place normalisation lives
/// (`model::normalize`): equal once normalised, or one is a SUFFIX of the
/// other on a path-segment boundary.
///
/// WHY A SUFFIX AND NOT THE LAST SEGMENT ALONE. An item is normally anchored
/// at a repo-relative path while the input carries the absolute one, so the
/// comparison has to survive a prefix it has never seen. Comparing only the
/// final segment did that, and far more besides: it collapsed every path to
/// its filename, so `a/orders.js` matched `b/orders.js`, and an anchor on a
/// backup folder matched any checkout ending in the same directory name.
///
/// Measured on the owner's store, 2026-08-09: a fact describing an offline
/// backup folder had fired 51 times against the local git checkout of the same
/// name. The fact was true, the anchor looked reasonable, and the matcher put
/// it somewhere it had no business being. Nothing reported it - it took a
/// person reading one judgement request.
///
/// This is a NARROWING, which is the safe direction for a rule that decides
/// where a fact appears: everything it still matches, it matched before. A
/// single-segment anchor keeps matching any path ending in it; that looseness
/// is real, and the write gate already refuses new items anchored on a bare
/// role name.
pub fn target_matches(item_kind: crate::item::TargetKind, item_value: &str, in_kind: crate::item::TargetKind, in_value: &str) -> bool {
    if item_kind != in_kind {
        return false;
    }
    let a = normalize_target(item_value);
    let b = normalize_target(in_value);
    a == b || ends_on_segment_boundary(&a, &b) || ends_on_segment_boundary(&b, &a)
}

/// Does `longer` end with `shorter`, with a path separator immediately before
/// it? The separator requirement is what stops `.../my-orders.js` from reading
/// as though it ended with `orders.js`: a bare `ends_with` on the two strings
/// would silently turn "ends with this path" into "shares these last letters".
fn ends_on_segment_boundary(longer: &str, shorter: &str) -> bool {
    if shorter.is_empty() || longer.len() <= shorter.len() {
        return false;
    }
    longer.ends_with(shorter) && longer.as_bytes()[longer.len() - shorter.len() - 1] == b'/'
}
