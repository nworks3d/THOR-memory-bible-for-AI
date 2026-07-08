use crate::event_store::{EventKind, EventStore};
use std::io::BufRead;
use std::path::Path;

#[derive(Debug, Default, Clone, Copy)]
pub struct ImportStats {
    pub imported: usize,
    pub skipped_existing: usize,
    pub skipped_malformed: usize,
}

/// Import facts from a JSONL snapshot (one object per line). THOR never opens
/// the source memory store (e.g. the live mimir DB): an external, read-only
/// exporter produces the JSONL, so the import is fully decoupled and cannot
/// touch the source.
///
/// Each line: `{"entity_id": "...", "body": "...", "actor": "..."?}`.
/// Every record becomes one `fact_created` for `entity_id`. Idempotent: an
/// entity that already has any event is left untouched, so re-running the
/// import (or importing an overlapping snapshot) never duplicates a fact.
pub fn import_jsonl(store: &mut EventStore, path: &Path) -> anyhow::Result<ImportStats> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut stats = ImportStats::default();

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

        // Idempotency: skip an entity that already exists in the store.
        if !store.get_events_by_entity(&entity_id)?.is_empty() {
            stats.skipped_existing += 1;
            continue;
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
