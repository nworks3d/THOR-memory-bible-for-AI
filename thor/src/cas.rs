use std::collections::{HashMap, HashSet};
use crate::event_store::{Event, EventKind};

#[derive(Debug, Clone)]
pub struct HeadSet {
    pub heads: HashSet<String>,
    pub is_diverged: bool,
}

/// Parse the JSON body of a fact_resolved event into (keep_rev, discarded).
/// Returns None when the body is not a well-formed resolve record.
pub fn parse_resolve_body(body: &str) -> Option<(String, Vec<String>)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let keep_rev = value.get("keep_rev")?.as_str()?.to_string();
    let discarded = value
        .get("discarded")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    Some((keep_rev, discarded))
}

/// Parse a `fact_reprojected` body into its project override:
/// `Some(Some(key))` = scope to that project, `Some(None)` = make global,
/// `None` = malformed (the fold treats it as a no-op, same as an invalid resolve).
pub fn parse_reproject_body(body: &str) -> Option<Option<String>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    match value.get("project") {
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(Some(s.clone())),
        _ => None,
    }
}

/// The effective PROJECT of every entity, folded from the log (events in seq order).
/// `None` = global. Rule: the last valid `fact_reprojected` wins; otherwise the birth
/// project = the entity_id prefix before the first `:` (a repo chunk or a
/// project-minted memory); otherwise `None` (an unprefixed id is born global). This is
/// a pure function of already-hashed fields (kind, entity_id, body), so it is
/// deterministic, cacheable and fsck-assertable, and folds in the SAME O(n) pass recall
/// already makes over the events.
pub fn compute_projects(events: &[Event]) -> HashMap<String, Option<String>> {
    let mut projects: HashMap<String, Option<String>> = HashMap::new();
    for event in events {
        match event.kind {
            EventKind::FactReprojected => {
                // A reproject OVERRIDES (last wins). Malformed body = no-op.
                if let Some(override_project) = parse_reproject_body(&event.body) {
                    projects.insert(event.entity_id.clone(), override_project);
                }
            }
            _ => {
                // Birth project: set on first sight only, so a later revise cannot
                // clobber an earlier reproject. Prefix before the first ':' (owned).
                projects
                    .entry(event.entity_id.clone())
                    .or_insert_with(|| crate::repo::owner_project(&event.entity_id).map(|p| p.to_string()));
            }
        }
    }
    projects
}

