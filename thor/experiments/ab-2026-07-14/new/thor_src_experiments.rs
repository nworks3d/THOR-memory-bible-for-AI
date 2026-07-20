//! A/B experiment algorithms (sandbox only). Each is a groundbreaker mined from
//! a rival's source, reimplemented dep-free in THOR's idiom and unit-tested on
//! synthetic edge cases so the MECHANISM is proven before any integration into
//! the write/consolidate path. These are measured by `cargo test experiments`,
//! not by the recall/drift harness (they touch dedup/merge, not ranking).

// ---------------------------------------------------------------------------
// 1. Entropy-gated name resolution (idea: getzep/graphiti dedup_helpers).
//    Shannon entropy of a NAME routes the near-dup decision: a distinctive name
//    (high entropy, long enough) may be fuzzy-matched; a generic/short one is
//    too risky and must escalate instead of auto-merging. This is the cheap,
//    LLM-free confidence signal that keeps "data"/"the api" from being fuzzily
//    merged while trusting "recall_fused_scoped".
// ---------------------------------------------------------------------------

/// Shannon entropy (bits/char) of a string over its character distribution.
pub fn shannon_bits(s: &str) -> f64 {
    use std::collections::HashMap;
    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0) += 1;
    }
    let n = s.chars().count() as f64;
    if n == 0.0 {
        return 0.0;
    }
    -counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            p * p.log2()
        })
        .sum::<f64>()
}

/// True when a name is distinctive enough to trust a fuzzy (MinHash/Jaccard)
/// match instead of escalating. Generic short words fail on length or entropy.
pub fn name_is_distinctive(name: &str) -> bool {
    let compact: String = name.chars().filter(|c| !c.is_whitespace()).collect();
    compact.chars().count() >= 8 && shannon_bits(&compact) >= 2.7
}

/// Character 3-gram Jaccard similarity - the cheap fuzzy match used only after
/// the entropy gate says a name is distinctive.
pub fn trigram_jaccard(a: &str, b: &str) -> f64 {
    use std::collections::HashSet;
    let grams = |s: &str| -> HashSet<String> {
        let cs: Vec<char> = s.to_lowercase().chars().collect();
        if cs.len() < 3 {
            return cs.iter().map(|c| c.to_string()).collect();
        }
        (0..cs.len() - 2).map(|i| cs[i..i + 3].iter().collect()).collect()
    };
    let (ga, gb) = (grams(a), grams(b));
    let inter = ga.intersection(&gb).count() as f64;
    let union = ga.union(&gb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// The full router: should these two names auto-merge as near-duplicates?
/// Escalate (return false) unless BOTH are distinctive AND their fuzzy overlap
/// clears the threshold. A generic name never auto-merges, however similar.
pub fn should_auto_merge_names(a: &str, b: &str, jaccard_min: f64) -> bool {
    name_is_distinctive(a) && name_is_distinctive(b) && trigram_jaccard(a, b) >= jaccard_min
}

// ---------------------------------------------------------------------------
// 2. Mutual-kNN cohesion gate (idea: redis/agent-memory-server
//    _semantic_merge_group_is_cohesive). Before merging a cluster, require every
//    member to be within `threshold` cosine of EVERY other member, so a "bridge"
//    member (near the anchor, far from the rest) blocks the merge - defeating
//    transitive over-merge (A~B, B~C, but A!~C fusing A and C via B).
// ---------------------------------------------------------------------------

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len().min(b.len()) {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// True only if the cluster is mutually cohesive: every pair within `threshold`.
/// A single bridge member fails the whole group, so the merge is refused and the
/// distinct topics stay separate (lossless: nothing is destroyed either way).
pub fn cluster_is_cohesive(vecs: &[Vec<f32>], threshold: f32) -> bool {
    for i in 0..vecs.len() {
        for j in (i + 1)..vecs.len() {
            if cosine(&vecs[i], &vecs[j]) < threshold {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entropy_name_router() {
        // Distinctive names (long, high entropy) -> trusted for fuzzy match.
        assert!(name_is_distinctive("recall_fused_scoped"));
        assert!(name_is_distinctive("The-mega-Smoother"));
        assert!(name_is_distinctive("MatrixCache::ensure"));
        // Generic / short -> must escalate, never auto-merge.
        assert!(!name_is_distinctive("data"));
        assert!(!name_is_distinctive("the api"));
        assert!(!name_is_distinctive("config"));
        assert!(!name_is_distinctive("aaaaaaaa")); // long but zero entropy

        // Router: two distinctive near-identical names auto-merge...
        assert!(should_auto_merge_names(
            "recall_fused_scoped",
            "recall_fused_scope",
            0.6
        ));
        // ...but two GENERIC names never do, even at high literal overlap.
        assert!(!should_auto_merge_names("the data", "the data!", 0.6));
        // ...and two distinctive but unrelated names do not.
        assert!(!should_auto_merge_names("recall_fused_scoped", "The-mega-Smoother", 0.6));
        eprintln!("ENTROPY-DEDUP: router classifies distinctive/generic + fuzzy correctly");
    }

    #[test]
    fn test_cohesion_gate_blocks_bridge_merge() {
        // 3D unit-ish vectors. A~B and B~C, but A and C are far apart (a bridge).
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.7, 0.7, 0.0]; // close to both A and C
        let c = vec![0.0, 1.0, 0.0]; // far from A
        // A cohesive pair (A,B) merges.
        assert!(cluster_is_cohesive(&[a.clone(), b.clone()], 0.6));
        // The bridge cluster (A,B,C) is NOT cohesive -> merge refused.
        assert!(!cluster_is_cohesive(&[a.clone(), b.clone(), c.clone()], 0.6));
        // A genuinely tight cluster passes.
        let d = vec![0.98, 0.20, 0.0];
        assert!(cluster_is_cohesive(&[a, d], 0.6));
        eprintln!("COHESION-GATE: bridge cluster (A~B~C, A!~C) correctly refused");
    }
}
