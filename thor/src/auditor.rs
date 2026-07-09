use crate::event_store::{canonical_repr, canonicalize_body, hash_event, Event, EventKind};
use crate::cas::{compute_head_sets, HeadSet};
use std::collections::{HashMap, HashSet};

pub struct DifferentialAuditor;

fn is_mutation(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::FactRevised | EventKind::FactSuperseded | EventKind::FactRetracted
    )
}

/// Auditor-local parse of a fact_resolved body. Deliberately written
/// independently of cas::parse_resolve_body: the auditor shares NO helper
/// code with the projector it audits.
fn parse_resolve(body: &str) -> Option<(String, Vec<String>)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let keep_rev = value["keep_rev"].as_str()?.to_string();
    let items = match &value["discarded"] {
        serde_json::Value::Array(items) => items,
        _ => return None,
    };
    let mut discarded = Vec::new();
    for item in items {
        if let Some(rev) = item.as_str() {
            discarded.push(rev.to_string());
        }
    }
    Some((keep_rev, discarded))
}

/// The head-set of one entity as it stood just BEFORE the event at `seq`:
/// every rev introduced earlier that no removal (citation or valid resolve)
/// earlier than `seq` has taken out. Pass i64::MAX for the final head-set.
fn heads_before(
    seq: i64,
    introduced_at: &HashMap<String, i64>,
    removed_at: &HashMap<String, i64>,
) -> HashSet<String> {
    introduced_at
        .iter()
        .filter(|(_, &born)| born < seq)
        .filter(|(rev, _)| removed_at.get(rev.as_str()).map_or(true, |&gone| gone >= seq))
        .map(|(rev, _)| rev.clone())
        .collect()
}

/// Derive the head-set of one entity from the spec via a citation graph, NOT
/// a stateful forward fold. Input: this entity's events in ascending seq
/// order.
///
/// Spec, restated as removal events on a graph:
/// 1. every create/mutate event INTRODUCES its this_hash as a rev;
/// 2. a rev is REMOVED at the seq of the first later same-entity mutation
///    that cites it as parent_rev (a citation of a non-introduced or
///    not-yet-introduced rev removes nothing: that write branched);
/// 3. a rev is REMOVED at the seq of a valid resolve naming it in
///    discarded[]; a resolve is valid iff keep_rev is a head at that moment,
///    keep_rev is not itself discarded, and {keep_rev} union discarded[]
///    equals the head-set at that moment;
/// 4. heads = introduced revs never removed.
fn spec_heads_for_entity(events: &[&Event]) -> HashSet<String> {
    let mut introduced_at: HashMap<String, i64> = HashMap::new();
    for event in events {
        if event.kind == EventKind::FactCreated || is_mutation(event.kind) {
            introduced_at
                .entry(event.this_hash.clone())
                .or_insert(event.seq);
        }
    }

    let mut removed_at: HashMap<String, i64> = HashMap::new();
    for event in events {
        if !is_mutation(event.kind) {
            continue;
        }
        let parent = match event.parent_rev.as_deref() {
            Some(parent) => parent,
            None => continue,
        };
        match introduced_at.get(parent) {
            Some(&born) if event.seq > born => {
                removed_at.entry(parent.to_string()).or_insert(event.seq);
            }
            _ => {}
        }
    }

    for event in events {
        if event.kind != EventKind::FactResolved {
            continue;
        }
        let (keep_rev, discarded) = match parse_resolve(&event.body) {
            Some(parsed) => parsed,
            None => continue,
        };
        if discarded.iter().any(|rev| *rev == keep_rev) {
            continue;
        }
        let current = heads_before(event.seq, &introduced_at, &removed_at);
        if !current.contains(&keep_rev) {
            continue;
        }
        let mut cited: Vec<String> = discarded.clone();
        cited.push(keep_rev.clone());
        cited.sort();
        cited.dedup();
        let mut current_sorted: Vec<String> = current.into_iter().collect();
        current_sorted.sort();
        if cited != current_sorted {
            continue;
        }
        for rev in discarded {
            removed_at.entry(rev).or_insert(event.seq);
        }
    }

    heads_before(i64::MAX, &introduced_at, &removed_at)
}

