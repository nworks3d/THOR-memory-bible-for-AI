//! The health check: one plain-language line per component, no jargon, no
//! silent gaps. Every reader here degrades to an honest "not configured" or
//! "unreachable" line instead of an error that stops the other lines from
//! printing - a health check that dies on the first broken thing is worse
//! than no health check.

use std::path::Path;
use thor_core::auditor::{verify_chain_integrity, DifferentialAuditor};
use thor_core::event_store::EventStore;

/// Component 1+2: does the store exist, how many events, is the chain
/// (hash continuity + the independent differential fold) intact.
pub fn store_line(db: &Path) -> String {
    if !db.exists() {
        return format!("memory store: not found at {}", db.display());
    }
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(e) => return format!("memory store: found at {}, but could not be opened ({e})", db.display()),
    };
    let events = match store.get_all_events() {
        Ok(e) => e,
        Err(e) => return format!("memory store: found at {} but could not be read ({e})", db.display()),
    };
    let chain_ok = verify_chain_integrity(&events).is_ok() && DifferentialAuditor::verify_consistency(&events).is_ok();
    if chain_ok {
        format!("memory store: {} events, chain intact", events.len())
    } else {
        let reason = verify_chain_integrity(&events)
            .err()
            .or_else(|| DifferentialAuditor::verify_consistency(&events).err())
            .unwrap_or_else(|| "unknown".to_string());
        format!("memory store: {} events, CHAIN BROKEN ({reason})", events.len())
    }
}

/// Component 3: is a code index configured, and if so, at which commit
/// (plus how far the working copy has drifted from it).
pub fn code_index_line(index_db: Option<&Path>, repo: Option<&Path>) -> String {
    match (index_db, repo) {
        (Some(index_db), Some(repo)) => {
            if !index_db.exists() {
                return format!("code index: not found at {}", index_db.display());
            }
            match serve::lookup::code_index_status(index_db, repo) {
                Ok(p) => match p.current_commit {
                    // The same COMMIT is not the same FILES. This line said
                    // "matches the working copy" while four files sat
                    // uncommitted - a rosier answer than the truth, and a
                    // line number out of the index is wrong for exactly those
                    // four files. Checked now, and said out loud.
                    Ok(current)
                        if current == p.indexed_commit
                            && p.uncommitted_changed.unwrap_or(0) == 0 =>
                    {
                        format!("code index: at commit {}, matches the working copy", short(&p.indexed_commit))
                    }
                    Ok(current) if current == p.indexed_commit => format!(
                        "code index: at commit {}, but {} file(s) in the working copy are \
                         uncommitted - their line numbers are stale",
                        short(&p.indexed_commit),
                        p.uncommitted_changed
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "an unknown number of".to_string())
                    ),
                    Ok(current) => format!(
                        "code index: at commit {}, working copy is at {} ({} file(s) differ)",
                        short(&p.indexed_commit),
                        short(&current),
                        p.files_differ.map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".to_string())
                    ),
                    Err(e) => format!(
                        "code index: at commit {}, but the repository could not be read ({e})",
                        short(&p.indexed_commit)
                    ),
                },
                Err(e) => format!("code index: could not be checked ({e})"),
            }
        }
        (None, None) => "code index: not configured".to_string(),
        _ => "code index: not configured (pass both the index and the repository to check one)".to_string(),
    }
}

/// Component 4: is a replica configured, and does it run level with us.
pub fn replica_line(db: &Path, replica: Option<(&str, &str)>) -> String {
    let Some((base, token)) = replica else {
        return "replica: not configured".to_string();
    };
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(e) => return format!("replica: configured at {base}, but the local store could not be opened ({e})"),
    };
    let (local_seq, _) = match store.contiguous_tip() {
        Ok(t) => t,
        Err(e) => return format!("replica: configured at {base}, but the local tip could not be read ({e})"),
    };
    match crate::transport::remote_cursor(base, token) {
        Ok(remote) => {
            let lag = local_seq - remote.contiguous_seq;
            if lag == 0 {
                format!("replica: {base}, in sync ({local_seq} events on both sides)")
            } else if lag > 0 {
                format!("replica: {base}, BEHIND by {lag} event(s) ({} of {local_seq})", remote.contiguous_seq)
            } else {
                format!("replica: {base}, AHEAD of us by {} event(s) - not a plain replica of this store", -lag)
            }
        }
        Err(e) => format!("replica: {base}, UNREACHABLE ({e})"),
    }
}

