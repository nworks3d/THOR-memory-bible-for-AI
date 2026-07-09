use crate::event_store::{Event, EventKind, EventStore};
use std::io::BufRead;
use std::path::Path;

#[derive(Debug, Default, Clone, Copy)]
pub struct ImportStats {
    pub imported: usize,
    pub revised: usize,
    pub retracted: usize,
    pub skipped_existing: usize,
    pub skipped_malformed: usize,
    pub skipped_diverged: usize,
    /// New entities refused because a live fact with the same normalized body
    /// prefix already exists under ANOTHER id - the dual-write round-trip twin.
    pub skipped_duplicate: usize,
}

/// The single current head rev of an entity, via the ONE authoritative CAS fold
/// (`cas::compute_head_sets`, the same rule `append_mutate_checked` uses). A
/// parent-pointer heuristic is NOT equivalent: head-neutral events (fact_echoed
/// from `mark`, fact_reprojected, fact_resolved bookkeeping) are appended with
/// no parent, so counting "hashes nobody cites" as heads would flag a marked or
/// reprojected entity as diverged and freeze source corrections forever.
/// `None` when the entity is genuinely diverged (more than one contested head) -
/// the importer then leaves it untouched rather than guess which head a
/// correction supersedes.
fn single_head(events: &[Event]) -> Option<String> {
    let entity_id = events.first().map(|e| e.entity_id.as_str())?;
    let head_sets = crate::cas::compute_head_sets(events);
    let head_set = head_sets.get(entity_id)?;
    if head_set.heads.len() == 1 {
        head_set.heads.iter().next().cloned()
    } else {
        None
    }
}