impl DifferentialAuditor {
    /// Second, independently implemented derivation of the head-sets from the
    /// raw log, used as a differential oracle against the projector
    /// (cas::compute_head_sets). Events are grouped per entity first and each
    /// entity goes through a citation-graph derivation; see
    /// spec_heads_for_entity.
    pub fn compute_heads_from_spec(events: &[Event]) -> HashMap<String, HeadSet> {
        let mut per_entity: HashMap<String, Vec<&Event>> = HashMap::new();
        for event in events {
            match event.kind {
                // Head-neutral kinds are excluded for the same reason the
                // canonical fold never stores an entry for them: an entity whose
                // only events are head-neutral (e.g. a stray reproject against an
                // id that never existed) must yield NO entry on either side. The
                // spec used to keep an EMPTY entry for such an entity while the
                // canonical fold kept none - a representation mismatch reported
                // as a false head error (hit live on 2026-07-10).
                EventKind::FactReasserted | EventKind::FactEchoed | EventKind::FactReprojected => {}
                _ => per_entity
                    .entry(event.entity_id.clone())
                    .or_default()
                    .push(event),
            }
        }

        let mut result = HashMap::new();
        for (entity_id, entity_events) in per_entity {
            let heads = spec_heads_for_entity(&entity_events);
            let is_diverged = heads.len() > 1;
            result.insert(entity_id, HeadSet { heads, is_diverged });
        }
        result
    }

    /// Compare an externally computed projector result against the spec fold.
    /// Exposed separately so tests can feed a deliberately buggy projector
    /// variant and prove the two derivations genuinely disagree.
    pub fn verify_heads_match(
        projector_heads: &HashMap<String, HeadSet>,
        events: &[Event],
    ) -> Result<(), String> {
        let spec_heads = Self::compute_heads_from_spec(events);

        for entity_id in projector_heads.keys().chain(spec_heads.keys()) {
            let canonical = projector_heads.get(entity_id);
            let spec = spec_heads.get(entity_id);

            match (canonical, spec) {
                (Some(c), Some(s)) => {
                    if c.heads != s.heads || c.is_diverged != s.is_diverged {
                        return Err(format!(
                            "Head-set mismatch for entity {}: canonical {:?} != spec {:?}",
                            entity_id, c, s
                        ));
                    }
                }
                (Some(c), None) => {
                    return Err(format!(
                        "Canonical has heads for {} but spec does not: {:?}",
                        entity_id, c
                    ))
                }
                (None, Some(s)) => {
                    return Err(format!(
                        "Spec has heads for {} but canonical does not: {:?}",
                        entity_id, s
                    ))
                }
                (None, None) => {}
            }
        }

        Ok(())
    }

    pub fn verify_consistency(events: &[Event]) -> Result<(), String> {
        Self::verify_heads_match(&compute_head_sets(events), events)
    }
}