/// Component 5: how many fireable items (Rule/Orientation) still have no
/// falsifier - the worklist number, in plain language.
pub fn falsifier_line(db: &Path) -> String {
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(_) => return "falsifiers: memory store not available - nothing to count".to_string(),
    };
    let status = serve::status::store_status(&store);
    if status.fireable_total == 0 {
        return "falsifiers: no rules or orientations declared yet".to_string();
    }
    if status.missing_falsifier == 0 {
        format!("falsifiers: all {} rule(s)/orientation(s) have one", status.fireable_total)
    } else {
        format!(
            "falsifiers: {} of {} rule(s)/orientation(s) still have no falsifier",
            status.missing_falsifier, status.fireable_total
        )
    }
}

/// How many rules and orientations carry a machine-runnable proof, not just
/// prose - the number that decides how much of this memory can ever stop a
/// wrong write.
///
/// WHY THIS LINE EXISTS. The doctrine is that only a rule whose check runs
/// and holds may block, and that prose can inform but never forbid. That
/// was built, tested, and proven blind. What nobody could see was how much
/// of the store it actually applied to, because nothing counted it: this
/// health check reported falsifier coverage at 100% and read as healthy
/// while proof coverage sat at 2 items out of 2999 (measured 2026-08-06).
/// A capability nothing is wired into looks exactly like one that works.
pub fn proof_line(db: &Path) -> String {
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(_) => return "provable rules: memory store not available - nothing to count".to_string(),
    };
    let items = serve::live::live_items(&store);
    let fireable: Vec<_> = items
        .iter()
        .filter(|i| matches!(i.item.kind, model::item::Kind::Rule | model::item::Kind::Orientation))
        .collect();
    if fireable.is_empty() {
        return "provable rules: no rules or orientations declared yet".to_string();
    }
    let with_check = fireable.iter().filter(|i| i.item.check.is_some()).count();
    format!(
        "provable rules: {} of {} rule(s)/orientation(s) carry a runnable check ({:.1}%) - only those can ever block a wrong write, the rest can only inform",
        with_check,
        fireable.len(),
        100.0 * with_check as f64 / fireable.len() as f64
    )
}

/// What has quietly rotted since the last time anyone looked: anchors that
/// resolve to nothing, and proofs that now come out false.
///
/// WHY THIS LINE EXISTS. A memory decays between writes, not during them.
/// The write gate can refuse a bad declaration and the neighbourhood toll
/// can refuse a write next to rot, but neither ever fires while a file is
/// simply moved or deleted by ordinary work. On 2026-08-06 that had left
/// 128 anchors in four projects pointing at nothing: those facts fired
/// NOWHERE, and no surface said so. Repairing them took a full sweep. The
/// point of counting it here is that the next hundred announce themselves
/// instead of waiting for someone to go looking.
///
/// Needs `--checkouts`, for the same reason `orphan_projects_line` does: an
/// anchor is relative to a project's own working copy, and this refuses to
/// guess where that is.
pub fn decay_line(db: &Path, checkouts: Option<&Path>) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "decay: store unreadable, not checked".to_string();
    };
    let Some(root) = checkouts else {
        return "decay: pass --checkouts <dir> to check anchors and proofs against real working copies".to_string();
    };

    let mut roots: std::collections::BTreeMap<String, std::path::PathBuf> = Default::default();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    roots.insert(key, path);
                }
            }
        }
    }

    let items = serve::live::live_items(&store);
    let (mut dead, mut failing, mut judged) = (0usize, 0usize, 0usize);
    for li in items.iter().filter(|li| li.item.kind.can_fire()) {
        let Some(project) = li.item.project.as_deref() else { continue };
        let Some(base) = roots.get(project) else { continue };
        judged += 1;
        for binding in &li.item.bindings {
            if let model::item::Binding::Target { kind: model::item::TargetKind::Path, value } = binding {
                // An absolute path is somewhere else on this machine, not in
                // the checkout, so it is not this line's business.
                if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                    continue;
                }
                if !base.join(value.replace('\\', "/")).exists() {
                    dead += 1;
                }
            }
        }
        if let Some(check) = li.item.check.as_ref() {
            if model::check::run(check, base) == model::check::Outcome::Fails {
                failing += 1;
            }
        }
    }

    if judged == 0 {
        return "decay: no project under --checkouts answers to any item's key, nothing checked".to_string();
    }
    if dead == 0 && failing == 0 {
        return format!("decay: none - every anchor of {judged} project-scoped item(s) resolves, every proof holds");
    }
    format!(
        "decay: {dead} anchor(s) point at nothing (those facts fire NOWHERE) and {failing} proof(s) now come out FALSE, across {judged} project-scoped item(s)"
    )
}