/// Import facts from a JSONL snapshot (one object per line). THOR never opens
/// the source memory store (e.g. the live mimir DB): an external, read-only
/// exporter produces the JSONL, so the import is fully decoupled and cannot
/// touch the source.
///
/// Each line: `{"entity_id": "...", "body": "...", "actor": "..."?, "status": "..."?}`.
/// A new entity becomes a `fact_created`. An entity that ALREADY exists is
/// reconciled so a correction in the source propagates instead of being frozen:
/// - `status` = superseded/deleted/retracted -> `fact_retracted` (stop serving it);
/// - the body CHANGED -> `fact_revised` (the old version stops being a head);
/// - the body is identical -> skipped (idempotent: re-running imports nothing);
/// - a diverged entity is left untouched (no safe single head to supersede).
pub fn import_jsonl(store: &mut EventStore, path: &Path) -> anyhow::Result<ImportStats> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut stats = ImportStats::default();

    // Near-duplicate refusal for NEW entities, same normalization as the MCP
    // remember tool: without it, every dual-written fact (stored directly in
    // THOR and in the source) comes back as a TWIN under the source's id on the
    // next import - measured live: 19 twins accumulated in two days. The map is
    // built once per run; entities created by THIS run are added as we go.
    let mut live_prefixes: std::collections::HashMap<String, String> = {
        let events = store.get_all_events()?;
        let heads = crate::cas::compute_head_sets(&events);
        let by_hash: std::collections::HashMap<&str, &Event> =
            events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
        heads
            .iter()
            .flat_map(|(id, hs)| {
                hs.heads.iter().filter_map(|rev| {
                    by_hash.get(rev.as_str()).and_then(|ev| {
                        (!matches!(ev.kind, EventKind::FactRetracted))
                            .then(|| (crate::recall::dedup_prefix(&ev.body), id.clone()))
                    })
                })
            })
            .collect()
    };

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                stats.skipped_malformed += 1;
                continue;
            }
        };
        let entity_id = match value.get("entity_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => {
                stats.skipped_malformed += 1;
                continue;
            }
        };
        // Require a non-empty STRING body: a number/null/bool/absent body would
        // otherwise be coerced to "" and imported as a useless empty-body head
        // that recall can never match and idempotency then locks in forever.
        // Treat it like a missing entity_id: skip and count it malformed.
        let body = match value.get("body").and_then(|v| v.as_str()) {
            Some(b) if !b.trim().is_empty() => b,
            _ => {
                stats.skipped_malformed += 1;
                continue;
            }
        };
        let actor = value
            .get("actor")
            .and_then(|v| v.as_str())
            .unwrap_or("mimir-import");

        // Reconcile an entity that already exists so a SOURCE correction propagates
        // (the old importer froze the first-seen body forever). Diverged entities are
        // left untouched; identical bodies are skipped (idempotent).
        let existing = store.get_events_by_entity(&entity_id)?;
        if !existing.is_empty() {
            let head = match single_head(&existing) {
                Some(h) => h,
                None => {
                    stats.skipped_diverged += 1;
                    continue;
                }
            };
            let head_ev = existing.iter().find(|e| e.this_hash == head);
            let already_retracted =
                head_ev.map(|e| matches!(e.kind, EventKind::FactRetracted)).unwrap_or(false);
            let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
            if matches!(status.as_str(), "superseded" | "deleted" | "retracted") {
                if already_retracted {
                    stats.skipped_existing += 1;
                    continue;
                }
                store.append_event(
                    "import", "mimir-import", actor, EventKind::FactRetracted, &entity_id, Some(&head),
                    "[retracted: superseded in source]",
                )?;
                stats.retracted += 1;
                continue;
            }
            // Live head with an identical body -> nothing to do (idempotent re-import).
            if !already_retracted && head_ev.map(|e| e.body == body).unwrap_or(false) {
                stats.skipped_existing += 1;
                continue;
            }
            // Body changed (or resurrecting a retracted head from the source) -> revise.
            store.append_event(
                "import", "mimir-import", actor, EventKind::FactRevised, &entity_id, Some(&head), body,
            )?;
            stats.revised += 1;
            continue;
        }

        // A NEW id whose body prefix matches an existing live fact is the
        // dual-write round-trip twin - skip it instead of minting a duplicate.
        let prefix = crate::recall::dedup_prefix(body);
        if let Some(existing_id) = live_prefixes.get(&prefix) {
            if existing_id != &entity_id {
                stats.skipped_duplicate += 1;
                continue;
            }
        }

        store.append_event(
            "import",
            "mimir-import",
            actor,
            EventKind::FactCreated,
            &entity_id,
            None,
            body,
        )?;
        live_prefixes.insert(prefix, entity_id.clone());
        stats.imported += 1;
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall::recall;
    use std::io::Write;

    fn write_jsonl(dir: &Path, name: &str, lines: &[&str]) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", line).unwrap();
        }
        path
    }

    #[test]
    fn test_import_rejects_nonstring_or_empty_body() {
        // audit finding: a non-string/absent/blank body must be rejected as
        // malformed, not silently imported as an empty-body head.
        let dir = tempfile::tempdir().unwrap();
        let jsonl = write_jsonl(
            dir.path(),
            "b.jsonl",
            &[
                r#"{"entity_id":"NUM","body":42}"#,
                r#"{"entity_id":"NUL","body":null}"#,
                r#"{"entity_id":"MISS"}"#,
                r#"{"entity_id":"EMPTY","body":"   "}"#,
                r#"{"entity_id":"OK","body":"real content here"}"#,
            ],
        );
        let db = dir.path().join("b.db");
        let mut store = EventStore::new(&db).unwrap();
        let stats = import_jsonl(&mut store, &jsonl).unwrap();
        assert_eq!(stats.imported, 1, "only the real-body line imports");
        assert_eq!(stats.skipped_malformed, 4, "number/null/missing/blank bodies are malformed");
        assert!(store.get_events_by_entity("NUM").unwrap().is_empty());
        assert!(store.get_events_by_entity("EMPTY").unwrap().is_empty());
        assert!(!store.get_events_by_entity("OK").unwrap().is_empty());
    }

    #[test]
    fn test_import_propagates_a_corrected_body_then_retracts() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.db");
        let mut store = EventStore::new(&db).unwrap();
        // first import
        let v1 = write_jsonl(dir.path(), "v1.jsonl", &[r#"{"entity_id":"DEC1","body":"the db lives on the desktop"}"#]);
        assert_eq!(import_jsonl(&mut store, &v1).unwrap().imported, 1);
        // the source corrected the fact: a changed body is REVISED, not skipped
        let v2 = write_jsonl(dir.path(), "v2.jsonl", &[r#"{"entity_id":"DEC1","body":"the db now lives on the NAS"}"#]);
        let s2 = import_jsonl(&mut store, &v2).unwrap();
        assert_eq!(s2.revised, 1, "a changed body propagates as a revision");
        assert_eq!(s2.imported, 0);
        assert!(
            recall(&store, "where does the db live NAS", 3).unwrap().iter().any(|h| h.entity_id == "DEC1" && h.body.contains("NAS")),
            "recall serves the corrected body"
        );
        assert!(
            recall(&store, "db desktop", 3).unwrap().iter().all(|h| !(h.entity_id == "DEC1" && h.body.contains("desktop"))),
            "the stale body no longer surfaces"
        );
        // re-import the identical corrected body -> idempotent skip
        assert_eq!(import_jsonl(&mut store, &v2).unwrap().skipped_existing, 1);
        // a status=superseded line retracts it
        let v3 = write_jsonl(dir.path(), "v3.jsonl", &[r#"{"entity_id":"DEC1","body":"the db now lives on the NAS","status":"superseded"}"#]);
        assert_eq!(import_jsonl(&mut store, &v3).unwrap().retracted, 1);
        assert!(
            recall(&store, "where does the db live NAS", 3).unwrap().iter().all(|h| h.entity_id != "DEC1"),
            "a superseded fact stops surfacing"
        );
    }

    #[test]
    fn test_import_still_revises_after_head_neutral_events() {
        // review finding: mark (fact_echoed), reproject and resolve append
        // parent-less, head-NEUTRAL events. The old parent-pointer heuristic
        // counted those as extra "heads" and froze the entity as false-diverged,
        // silently disabling source-correction propagation forever.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("n.db");
        let mut store = EventStore::new(&db).unwrap();
        let v1 = write_jsonl(dir.path(), "v1.jsonl", &[r#"{"entity_id":"M1","body":"the db lives on the desktop"}"#]);
        assert_eq!(import_jsonl(&mut store, &v1).unwrap().imported, 1);
        // the agent marks the fact useful (exactly what the tools tell it to do)
        store
            .append_event("s", "l", "a", EventKind::FactEchoed, "M1", None, "")
            .unwrap();
        // ...and a reproject also lands (thor backfill-projects stamps every import)
        store
            .append_event("s", "l", "a", EventKind::FactReprojected, "M1", None, r#"{"project":"ProjA"}"#)
            .unwrap();
        // the source corrects the fact: it must still propagate as a revision
        let v2 = write_jsonl(dir.path(), "v2.jsonl", &[r#"{"entity_id":"M1","body":"the db now lives on the NAS"}"#]);
        let s2 = import_jsonl(&mut store, &v2).unwrap();
        assert_eq!(s2.revised, 1, "head-neutral events must not freeze corrections");
        assert_eq!(s2.skipped_diverged, 0, "a marked entity is not diverged");
        assert!(
            recall(&store, "where does the db live NAS", 3).unwrap().iter().any(|h| h.entity_id == "M1" && h.body.contains("NAS")),
            "recall serves the corrected body"
        );
        // a status line still retracts it afterwards
        let v3 = write_jsonl(dir.path(), "v3.jsonl", &[r#"{"entity_id":"M1","body":"the db now lives on the NAS","status":"deleted"}"#]);
        assert_eq!(import_jsonl(&mut store, &v3).unwrap().retracted, 1);
    }

    #[test]
    fn test_import_refuses_dual_write_twin() {
        // The dual-write round-trip: a fact stored directly in THOR comes back
        // from the source under the SOURCE's id. The importer must refuse the
        // twin instead of minting a second live entity with the same body.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        store
            .append_event(
                "s", "l", "a", EventKind::FactCreated, "mcp-direct-write", None,
                "REGEL: the deploy tarball is copied with scp -O because the NAS has no SFTP subsystem",
            )
            .unwrap();
        let snap = write_jsonl(
            dir.path(),
            "t.jsonl",
            &[
                r#"{"entity_id":"01KROUNDTRIP","body":"REGEL: the deploy tarball is copied with scp -O because the NAS has no SFTP subsystem\n\n[memory/decision | tags: deploy | project: global | mimir:01KROUNDTRIP]"}"#,
                r#"{"entity_id":"01KFRESH","body":"a genuinely different fact about the backup schedule"}"#,
            ],
        );
        let stats = import_jsonl(&mut store, &snap).unwrap();
        assert_eq!(stats.skipped_duplicate, 1, "the twin is refused (footer stripped before compare)");
        assert_eq!(stats.imported, 1, "the genuinely new fact still imports");
        assert!(store.get_events_by_entity("01KROUNDTRIP").unwrap().is_empty());
    }

    #[test]
    fn test_import_and_idempotency() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = write_jsonl(
            dir.path(),
            "snap.jsonl",
            &[
                r#"{"entity_id":"NWT1CS","body":"a decision about THOR direction","actor":"user"}"#,
                r#"{"entity_id":"H0PXW2","body":"a milestone about mimir hardening"}"#,
                r#"   "#,
                r#"{"malformed json"#,
                r#"{"body":"no entity id here"}"#,
            ],
        );

        let db = dir.path().join("m.db");
        let mut store = EventStore::new(&db).unwrap();
        let stats = import_jsonl(&mut store, &jsonl).unwrap();
        assert_eq!(stats.imported, 2);
        assert_eq!(stats.skipped_malformed, 2, "malformed line + missing entity_id");
        assert_eq!(stats.skipped_existing, 0);

        // recall over the imported content works
        let hits = recall(&store, "THOR direction decision", 3).unwrap();
        assert!(hits.iter().any(|h| h.entity_id == "NWT1CS"));

        // re-import the same snapshot: everything is now skipped as existing
        let stats2 = import_jsonl(&mut store, &jsonl).unwrap();
        assert_eq!(stats2.imported, 0);
        assert_eq!(stats2.skipped_existing, 2, "both facts already present");
        assert_eq!(store.get_all_events().unwrap().len(), 2, "no duplicates appended");
    }
}