pub fn verify_chain_integrity(events: &[Event]) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }

    let mut expected_prev = String::new();
    let mut seen_uuids = HashSet::new();
    let mut prev_seq = 0i64;

    for event in events {
        if event.seq != prev_seq + 1 {
            return Err(format!(
                "Non-contiguous seq: expected {}, got {}",
                prev_seq + 1,
                event.seq
            ));
        }

        if event.prev_hash != expected_prev {
            return Err(format!(
                "Hash chain broken at seq {}: expected prev_hash {}, got {}",
                event.seq, expected_prev, event.prev_hash
            ));
        }

        // Tamper evidence: recompute this_hash from the stored fields through
        // the SAME canonical_repr the writer used. Any bitflip in kind,
        // entity_id, parent_rev or body_ch changes the recomputed hash. seq is
        // NOT hashed (see canonical_repr); its integrity comes from the
        // contiguity check above plus the prev_hash chain.
        let recomputed = hash_event(&event.prev_hash, &canonical_repr(event));
        if recomputed != event.this_hash {
            return Err(format!(
                "Hash mismatch at seq {}: stored this_hash {} does not match recomputed {} (event fields were tampered)",
                event.seq, event.this_hash, recomputed
            ));
        }

        // body itself is covered via its canonical form
        if event.body_ch != canonicalize_body(&event.body) {
            return Err(format!(
                "body_ch mismatch at seq {}: stored canonical body does not match canonicalize(body)",
                event.seq
            ));
        }

        if seen_uuids.contains(&event.event_uuid) {
            return Err(format!("Duplicate event_uuid: {}", event.event_uuid));
        }
        seen_uuids.insert(event.event_uuid.clone());

        expected_prev = event.this_hash.clone();
        prev_seq = event.seq;
    }

    Ok(())
}