/// How many items have been put in front of a reader over and over without
/// ever being judged either way.
///
/// WHY THIS IS A COUNT AND NOT A REFUSAL. Judging a served fact costs the
/// session that does it and pays only the next one, so it does not happen -
/// on this store, in its whole life, exactly four judgements had ever been
/// recorded, all on one day. The obvious fix is a toll, the way the write
/// gate tolls a write next to rot. It does not fit here: a toll needs
/// something local to hold hostage, and the worst offenders are the pinned
/// rules that fire at every session start with no target at all. The other
/// candidate, refusing at the end of a turn, is the capture guard's shape -
/// measured on a blind hold-out at 55% catch and 11.4% false blocks, and
/// recorded as not deployable.
///
/// So this does the one thing that demonstrably moved the other number:
/// count it where someone will see it. Proof coverage sat at 3 of 3004
/// until a line here said so out loud, and it is 246 a day later.
pub fn unjudged_line(db: &Path) -> String {
    const HEAVY: usize = 40;
    let Ok(store) = EventStore::open_existing(db) else {
        return "unjudged: store unreadable, not checked".to_string();
    };
    let Ok(events) = store.event_kinds() else {
        return "unjudged: the log could not be read, not checked".to_string();
    };
    use thor_core::event_store::EventKind;
    let mut served: std::collections::HashMap<String, usize> = Default::default();
    let mut judged: std::collections::HashSet<String> = Default::default();
    for (kind, id) in events {
        match kind {
            EventKind::ItemServed => *served.entry(id).or_default() += 1,
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise => {
                judged.insert(id);
            }
            _ => {}
        }
    }
    // Pinned items are excluded here for the same reason the Stop-time ask
    // excludes them: "did it belong where it fired" is already answered by
    // the owner having pinned it. Counting them would make this line
    // disagree with the mechanism it exists to report on.
    let pinned: std::collections::HashSet<String> = serve::live::live_items(&store)
        .into_iter()
        .filter(|li| li.item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always)))
        .map(|li| li.id)
        .collect();
    let heavy = served
        .iter()
        .filter(|(id, n)| **n >= HEAVY && !judged.contains(*id) && !pinned.contains(*id))
        .count();
    let total_unjudged = served.keys().filter(|id| !judged.contains(*id) && !pinned.contains(*id)).count();
    if total_unjudged == 0 {
        return "unjudged: every item that has ever fired has been judged at least once".to_string();
    }
    format!(
        "unjudged: {heavy} trigger-bound item(s) fired {HEAVY}+ times and were never judged either way ({total_unjudged} unjudged in total, pinned items excluded - the owner answered that question by pinning them) - `mark` is the only thing that ever retires noise, and silence decides nothing"
    )
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(8)]
}

/// Component 6: which mode surface 4's meaning search (feature `semantic`)
/// runs in right now - the exact same line `serve status` prints (see
/// `serve::semantic_paths::mode_line`), so the two can never disagree about
/// what is active. Never opens the store: whether the mode is active or
/// falling back depends only on the binary's own compile flags and the
/// model directory, not on any particular store's content.
pub fn semantic_line(model_dir: Option<&Path>) -> String {
    serve::semantic_paths::mode_line(model_dir)
}

