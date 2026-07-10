pub mod event_store;
pub mod cas;
pub mod footer;
pub mod auditor;
pub mod recall;
pub mod repo;
pub mod ingest;
pub mod ledger;
pub mod review;
pub mod consolidate;
#[cfg(feature = "semantic")]
pub mod embed;
#[cfg(feature = "semantic")]
pub mod vectors;
#[cfg(feature = "semantic")]
pub mod embed_daemon;
#[cfg(feature = "semantic")]
pub mod rerank;
pub mod courier;
pub mod importer;
pub mod mcp;
pub mod sync;
pub mod guard;
pub mod install;
pub mod backup;
pub mod cli;

#[cfg(test)]
mod comprehensive_tests {
    use crate::auditor::{verify_chain_integrity, DifferentialAuditor};
    use crate::cas::compute_head_sets;
    use crate::event_store::{EventKind, EventStore, ResolveConflict};
    use std::collections::HashMap;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn test_1_replay_determinism() {
        // replay must be recomputed from a REOPENED on-disk log and land on
        // the same head-sets as the live fold before the store was dropped
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("replay.db");

        let heads_before;
        {
            let mut store = EventStore::new(&db_path).unwrap();
            let ev1 = store
                .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body1")
                .unwrap();
            store
                .append_event(
                    "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(&ev1.this_hash), "body2",
                )
                .unwrap();
            store
                .append_event(
                    "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-parent"), "body3",
                )
                .unwrap();
            heads_before = compute_head_sets(&store.get_all_events().unwrap());
        } // store dropped: connection closed, everything must come from disk now

        let store = EventStore::new(&db_path).unwrap();
        let events = store.get_all_events().unwrap();
        let heads_after = compute_head_sets(&events);