pub fn detect_fork(events: &[Event]) -> Result<(), String> {
    let mut prev_hash_to_events: HashMap<String, Vec<&Event>> = HashMap::new();

    for event in events {
        prev_hash_to_events
            .entry(event.prev_hash.clone())
            .or_insert_with(Vec::new)
            .push(event);
    }

    for (prev_hash, events_with_prev) in prev_hash_to_events {
        if events_with_prev.len() > 1 {
            let this_hashes: Vec<&str> = events_with_prev
                .iter()
                .map(|e| e.this_hash.as_str())
                .collect();
            return Err(format!(
                "Fork detected: multiple events with prev_hash {} produce hashes: {:?}",
                prev_hash, this_hashes
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventStore;

    #[test]
    fn test_verify_consistency_empty() {
        let events = vec![];
        let result = DifferentialAuditor::verify_consistency(&events);
        assert!(result.is_ok());
    }

    #[test]
    fn test_reprojected_only_entity_is_consistent() {
        // A stray reproject against an id with no other events (CLI misuse, a
        // CRLF-mangled id) is head-neutral: BOTH derivations must omit the
        // entity. The spec used to keep an empty entry while the canonical fold
        // kept none, and fsck reported a false head error on a healthy log.
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "real", None, "a fact")
            .unwrap();
        store
            .append_event(
                "s", "l", "a", EventKind::FactReprojected, "phantom\r", None,
                r#"{"project":"ProjA"}"#,
            )
            .unwrap();
        let events = store.get_all_events().unwrap();
        assert!(
            DifferentialAuditor::verify_consistency(&events).is_ok(),
            "a reprojected-only entity must not trip the differential auditor"
        );
    }

    #[test]
    fn test_verify_chain_integrity_empty() {
        let events = vec![];
        let result = verify_chain_integrity(&events);
        assert!(result.is_ok());
    }

    #[test]
    fn test_fsck_recomputes_hashes_on_tampered_fields() {
        let mut store = EventStore::in_memory().unwrap();
        let ev1 = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body one")
            .unwrap();
        store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(&ev1.this_hash), "body two",
            )
            .unwrap();

        let clean = store.get_all_events().unwrap();
        assert!(
            verify_chain_integrity(&clean).is_ok(),
            "untampered log must pass"
        );

        // every tamper below leaves this_hash/prev_hash untouched;
        // fsck must still catch it by recomputing the hash

        let mut body_flip = clean.clone();
        body_flip[1].body = "tampered body".to_string();
        body_flip[1].body_ch = canonicalize_body("tampered body");
        assert!(
            verify_chain_integrity(&body_flip).is_err(),
            "body flip (with consistent body_ch) must be detected"
        );

        let mut body_only = clean.clone();
        body_only[1].body = "tampered body".to_string();
        assert!(
            verify_chain_integrity(&body_only).is_err(),
            "body flip (stale body_ch) must be detected"
        );

        let mut entity_flip = clean.clone();
        entity_flip[1].entity_id = "other-entity".to_string();
        assert!(
            verify_chain_integrity(&entity_flip).is_err(),
            "entity_id flip must be detected"
        );

        let mut kind_flip = clean.clone();
        kind_flip[1].kind = EventKind::FactRetracted;
        assert!(
            verify_chain_integrity(&kind_flip).is_err(),
            "kind flip must be detected"
        );

        let mut parent_flip = clean.clone();
        parent_flip[1].parent_rev = Some("forged-parent".to_string());
        assert!(
            verify_chain_integrity(&parent_flip).is_err(),
            "parent_rev flip must be detected"
        );

        let mut body_ch_flip = clean.clone();
        body_ch_flip[1].body_ch = "forged canonical".to_string();
        assert!(
            verify_chain_integrity(&body_ch_flip).is_err(),
            "body_ch flip must be detected"
        );
    }

    #[test]
    fn test_differential_auditor_catches_buggy_projector() {
        let mut store = EventStore::in_memory().unwrap();
        let rev_a = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body A")
            .unwrap()
            .this_hash;
        let rev_b = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-parent"), "body B",
            )
            .unwrap()
            .this_hash;
        let events = store.get_all_events().unwrap();

        // the honest projector agrees with the spec fold: heads {A, B}
        assert!(DifferentialAuditor::verify_consistency(&events).is_ok());

        // buggy variant 1: fast-forward-on-miss (the stale revise silently
        // replaced the head instead of branching)
        let mut ff_on_miss = HashMap::new();
        ff_on_miss.insert(
            "e1".to_string(),
            HeadSet {
                heads: HashSet::from([rev_b.clone()]),
                is_diverged: false,
            },
        );
        assert!(
            DifferentialAuditor::verify_heads_match(&ff_on_miss, &events).is_err(),
            "spec fold must flag a fast-forward-on-miss projector"
        );

        // buggy variant 2: a projector that silently drops a head
        let mut dropped = compute_head_sets(&events);
        let head_set = dropped.get_mut("e1").unwrap();
        head_set.heads.remove(&rev_a);
        head_set.is_diverged = head_set.heads.len() > 1;
        assert!(
            DifferentialAuditor::verify_heads_match(&dropped, &events).is_err(),
            "spec fold must flag a dropped head"
        );
    }

    #[test]
    fn test_differential_auditor_models_resolve() {
        let mut store = EventStore::in_memory().unwrap();
        let rev_a = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body A")
            .unwrap()
            .this_hash;
        let rev_b = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-parent"), "body B",
            )
            .unwrap()
            .this_hash;

        // a valid resolve: both folds must land on {A}
        store
            .append_resolve("s1", "l1", "cli", "e1", &rev_a, &[rev_b.clone()])
            .unwrap();
        let events = store.get_all_events().unwrap();
        assert!(DifferentialAuditor::verify_consistency(&events).is_ok());
        let spec = DifferentialAuditor::compute_heads_from_spec(&events);
        assert_eq!(spec["e1"].heads, HashSet::from([rev_a.clone()]));

        // a new branch, then an INVALID partial resolve injected raw (bypassing
        // the CAS guard): both folds must treat it as a no-op
        let rev_c = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-2"), "body C",
            )
            .unwrap()
            .this_hash;
        let partial = serde_json::json!({ "keep_rev": rev_a, "discarded": [] }).to_string();
        store
            .append_event("s1", "l1", "a1", EventKind::FactResolved, "e1", None, &partial)
            .unwrap();

        let events = store.get_all_events().unwrap();
        assert!(DifferentialAuditor::verify_consistency(&events).is_ok());
        let spec = DifferentialAuditor::compute_heads_from_spec(&events);
        assert!(
            spec["e1"].heads.contains(&rev_c),
            "the head the invalid resolve never cited must survive in the spec fold"
        );
        assert_eq!(spec["e1"].heads.len(), 2);
    }
}