/// Component 6: project keys held by live items that NO checkout under
/// `checkouts` resolves to - items that can never reach a pushed surface
/// because nothing answers to their name.
///
/// WHY THIS LINE EXISTS. Three separate defects on 2026-08-03 had exactly
/// one shape: an item named a project that resolution could not produce.
/// The global tier stored as a project called "global"; 1.0's marker file
/// name that 2.0 never read; a linked worktree resolving to its own slug.
/// All three were invisible from every surface - a rule scoped out looks
/// exactly like a rule that never matched. This turns that class from
/// silent into a line anyone can read.
pub fn orphan_projects_line(db: &Path, checkouts: Option<&Path>) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "project keys: store unreadable, not checked".to_string();
    };
    let items = serve::live::live_items(&store);
    let held: Vec<String> = items
        .iter()
        .filter(|li| li.item.kind.can_fire())
        .filter_map(|li| li.item.project.clone())
        .collect();
    if held.is_empty() {
        return "project keys: every fireable item is global, nothing to check".to_string();
    }

    let Some(root) = checkouts else {
        let distinct: std::collections::BTreeSet<&str> = held.iter().map(String::as_str).collect();
        return format!(
            "project keys: {} distinct key(s) in use; pass --checkouts <dir> to check that a real checkout answers to each one",
            distinct.len()
        );
    };

    let mut resolvable: std::collections::BTreeSet<String> = Default::default();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    resolvable.insert(key);
                }
            }
        }
    }
    let orphans = serve::project::orphaned_project_keys(held.iter().map(String::as_str), &resolvable);
    if orphans.is_empty() {
        return format!(
            "project keys: every key in use resolves to a checkout under {}",
            root.display()
        );
    }
    let detail: Vec<String> = orphans.iter().map(|(k, n)| format!("{k} ({n} item(s))")).collect();
    format!(
        "project keys: {} key(s) that no checkout under {} answers to, so their items only ever answer a lookup and never fire at a moment: {}",
        orphans.len(),
        root.display(),
        detail.join(", ")
    )
}

