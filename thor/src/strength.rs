//! ONE usage-strength concept, computed in ONE place and read by exactly TWO
//! consumers: the courier's slot-3 promotion (the only ranking consumer) and
//! consolidate's decay eligibility. Deliberately NOT a general recall score
//! term - two uncoordinated usage nudges on two layers was the pressure-tested
//! design risk this module exists to prevent.
//!
//! strength = recency-weighted fact_echoed events (synced, institutional
//! "this actually helped") + half-weighted, capped access counts (local reads:
//! MCP get / recall serves) - noise marks (local "this was noise for me
//! here"). Echoes decay by EVENTS behind the log tip (the hash-chained log
//! carries no wall clock). Noise and access live in the LOCAL ledger only:
//! "noise for me during this task" is not an institutional fact, and reads
//! never belong in the synced log.

use crate::event_store::EventStore;
use std::collections::HashMap;
use std::path::Path;

/// Echo recency half-life, in events behind the log tip: an echo from ~one
/// hygiene-cycle ago counts half.
const ECHO_HALF_LIFE_EVENTS: f64 = 2000.0;
/// A read is a weaker signal than a deliberate mark.
const ACCESS_WEIGHT: f64 = 0.5;
/// Access saturates: re-reading a fact endlessly must not make it unkillable
/// (gaming your own access count has no unbounded payoff).
const ACCESS_CAP: f64 = 4.0;
/// One noise mark cancels one full-strength echo.
const NOISE_WEIGHT: f64 = 1.0;

/// Usage strength per entity, for the given ids only - EVERY read here is
/// id-scoped SQL (`entity_id IN`/`k IN`), because this runs on the courier's
/// per-prompt hot path and the echo/access/noise stores all grow with total
/// history, never with the candidate pool. Duplicate ids (a diverged fact
/// surfacing once per contested head) are folded once, not double-counted.
/// Fail-soft everywhere: a query/ledger error contributes zero, never blocks.
pub fn strength_for(store: &EventStore, db: &Path, ids: &[String]) -> HashMap<String, f64> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let unique: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        ids.iter().filter(|id| seen.insert(id.as_str())).cloned().collect()
    };
    let tip = tip_seq(store);
    let mut out: HashMap<String, f64> = HashMap::new();
    for (id, seq) in echo_seqs(store, &unique) {
        let behind = (tip - seq).max(0) as f64;
        *out.entry(id).or_insert(0.0) += 0.5f64.powf(behind / ECHO_HALF_LIFE_EVENTS);
    }
    let access = crate::ledger::counters_for(db, "access", &unique);
    let noise = crate::ledger::counters_for(db, "noise", &unique);
    for id in &unique {
        let mut s = out.remove(id).unwrap_or(0.0);
        s += ACCESS_WEIGHT * (access.get(id).copied().unwrap_or(0) as f64).min(ACCESS_CAP);
        s -= NOISE_WEIGHT * noise.get(id).copied().unwrap_or(0) as f64;
        if s != 0.0 {
            out.insert(id.clone(), s);
        }
    }
    out
}

/// Convenience read of one entity's strength (absent = 0.0).
pub fn strength_of(store: &EventStore, db: &Path, id: &str) -> f64 {
    strength_for(store, db, &[id.to_string()]).remove(id).unwrap_or(0.0)
}

fn tip_seq(store: &EventStore) -> i64 {
    store
        .conn()
        .query_row("SELECT COALESCE(MAX(seq), 0) FROM event", [], |r| r.get(0))
        .unwrap_or(0)
}

/// (entity_id, seq) per fact_echoed event, restricted to `ids`.
fn echo_seqs(store: &EventStore, ids: &[String]) -> Vec<(String, i64)> {
    let conn = store.conn();
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT entity_id, seq FROM event WHERE kind = ? AND entity_id IN ({})",
        placeholders
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let params = std::iter::once(crate::event_store::EventKind::FactEchoed.as_str().to_string())
        .chain(ids.iter().cloned());
    let rows = match stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    }) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    rows.flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::{EventKind, EventStore};

    fn setup(dir: &Path) -> (EventStore, std::path::PathBuf) {
        let db = dir.join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "fact one").unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, "e2", None, "fact two").unwrap();
        (store, db)
    }

    #[test]
    fn echo_counts_with_recency_and_noise_subtracts() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = setup(dir.path());
        store.append_event("s", "l", "a", EventKind::FactEchoed, "e1", None, "echo").unwrap();
        let ids = vec!["e1".to_string(), "e2".to_string()];

        let s = strength_for(&store, &db, &ids);
        let fresh = s.get("e1").copied().unwrap_or(0.0);
        assert!(fresh > 0.9, "a fresh echo counts near-full: {fresh}");
        assert_eq!(s.get("e2"), None, "never touched = zero strength");

        // noise cancels: two noise marks drown one echo
        crate::ledger::increment(&db, "noise", "e1");
        crate::ledger::increment(&db, "noise", "e1");
        let s = strength_for(&store, &db, &ids);
        assert!(s.get("e1").copied().unwrap_or(0.0) < 0.0, "noise pushes strength negative");

        // access adds weakly and saturates at the cap
        for _ in 0..50 {
            crate::ledger::increment(&db, "access", "e2");
        }
        let s = strength_for(&store, &db, &ids);
        let read_only = s.get("e2").copied().unwrap_or(0.0);
        assert!(
            (read_only - ACCESS_WEIGHT * ACCESS_CAP).abs() < 1e-9,
            "50 reads saturate at the cap: {read_only}"
        );
    }

    #[test]
    fn reads_are_id_scoped_and_duplicates_fold_once() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = setup(dir.path());
        store.append_event("s", "l", "a", EventKind::FactEchoed, "e1", None, "echo").unwrap();
        crate::ledger::increment(&db, "access", "e1");
        crate::ledger::increment(&db, "access", "e1");
        // foreign ledger rows OUTSIDE the requested set must not leak in
        for i in 0..30 {
            crate::ledger::increment(&db, "access", &format!("other{i}"));
            crate::ledger::increment(&db, "noise", &format!("other{i}"));
        }
        let single = strength_for(&store, &db, &["e1".to_string()]);
        let doubled =
            strength_for(&store, &db, &["e1".to_string(), "e1".to_string()]);
        assert_eq!(
            single.get("e1"), doubled.get("e1"),
            "a diverged fact (same id twice in the pool) folds once, never double-counts"
        );
        assert_eq!(single.len(), 1, "foreign ids never appear in the result");
    }

    #[test]
    fn old_echoes_decay_by_event_distance() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, db) = setup(dir.path());
        store.append_event("s", "l", "a", EventKind::FactEchoed, "e1", None, "echo").unwrap();
        // push the tip far past the echo (one half-life of filler events)
        for i in 0..40 {
            store
                .append_event("s", "l", "a", EventKind::FactCreated, &format!("pad{i}"), None,
                    &format!("filler {i}"))
                .unwrap();
        }
        let s = strength_of(&store, &db, "e1");
        assert!(s > 0.0 && s < 1.0, "an aged echo counts less than a fresh one: {s}");
    }
}