/// The ONE authoritative CAS-fold over the raw log (events in seq order):
///
/// - create: the new rev joins the entity head-set.
/// - mutate (revise/supersede/retract): if parent_rev is a current head it is
///   REPLACED by the new rev (fast-forward); in every other case the new rev
///   is ADDED (branch). There is no rejection path that drops a write.
/// - resolve: a multi-head CAS, processed inline in seq order. It applies only
///   when {keep_rev} union discarded[] equals the current head-set and
///   keep_rev is itself a current head; then exactly the revs named in
///   discarded[] are removed. An invalid resolve in the log is a no-op on
///   heads: it can never remove a head it did not cite.
///
/// A rev leaves the head-set ONLY via a fast-forward that cited it exactly or
/// via a valid resolve that named it in discarded[]. No other removal path
/// exists.
pub fn compute_head_sets(events: &[Event]) -> HashMap<String, HeadSet> {
    let mut heads: HashMap<String, HashSet<String>> = HashMap::new();

    for event in events {
        match event.kind {
            EventKind::FactCreated => {
                heads
                    .entry(event.entity_id.clone())
                    .or_default()
                    .insert(event.this_hash.clone());
            }
            EventKind::FactRevised | EventKind::FactSuperseded | EventKind::FactRetracted => {
                let entity_heads = heads.entry(event.entity_id.clone()).or_default();
                match &event.parent_rev {
                    Some(parent_rev) if entity_heads.contains(parent_rev) => {
                        entity_heads.remove(parent_rev);
                        entity_heads.insert(event.this_hash.clone());
                    }
                    _ => {
                        entity_heads.insert(event.this_hash.clone());
                    }
                }
            }
            EventKind::FactResolved => {
                let entity_heads = heads.entry(event.entity_id.clone()).or_default();
                if let Some((keep_rev, discarded)) = parse_resolve_body(&event.body) {
                    let keep_also_discarded = discarded.iter().any(|d| *d == keep_rev);
                    let mut cited: HashSet<String> = discarded.iter().cloned().collect();
                    cited.insert(keep_rev.clone());
                    let valid = !keep_also_discarded
                        && entity_heads.contains(&keep_rev)
                        && cited == *entity_heads;
                    if valid {
                        for rev in &discarded {
                            entity_heads.remove(rev);
                        }
                    }
                }
            }
            // Reassert/echo/reproject never change the head-set. FactReprojected
            // is read only by the project fold (compute_projects).
            EventKind::FactReasserted | EventKind::FactEchoed | EventKind::FactReprojected => {}
        }
    }

    heads
        .into_iter()
        .map(|(entity_id, set)| {
            let is_diverged = set.len() > 1;
            (
                entity_id,
                HeadSet {
                    heads: set,
                    is_diverged,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(seq: i64, kind: EventKind, parent: Option<&str>, this_hash: &str, body: &str) -> Event {
        Event {
            seq,
            event_uuid: format!("uuid-{}", seq),
            session_id: "s".to_string(),
            lineage_id: "l".to_string(),
            actor: "a".to_string(),
            kind,
            entity_id: "e1".to_string(),
            parent_rev: parent.map(|s| s.to_string()),
            body: body.to_string(),
            body_ch: body.to_string(),
            prev_hash: String::new(),
            this_hash: this_hash.to_string(),
        }
    }

    fn resolve_body(keep: &str, discarded: &[&str]) -> String {
        serde_json::json!({ "keep_rev": keep, "discarded": discarded }).to_string()
    }

    fn heads_of(events: &[Event]) -> HashSet<String> {
        compute_head_sets(events)["e1"].heads.clone()
    }

    fn set(revs: &[&str]) -> HashSet<String> {
        revs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_fold_ff_replaces_cited_head() {
        let events = vec![
            mk(1, EventKind::FactCreated, None, "A", "a"),
            mk(2, EventKind::FactRevised, Some("A"), "B", "b"),
        ];
        let result = &compute_head_sets(&events)["e1"];
        assert_eq!(result.heads, set(&["B"]));
        assert!(!result.is_diverged);
    }

    #[test]
    fn test_fold_miss_branches() {
        let events = vec![
            mk(1, EventKind::FactCreated, None, "A", "a"),
            mk(2, EventKind::FactRevised, Some("stale"), "B", "b"),
        ];
        let result = &compute_head_sets(&events)["e1"];
        assert_eq!(result.heads, set(&["A", "B"]));
        assert!(result.is_diverged);
    }

    #[test]
    fn test_fold_resolve_removes_only_discarded() {
        let events = vec![
            mk(1, EventKind::FactCreated, None, "A", "a"),
            mk(2, EventKind::FactRevised, Some("stale"), "B", "b"),
            mk(3, EventKind::FactResolved, None, "R1", &resolve_body("A", &["B"])),
        ];
        assert_eq!(heads_of(&events), set(&["A"]));
    }

    #[test]
    fn test_fold_partial_resolve_is_noop() {
        // heads {A,B,C}; the resolve cites only {A,B} -> invalid -> no-op,
        // so the unlisted head C (and B) survive
        let events = vec![
            mk(1, EventKind::FactCreated, None, "A", "a"),
            mk(2, EventKind::FactRevised, Some("stale1"), "B", "b"),
            mk(3, EventKind::FactRevised, Some("stale2"), "C", "c"),
            mk(4, EventKind::FactResolved, None, "R1", &resolve_body("A", &["B"])),
        ];
        assert_eq!(heads_of(&events), set(&["A", "B", "C"]));
    }

    #[test]
    fn test_fold_resolve_keep_must_be_head() {
        let events = vec![
            mk(1, EventKind::FactCreated, None, "A", "a"),
            mk(2, EventKind::FactResolved, None, "R1", &resolve_body("X", &["A"])),
        ];
        assert_eq!(heads_of(&events), set(&["A"]));
    }

    #[test]
    fn test_fold_resolve_keep_in_discarded_is_noop() {
        let events = vec![
            mk(1, EventKind::FactCreated, None, "A", "a"),
            mk(2, EventKind::FactRevised, Some("stale"), "B", "b"),
            mk(3, EventKind::FactResolved, None, "R1", &resolve_body("A", &["A", "B"])),
        ];
        assert_eq!(heads_of(&events), set(&["A", "B"]));
    }

    #[test]
    fn test_fold_resolve_then_revise_fast_forwards() {
        // resolves are processed in seq order, so a later revise can FF from
        // the resolved winner
        let events = vec![
            mk(1, EventKind::FactCreated, None, "A", "a"),
            mk(2, EventKind::FactRevised, Some("stale"), "B", "b"),
            mk(3, EventKind::FactResolved, None, "R1", &resolve_body("A", &["B"])),
            mk(4, EventKind::FactRevised, Some("A"), "D", "d"),
        ];
        let result = &compute_head_sets(&events)["e1"];
        assert_eq!(result.heads, set(&["D"]));
        assert!(!result.is_diverged);
    }

    // per-entity event builder (mk hardcodes "e1")
    fn mk_e(seq: i64, entity: &str, kind: EventKind, this_hash: &str, body: &str) -> Event {
        let mut e = mk(seq, kind, None, this_hash, body);
        e.entity_id = entity.to_string();
        e
    }

    #[test]
    fn test_compute_projects_fold() {
        let reproj = |p: Option<&str>| match p {
            Some(k) => serde_json::json!({ "project": k }).to_string(),
            None => serde_json::json!({ "project": null }).to_string(),
        };
        let events = vec![
            // birth from prefix
            mk_e(1, "Repo1:src/a.rs#0", EventKind::FactCreated, "h1", "code"),
            // unprefixed = born global
            mk_e(2, "01KGLOBALMEM", EventKind::FactCreated, "h2", "a preference"),
            // global file under the reserved key
            mk_e(3, "@global:dev-loop.md#0", EventKind::FactCreated, "h3", "the dev loop"),
            // created in ProjA, reprojected to ProjB (last wins), then revised (must NOT clobber)
            mk_e(4, "ProjA:mem-x", EventKind::FactCreated, "h4", "a decision"),
            mk_e(5, "ProjA:mem-x", EventKind::FactReprojected, "h5", &reproj(Some("ProjB"))),
            mk_e(6, "ProjA:mem-x", EventKind::FactRevised, "h6", "a decision v2"),
            // reprojected to global (null)
            mk_e(7, "ProjC:mem-y", EventKind::FactCreated, "h7", "temp"),
            mk_e(8, "ProjC:mem-y", EventKind::FactReprojected, "h8", &reproj(None)),
            // malformed reproject = no-op (keeps birth ProjD)
            mk_e(9, "ProjD:mem-z", EventKind::FactCreated, "h9", "keep"),
            mk_e(10, "ProjD:mem-z", EventKind::FactReprojected, "h10", "not json"),
        ];
        let p = compute_projects(&events);
        assert_eq!(p["Repo1:src/a.rs#0"], Some("Repo1".to_string()));
        assert_eq!(p["01KGLOBALMEM"], None, "unprefixed = global");
        assert_eq!(p["@global:dev-loop.md#0"], Some("@global".to_string()));
        assert_eq!(p["ProjA:mem-x"], Some("ProjB".to_string()), "reproject wins, revise does not clobber");
        assert_eq!(p["ProjC:mem-y"], None, "reproject to null = global");
        assert_eq!(p["ProjD:mem-z"], Some("ProjD".to_string()), "malformed reproject = no-op -> birth");
    }
}