/// The whole report: one line per component, in a fixed order, so a person
/// (or a script) can read it top to bottom without guessing what is missing.
pub fn report(
    db: &Path,
    index_db: Option<&Path>,
    repo: Option<&Path>,
    replica: Option<(&str, &str)>,
    model_dir: Option<&Path>,
    checkouts: Option<&Path>,
) -> Vec<String> {
    vec![
        store_line(db),
        code_index_line(index_db, repo),
        replica_line(db, replica),
        falsifier_line(db),
        proof_line(db),
        decay_line(db, checkouts),
        unjudged_line(db),
        semantic_line(model_dir),
        orphan_projects_line(db, checkouts),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind};
    use model::store;

    fn rule(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "a fixture rule".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this fixture rule turns out to be wrong".to_string()),
            check: None,
        }
    }

    #[test]
    fn store_line_reports_missing_store_without_creating_one() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        let line = store_line(&missing);
        assert!(line.contains("not found"), "{line}");
        assert!(!missing.exists(), "the health check must never create a store");
    }

    #[test]
    fn store_line_counts_events_and_reports_an_intact_chain() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("r1")).unwrap();
        }
        let line = store_line(&db);
        assert!(line.contains("intact"), "{line}");
        assert!(line.contains('1'), "{line}");
    }

    #[test]
    fn code_index_line_says_plainly_when_not_configured() {
        assert_eq!(code_index_line(None, None), "code index: not configured");
    }

    #[test]
    fn replica_line_says_plainly_when_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert_eq!(replica_line(&db, None), "replica: not configured");
    }

    /// THE DEFECT THIS PREVENTS: a health check that reads as fully healthy
    /// while almost nothing in the store can actually block anything. Every
    /// other line here was green on 2026-08-06 with proof coverage at 2 of
    /// 2999, and no line said so.
    #[test]
    fn the_proof_line_reports_coverage_and_never_stays_silent_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("prose-only")).unwrap();
        }
        let line = proof_line(&db);
        assert!(line.contains("0 of 1"), "a store with no provable rule must say so: {line}");
        assert!(line.contains("block"), "and say what the number decides: {line}");
    }

    #[test]
    fn the_proof_line_is_part_of_the_report_not_an_optional_extra() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("prose-only")).unwrap();
        }
        let lines = report(&db, None, None, None, None, None);
        assert!(
            lines.iter().any(|l| l.starts_with("provable rules:")),
            "the health check must report proof coverage every run: {lines:?}"
        );
    }

    #[test]
    fn falsifier_line_counts_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("has-one")).unwrap();
            s.append_event(
                "s", "l", "a", thor_core::event_store::EventKind::FactCreated, "old-1", None,
                r#"{"id":"old-1","kind":"rule","text":"pre-migration","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null}"#,
            )
            .unwrap();
        }
        let store = EventStore::open_existing(&db).unwrap();
        let _ = store; // keep the connection alive for the duration of the call below
        let line = falsifier_line(&db);
        assert!(line.contains("1 of 2"), "{line}");
    }

    #[test]
    fn report_has_one_line_per_component_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let lines = report(&db, None, None, None, None, None);
        assert_eq!(lines.len(), 9);
        assert!(lines[0].starts_with("memory store:"));
        assert!(lines[1].starts_with("code index:"));
        assert!(lines[2].starts_with("replica:"));
        assert!(lines[3].starts_with("falsifiers:"));
        assert!(lines[4].starts_with("provable rules:"));
        assert!(lines[5].starts_with("decay:"));
        assert!(lines[6].starts_with("unjudged:"));
        assert!(lines[7].starts_with("semantic search:"));
        assert!(lines[8].starts_with("project keys:"));
    }

    // ------------------------------------------------------- semantic_line

    #[test]
    fn semantic_line_is_never_blank_regardless_of_the_store() {
        // Named after the defect it prevents: doctor must always state the
        // active search mode, in every build - this is the one health line
        // that never even opens the store to answer.
        let line = semantic_line(None);
        assert!(line.starts_with("semantic search:"), "{line}");
    }

    #[test]
    fn semantic_line_agrees_with_serves_own_search_mode() {
        // Deliberately never relies on which --features flag built THIS
        // binary (that would make the test's own expectation a guess): it
        // reads `serve::semantic_paths::search_mode` for the same explicit,
        // deliberately-empty directory and checks doctor's line agrees with
        // it - meaningful in either build. Passes an explicit temp dir
        // rather than `None`, since a real model may already be installed
        // at this machine's own default model directory.
        let dir = tempfile::tempdir().unwrap(); // no model files at all
        let line = semantic_line(Some(dir.path()));
        match serve::semantic_paths::search_mode(Some(dir.path())) {
            serve::semantic_paths::SearchMode::CompiledOut => assert!(line.contains("not compiled"), "{line}"),
            serve::semantic_paths::SearchMode::ModelMissing => assert!(line.contains("no model found"), "{line}"),
            serve::semantic_paths::SearchMode::ModelPresent => panic!("an empty directory must never read as present"),
        }
    }

    /// THE DEFECT THIS LINE EXISTS FOR. Three times on 2026-08-03 an item
    /// named a project that resolution could never produce, and every time it
    /// was invisible: a scoped-out rule looks exactly like a rule that never
    /// matched. This asserts the report NAMES such a key and says how many
    /// items hold it, rather than counting them as healthy.
    #[test]
    fn a_project_key_no_checkout_answers_to_is_named_with_its_weight() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut owned = rule("orphan-1");
            owned.project = Some("Nobody-Answers-To-This".to_string());
            store::declare(&mut store, "s", "l", "a", &owned).unwrap();
        }
        // A checkouts root with one real checkout that resolves to something else.
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Some-Other-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let line = orphan_projects_line(&db, Some(&checkouts));
        assert!(line.contains("Nobody-Answers-To-This"), "the key must be named: {line}");
        assert!(line.contains("1 item"), "and weighted by how many items hold it: {line}");
    }

    #[test]
    fn a_store_whose_keys_all_resolve_says_so_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut owned = rule("ok-1");
            owned.project = Some("Real-Repo".to_string());
            store::declare(&mut store, "s", "l", "a", &owned).unwrap();
        }
        let line = orphan_projects_line(&db, Some(&checkouts));
        assert!(line.contains("every key in use resolves"), "{line}");
    }
}