        assert_eq!(heads_before.len(), heads_after.len());
        for (entity_id, head_set_before) in heads_before.iter() {
            let head_set_after = heads_after
                .get(entity_id)
                .expect("entity must survive the persistence boundary");
            assert_eq!(head_set_before.heads, head_set_after.heads);
            assert_eq!(head_set_before.is_diverged, head_set_after.is_diverged);
        }
    }

    #[test]
    fn test_2_purity_no_time() {
        // (1) source-level guarantee: the fold modules never read clocks or locale
        let cas_src = include_str!("cas.rs");
        let auditor_src = include_str!("auditor.rs");
        for (name, src) in [("cas.rs", cas_src), ("auditor.rs", auditor_src)] {
            for banned in ["SystemTime", "Instant", "chrono", "std::time", "Local::now", "Utc::now"] {
                assert!(
                    !src.contains(banned),
                    "{} must not take time/locale input (found {})",
                    name,
                    banned
                );
            }
        }

        // (2) behavioral: two logs whose bodies differ ONLY in time-shaped
        // provenance must classify identically (same head positions by seq,
        // same divergence flag)
        fn head_positions(timestamp: &str) -> (Vec<i64>, bool) {
            let mut store = EventStore::in_memory().unwrap();
            let ev1 = store
                .append_event(
                    "s1", "l1", "a1", EventKind::FactCreated, "e1", None,
                    &format!("the fact | observed_at={}", timestamp),
                )
                .unwrap();
            store
                .append_event(
                    "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(&ev1.this_hash),
                    &format!("the fact v2 | observed_at={}", timestamp),
                )
                .unwrap();
            store
                .append_event(
                    "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-parent"),
                    &format!("the fact v3 | observed_at={}", timestamp),
                )
                .unwrap();

            let events = store.get_all_events().unwrap();
            let heads = compute_head_sets(&events);
            let head_set = &heads["e1"];
            let seq_by_hash: HashMap<&str, i64> =
                events.iter().map(|e| (e.this_hash.as_str(), e.seq)).collect();
            let mut positions: Vec<i64> = head_set
                .heads
                .iter()
                .map(|h| seq_by_hash[h.as_str()])
                .collect();
            positions.sort();
            (positions, head_set.is_diverged)
        }

        let a = head_positions("2026-01-01T00:00:00Z");
        let b = head_positions("1999-12-31T23:59:59+11:00");
        assert_eq!(
            a, b,
            "time-shaped body provenance must not change head classification"
        );
    }

    #[test]
    fn test_3_cas_miss_is_branch() {
        let mut store = EventStore::in_memory().unwrap();

        let ev1 = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body1")
            .unwrap();

        let ev2 = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(&ev1.this_hash), "body2",
            )
            .unwrap();

        let stale_hash = "old_rev_hash";
        let ev3 = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(stale_hash), "body3",
            )
            .unwrap();

        let events = store.get_all_events().unwrap();
        let heads = compute_head_sets(&events);

        let head_set = &heads["e1"];
        assert!(head_set.is_diverged, "Entity should be diverged");
        assert_eq!(head_set.heads.len(), 2, "Should have exactly 2 heads");
        assert!(head_set.heads.contains(&ev2.this_hash), "ev2 hash should be in heads");
        assert!(head_set.heads.contains(&ev3.this_hash), "ev3 hash should be in heads");

        assert_eq!(events.len(), 3, "All events should be present in log");
    }

    #[test]
    fn test_4_cas_hit_fast_forwards() {
        let mut store = EventStore::in_memory().unwrap();

        let ev1 = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body1")
            .unwrap();

        let ev2 = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(&ev1.this_hash), "body2",
            )
            .unwrap();

        let events = store.get_all_events().unwrap();
        let heads = compute_head_sets(&events);

        let head_set = &heads["e1"];
        assert!(!head_set.is_diverged, "Entity should not be diverged");
        assert_eq!(head_set.heads.len(), 1, "Should have exactly 1 head");
        assert!(head_set.heads.contains(&ev2.this_hash), "ev2 hash should be the sole head");
    }

    #[test]
    fn test_5_concurrent_appends_one_chain() {
        // N INDEPENDENT connections to the same file DB, appending from N
        // threads with NO shared Rust lock: serialization must come from
        // BEGIN IMMEDIATE + busy_timeout alone
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("concurrent.db");
        {
            // create the schema once before the race
            let _ = EventStore::new(&db_path).unwrap();
        }

        let n: usize = 8;
        let barrier = Arc::new(Barrier::new(n));
        let mut handles = vec![];
        for i in 0..n {
            let path = db_path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let mut store = EventStore::new(&path).unwrap();
                barrier.wait();
                store.append_event(
                    "s1",
                    "l1",
                    &format!("actor{}", i),
                    EventKind::FactCreated,
                    &format!("e{}", i),
                    None,
                    &format!("body{}", i),
                )
            }));
        }

        for handle in handles {
            let result = handle.join().unwrap();
            assert!(
                result.is_ok(),
                "no append may error (no dropped write): {:?}",
                result.err()
            );
        }

        let store = EventStore::new(&db_path).unwrap();
        let events = store.get_all_events().unwrap();
        assert_eq!(events.len(), n, "all {} events must be present", n);

        let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
        let expected: Vec<i64> = (1..=n as i64).collect();
        assert_eq!(seqs, expected, "seqs must be exactly 1..=N, no gaps or dups");

        assert!(
            verify_chain_integrity(&events).is_ok(),
            "a single unbroken hash chain must result"
        );
    }

    #[test]
    fn test_6_event_uuid_dedup() {
        let mut store = EventStore::in_memory().unwrap();

        let event1 = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body1")
            .unwrap();

        let result = store.conn().execute(
            "INSERT INTO event (seq, event_uuid, session_id, lineage_id, actor, kind, entity_id,
             parent_rev, body, body_ch, prev_hash, this_hash)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                store.get_next_seq().unwrap(),
                &event1.event_uuid,
                "s1",
                "l1",
                "a1",
                "fact_created",
                "e2",
                None::<String>,
                "body2",
                "body2",
                event1.this_hash,
                "newhash"
            ],
        );

        assert!(
            result.is_err(),
            "Duplicate event_uuid should be rejected by UNIQUE constraint"
        );

        let events = store.get_all_events().unwrap();
        assert_eq!(events.len(), 1, "Only original event should exist");
    }

    #[test]
    fn test_7_crash_mid_append() {
        // simulate a crash mid-append: BEGIN IMMEDIATE + INSERT on a second
        // connection, then drop it WITHOUT commit. SQLite rolls the open
        // transaction back; the reopened log must not contain the torn write.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("crash.db");

        {
            let mut store = EventStore::new(&db_path).unwrap();
            store
                .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body1")
                .unwrap();
        }

        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            conn.execute(
                "INSERT INTO event (seq, event_uuid, session_id, lineage_id, actor, kind, entity_id,
                 parent_rev, body, body_ch, prev_hash, this_hash)
                 VALUES (2, 'torn-uuid', 's1', 'l1', 'a1', 'fact_created', 'e2', NULL,
                         'torn body', 'torn body', 'bogus-prev', 'bogus-this')",
                [],
            )
            .unwrap();
            // dropped here with the transaction still open: no COMMIT
        }

        {
            let store = EventStore::new(&db_path).unwrap();
            let events = store.get_all_events().unwrap();
            assert_eq!(
                events.len(),
                1,
                "the uncommitted event must be absent after reopen"
            );
            assert!(
                verify_chain_integrity(&events).is_ok(),
                "the chain must be contiguous and valid"
            );
        }
    }

    #[test]
    fn test_8_fsck_catches_corruption() {
        let mut store = EventStore::in_memory().unwrap();

        let ev1 = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body1")
            .unwrap();

        let events = store.get_all_events().unwrap();

        let corruption_test_gap = vec![
            crate::event_store::Event {
                seq: 1,
                event_uuid: "uuid1".to_string(),
                session_id: "s1".to_string(),
                lineage_id: "l1".to_string(),
                actor: "a1".to_string(),
                kind: EventKind::FactCreated,
                entity_id: "e1".to_string(),
                parent_rev: None,
                body: "body1".to_string(),
                body_ch: "body1".to_string(),
                prev_hash: "".to_string(),
                this_hash: ev1.this_hash.clone(),
            },
            crate::event_store::Event {
                seq: 3,
                event_uuid: "uuid2".to_string(),
                session_id: "s1".to_string(),
                lineage_id: "l1".to_string(),
                actor: "a1".to_string(),
                kind: EventKind::FactRevised,
                entity_id: "e1".to_string(),
                parent_rev: Some(ev1.this_hash.clone()),
                body: "body2".to_string(),
                body_ch: "body2".to_string(),
                prev_hash: ev1.this_hash.clone(),
                this_hash: "hash2".to_string(),
            },
        ];

        let result = verify_chain_integrity(&corruption_test_gap);
        assert!(
            result.is_err(),
            "fsck should detect non-contiguous seq"
        );

        let fork_test = vec![
            events[0].clone(),
            crate::event_store::Event {
                seq: 2,
                event_uuid: "uuid2".to_string(),
                session_id: "s1".to_string(),
                lineage_id: "l1".to_string(),
                actor: "a1".to_string(),
                kind: EventKind::FactRevised,
                entity_id: "e1".to_string(),
                parent_rev: Some(ev1.this_hash.clone()),
                body: "body2a".to_string(),
                body_ch: "body2a".to_string(),
                prev_hash: ev1.this_hash.clone(),
                this_hash: "hash2a".to_string(),
            },
            crate::event_store::Event {
                seq: 2,
                event_uuid: "uuid3".to_string(),
                session_id: "s1".to_string(),
                lineage_id: "l1".to_string(),
                actor: "a1".to_string(),
                kind: EventKind::FactRevised,
                entity_id: "e1".to_string(),
                parent_rev: Some(ev1.this_hash.clone()),
                body: "body2b".to_string(),
                body_ch: "body2b".to_string(),
                prev_hash: ev1.this_hash.clone(),
                this_hash: "hash2b".to_string(),
            },
        ];

        let result = crate::auditor::detect_fork(&fork_test);
        assert!(result.is_err(), "fsck should detect fork");
    }

    #[test]
    fn test_9_resolve_rejects_stale_head_set() {
        let mut store = EventStore::in_memory().unwrap();
        let rev_a = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body A")
            .unwrap()
            .this_hash;
        let rev_b = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-1"), "body B",
            )
            .unwrap()
            .this_hash;
        // a third head appears that the resolver never saw
        let rev_c = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-2"), "body C",
            )
            .unwrap()
            .this_hash;

        let result = store.append_resolve("s1", "l1", "cli", "e1", &rev_a, &[rev_b.clone()]);
        assert!(
            result.is_err(),
            "a resolve citing a stale head-set must be rejected"
        );
        let conflict = result
            .unwrap_err()
            .downcast::<ResolveConflict>()
            .expect("rejection carries the typed conflict");
        assert_eq!(
            conflict.current_heads.len(),
            3,
            "the fresh head-set is returned to the caller"
        );

        let events = store.get_all_events().unwrap();
        let heads = &compute_head_sets(&events)["e1"];
        assert!(
            heads.heads.contains(&rev_c),
            "the head the resolver never saw must NOT be removed"
        );
        assert_eq!(heads.heads.len(), 3, "nothing was written, heads unchanged");
    }

    #[test]
    fn test_10_partial_resolve_never_drops_unlisted_heads() {
        let mut store = EventStore::in_memory().unwrap();
        let rev_a = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body A")
            .unwrap()
            .this_hash;
        let rev_b = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-1"), "body B",
            )
            .unwrap()
            .this_hash;
        let rev_c = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-2"), "body C",
            )
            .unwrap()
            .this_hash;

        // inject a fact_resolved that cites only part of the head-set,
        // bypassing the CAS guard (simulates a buggy or malicious writer)
        let body = serde_json::json!({ "keep_rev": rev_a, "discarded": [rev_b.clone()] }).to_string();
        store
            .append_event("s1", "l1", "a1", EventKind::FactResolved, "e1", None, &body)
            .unwrap();

        let events = store.get_all_events().unwrap();
        let heads = &compute_head_sets(&events)["e1"];
        assert!(
            heads.heads.contains(&rev_c),
            "a head not listed in discarded[] must survive"
        );
        assert!(
            heads.heads.contains(&rev_b),
            "an invalid partial resolve is a no-op, not a partial apply"
        );
        assert_eq!(heads.heads.len(), 3);

        // both folds must agree that it was a no-op
        assert!(DifferentialAuditor::verify_consistency(&events).is_ok());
    }

    #[test]
    fn test_11_resolve_keep_must_be_current_head() {
        let mut store = EventStore::in_memory().unwrap();
        let rev_a = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body A")
            .unwrap()
            .this_hash;

        let result = store.append_resolve("s1", "l1", "cli", "e1", "never-a-head", &[rev_a.clone()]);
        assert!(
            result.is_err(),
            "a keep_rev that was never a head must be rejected"
        );

        let events = store.get_all_events().unwrap();
        let heads = &compute_head_sets(&events)["e1"];
        assert_eq!(heads.heads.len(), 1);
        assert!(heads.heads.contains(&rev_a), "the real head is untouched");
    }

    #[test]
    fn test_12_resolve_happy_path_collapses_to_keep() {
        let mut store = EventStore::in_memory().unwrap();
        let rev_a = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body A")
            .unwrap()
            .this_hash;
        let rev_b = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-1"), "body B",
            )
            .unwrap()
            .this_hash;
        let rev_c = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-2"), "body C",
            )
            .unwrap()
            .this_hash;

        store
            .append_resolve("s1", "l1", "cli", "e1", &rev_a, &[rev_b, rev_c])
            .unwrap();

        let events = store.get_all_events().unwrap();
        let heads = &compute_head_sets(&events)["e1"];
        assert_eq!(heads.heads.len(), 1, "resolved down to the kept rev");
        assert!(heads.heads.contains(&rev_a));
        assert!(!heads.is_diverged);

        assert!(verify_chain_integrity(&events).is_ok());
        assert!(DifferentialAuditor::verify_consistency(&events).is_ok());
    }
}
