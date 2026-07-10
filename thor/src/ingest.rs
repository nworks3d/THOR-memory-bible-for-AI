//! Repo ingest: index a git repo's tracked text files into the store so recall
//! answers questions about the code itself, not just saved notes.
//!
//! Incremental against the current heads: a new chunk is CREATED, a changed
//! chunk is REVISED (fast-forward from its current head), an identical chunk is
//! SKIPPED, and a chunk whose file (or trailing chunk) disappeared is RETRACTED
//! so deleted code stops surfacing. Only `git ls-files` (tracked) paths are read,
//! so gitignored secrets are never ingested. Chunk ids are `<project>:<rel>#<n>`,
//! which is also how recall scopes a project (see `repo`).

use crate::event_store::{Event, EventKind, EventStore};
use crate::repo;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy)]
pub struct IngestStats {
    pub created: usize,
    pub revised: usize,
    pub unchanged: usize,
    pub retracted: usize,
    pub files: usize,
    pub skipped_binary: usize,
    pub skipped_big: usize,
    pub diverged_skipped: usize,
}

/// Body written when a chunk's file is gone: content-free so it can never match
/// a real recall query, and its old code rev stops being a head.
const TOMBSTONE: &str = "[retracted: removed from repo]";

/// Ingest each repo. `project_override` (e.g. `Some("@global")`) forces the project
/// key for every chunk instead of deriving it from the repo (used by
/// `thor ingest --global` to hold cross-cutting files in the always-in-scope tier).
pub fn ingest_repos(
    store: &mut EventStore,
    repos: &[PathBuf],
    actor: &str,
    project_override: Option<&str>,
) -> anyhow::Result<IngestStats> {
    let mut stats = IngestStats::default();
    for repo_arg in repos {
        // Canonicalize first: a relative path like "." has no file_name(), which
        // would leave the project unnamed. An absolute root also gives a stable
        // basename matching what the courier derives from a cwd. Strip Windows'
        // verbatim prefix so `git -C` / read_dir / join accept the path (incl. a
        // UNC path to the NAS: \\?\UNC\... -> \\...).
        let start = std::fs::canonicalize(repo_arg)
            .map(|p| repo::clean_verbatim_prefix(&p))
            .unwrap_or_else(|_| repo_arg.clone());
        // Source the file list. A git repo (a `.git` at or above `start`) uses
        // `git ls-files` (tracked-only, so gitignored secrets are never read). A
        // plain directory has no such guard, so we walk it directly (dotfiles and
        // heavy dirs skipped - see `repo::walk_files`). This is what lets THOR
        // index a loose docs folder, matching mimir's non-git doc collections.
        let (root, files, complete) = match repo::find_repo_root(&start) {
            Some(r) => {
                // `start` is inside a git repo. With an explicit key (--project) on a
                // strict SUBDIR, ingest ONLY that subtree (tracked-only, rebased to
                // the subdir) so a pinned key never slurps the whole parent repo.
                // Otherwise ingest the whole repo - the common case, and what a
                // SessionStart refresh from a subdir expects.
                let subrel = start
                    .strip_prefix(&r)
                    .ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .filter(|s| !s.is_empty());
                match (project_override, subrel) {
                    (Some(_), Some(sub)) => {
                        let prefix = format!("{}/", sub);
                        let files = repo::tracked_files(&r)
                            .into_iter()
                            .filter(|f| f.starts_with(&prefix))
                            .map(|f| f[prefix.len()..].to_string())
                            .collect();
                        (start.clone(), files, true)
                    }
                    _ => {
                        let files = repo::tracked_files(&r);
                        (r, files, true)
                    }
                }
            }
            None => {
                if !start.is_dir() {
                    eprintln!("thor ingest: skip (not a directory): {}", repo_arg.display());
                    continue;
                }
                let (files, complete) = repo::walk_files(&start);
                (start, files, complete)
            }
        };
        // Project key: an explicit override (--global / --project), else the repo's
        // project_key (a `.thor` marker, else the repo-root basename), else the
        // directory basename (a non-git folder with no marker) - matching what the
        // courier derives from the working directory.
        let project = match project_override {
            Some(p) => p.to_string(),
            None => match crate::repo::project_key(&root)
                .or_else(|| root.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
            {
                Some(k) => k,
                None => {
                    eprintln!(
                        "thor ingest: skip (cannot determine a project key): {}",
                        repo_arg.display()
                    );
                    continue;
                }
            },
        };

        // 1. desired state: entity_id -> body for every current chunk.
        let mut desired: HashMap<String, String> = HashMap::new();
        for rel in files {
            if repo::is_skip_ext(&rel) {
                stats.skipped_binary += 1;
                continue;
            }
            let mut text = match std::fs::read_to_string(root.join(&rel)) {
                Ok(t) => t,
                Err(_) => {
                    stats.skipped_binary += 1; // not utf-8 / unreadable
                    continue;
                }
            };
            if repo::truncate_to_max_file_chars(&mut text) {
                stats.skipped_big += 1;
            }
            let chunks = repo::chunk_text(&text, repo::MAX_CHUNK_CHARS);
            let total = chunks.len();
            // Markdown chunks carry their heading trail as a footer crumb, so
            // a chunk cut below its heading stays findable by section name.
            let trails = if repo::is_crumb_doc(&rel) {
                repo::heading_trails(&chunks)
            } else {
                vec![String::new(); total]
            };
            for (i, ch) in chunks.iter().enumerate() {
                desired.insert(
                    repo::chunk_entity_id(&project, &rel, i),
                    repo::chunk_body(ch, &project, &rel, i, total, &trails[i]),
                );
            }
            stats.files += 1;
        }

        // 2. current single-head state for this project's chunks.
        let all = store.get_all_events()?;
        let heads = crate::cas::compute_head_sets(&all);
        let by_rev: HashMap<&str, &Event> = all.iter().map(|e| (e.this_hash.as_str(), e)).collect();
        let prefix = format!("{}:", project);
        // eid -> (head_rev, head_body, head_kind); diverged/multi-head entities skipped.
        let mut current: HashMap<String, (String, String, EventKind)> = HashMap::new();
        for (eid, hs) in &heads {
            if !eid.starts_with(&prefix) {
                continue;
            }
            // Only chunk-shaped ids are managed by ingest. A project-scoped memory
            // (`<project>:mem-<uuid>`) also matches the prefix but must NEVER be
            // retracted as a vanished chunk.
            if !crate::repo::is_chunk_id(eid) {
                continue;
            }
            if hs.heads.len() != 1 {
                stats.diverged_skipped += 1;
                continue;
            }
            let rev = hs.heads.iter().next().unwrap();
            if let Some(ev) = by_rev.get(rev.as_str()) {
                current.insert(eid.clone(), (rev.clone(), ev.body.clone(), ev.kind));
            }
        }

        // 3. reconcile desired vs current: create / revise / skip.
        for (eid, body) in &desired {
            match current.get(eid) {
                Some((rev, cur_body, kind)) => {
                    let retracted = matches!(kind, EventKind::FactRetracted);
                    if !retracted && cur_body == body {
                        stats.unchanged += 1;
                    } else {
                        store.append_event(
                            "ingest",
                            "repo-ingest",
                            actor,
                            EventKind::FactRevised,
                            eid,
                            Some(rev),
                            body,
                        )?;
                        stats.revised += 1;
                    }
                }
                None => {
                    // Not a clean single head. If it exists at all it is diverged
                    // (counted above) - do not add another head; only create when
                    // the entity is genuinely new.
                    if heads.contains_key(eid) {
                        continue;
                    }
                    store.append_event(
                        "ingest",
                        "repo-ingest",
                        actor,
                        EventKind::FactCreated,
                        eid,
                        None,
                        body,
                    )?;
                    stats.created += 1;
                }
            }
        }

        // 4. retract chunks whose file (or trailing chunk) vanished. Skip entirely
        //    when a non-git walk was INCOMPLETE (an I/O error left part of the tree
        //    unread): a subtree we could not read is not the same as deleted, so
        //    retracting it would churn still-present content on a transient failure.
        if complete {
            for (eid, (rev, _body, kind)) in &current {
                if desired.contains_key(eid) || matches!(kind, EventKind::FactRetracted) {
                    continue;
                }
                store.append_event(
                    "ingest",
                    "repo-ingest",
                    actor,
                    EventKind::FactRetracted,
                    eid,
                    Some(rev),
                    TOMBSTONE,
                )?;
                stats.retracted += 1;
            }
        } else {
            eprintln!(
                "thor ingest: folder listing incomplete (I/O error) for '{}'; skipping retraction to avoid churning live content",
                project
            );
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall::recall;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "git {:?} failed", args);
    }

    fn write(dir: &std::path::Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    // Set up a real git repo so `git ls-files` (tracked-only) drives ingest.
    fn init_repo(dir: &std::path::Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
    }

    #[test]
    fn ingest_incremental_create_revise_skip_retract() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("MyProj");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_repo(&repo_dir);
        write(&repo_dir, "a.rs", "fn alpha() { let mesh_volume = 1; }");
        write(&repo_dir, "b.rs", "fn beta() {}");
        write(&repo_dir, "secret.env", "API_KEY=supersecret");
        git(&repo_dir, &["add", "a.rs", "b.rs"]); // secret.env NOT tracked
        let db = tmp.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();

        // first ingest: two files created, secret never read
        let s = ingest_repos(&mut store, &[repo_dir.clone()], "test", None).unwrap();
        assert_eq!(s.created, 2, "a.rs + b.rs chunks created");
        assert_eq!(s.files, 2);
        let hits = recall(&store, "mesh_volume alpha", 5).unwrap();
        assert!(hits.iter().any(|h| h.entity_id == "MyProj:a.rs#0"), "code is recallable");
        assert!(
            recall(&store, "supersecret API_KEY", 5).unwrap().is_empty(),
            "gitignored/untracked secret must never be ingested"
        );

        // re-ingest unchanged: everything skipped, no new events
        let n_before = store.get_all_events().unwrap().len();
        let s = ingest_repos(&mut store, &[repo_dir.clone()], "test", None).unwrap();
        assert_eq!(s.unchanged, 2);
        assert_eq!(s.created + s.revised + s.retracted, 0);
        assert_eq!(store.get_all_events().unwrap().len(), n_before, "no-op re-ingest writes nothing");

        // a project-scoped MEMORY shares the "MyProj:" prefix but is NOT a chunk;
        // ingest must never retract it as a vanished chunk.
        store
            .append_event("s", "l", "user", EventKind::FactCreated, "MyProj:mem-keepme", None, "remember the keepme widget")
            .unwrap();
        let s = ingest_repos(&mut store, &[repo_dir.clone()], "test", None).unwrap();
        assert_eq!(s.retracted, 0, "a project memory must not be retracted by ingest");
        assert!(
            recall(&store, "keepme widget", 5).unwrap().iter().any(|h| h.entity_id == "MyProj:mem-keepme"),
            "the project memory survives ingest"
        );

        // change a.rs -> revised
        write(&repo_dir, "a.rs", "fn alpha() { let mesh_volume = 42; new_line(); }");
        let s = ingest_repos(&mut store, &[repo_dir.clone()], "test", None).unwrap();
        assert_eq!(s.revised, 1);
        assert_eq!(s.unchanged, 1);

        // delete b.rs from the repo -> its chunk is retracted and stops surfacing
        std::fs::remove_file(repo_dir.join("b.rs")).unwrap();
        git(&repo_dir, &["rm", "-q", "b.rs"]);
        let s = ingest_repos(&mut store, &[repo_dir.clone()], "test", None).unwrap();
        assert_eq!(s.retracted, 1, "deleted file's chunk retracted");
        assert!(
            recall(&store, "beta", 5).unwrap().iter().all(|h| h.entity_id != "MyProj:b.rs#0"),
            "deleted code no longer surfaces in recall"
        );
    }

    #[test]
    fn jsonl_eval_corpus_is_skipped_and_stale_chunks_retract() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("MyProj");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_repo(&repo_dir);
        write(&repo_dir, "a.rs", "fn alpha() {}");
        write(
            &repo_dir,
            "eval/scenarios.jsonl",
            r#"{"id":"s01","task_prompt":"move the database to the NAS share"}"#,
        );
        git(&repo_dir, &["add", "a.rs", "eval/scenarios.jsonl"]); // BOTH tracked
        let db = tmp.path().join("j.db");
        let mut store = EventStore::new(&db).unwrap();

        // Simulate the pre-fix live store: the eval corpus was chunked in
        // before jsonl joined SKIP_EXT.
        store
            .append_event(
                "ingest", "repo-ingest", "test", EventKind::FactCreated,
                "MyProj:eval/scenarios.jsonl#0",
                None,
                "{\"id\":\"s01\",\"task_prompt\":\"move the database to the NAS share\"}\n\n[repo file | MyProj/eval/scenarios.jsonl | chunk 1/1]",
            )
            .unwrap();

        let s = ingest_repos(&mut store, &[repo_dir.clone()], "test", None).unwrap();
        assert_eq!(s.created, 1, "only a.rs is chunked; the tracked jsonl is skip-ext");
        assert!(s.skipped_binary >= 1, "the jsonl counts as skipped");
        assert_eq!(s.retracted, 1, "the stale pre-fix jsonl chunk is retracted by reconcile");
        assert!(
            recall(&store, "task_prompt database NAS share", 5)
                .unwrap()
                .iter()
                .all(|h| h.entity_id != "MyProj:eval/scenarios.jsonl#0"),
            "eval scenario text no longer surfaces in recall"
        );
        // idempotent: a second run neither re-creates nor re-retracts it
        let s2 = ingest_repos(&mut store, &[repo_dir.clone()], "test", None).unwrap();
        assert_eq!(s2.created + s2.revised + s2.retracted, 0, "clean no-op after the cleanup");
    }

    #[test]
    fn global_ingest_surfaces_in_every_project() {
        use crate::recall::{recall_scoped, RecallScope};
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("shared-docs");
        std::fs::create_dir_all(&shared).unwrap();
        init_repo(&shared);
        write(&shared, "dev-loop.md", "the dev loop: build, test, commit cleanly");
        git(&shared, &["add", "."]);
        let db = tmp.path().join("g.db");
        let mut store = EventStore::new(&db).unwrap();

        // ingest AS GLOBAL (the @global tier)
        let s = ingest_repos(&mut store, &[shared.clone()], "test", Some("@global")).unwrap();
        assert!(s.created >= 1);
        assert!(
            store.get_all_events().unwrap().iter().any(|e| e.entity_id.starts_with("@global:dev-loop.md#")),
            "cross-cutting file minted under the @global namespace"
        );
        // THE VISION: it surfaces from an UNRELATED project's scope
        let scope = RecallScope::current(Some("SomeOtherProject".to_string()));
        let hits = recall_scoped(&store, "dev loop build test", 5, &scope).unwrap();
        assert!(
            hits.iter().any(|h| h.entity_id.starts_with("@global:")),
            "global cross-cutting docs surface in every project"
        );
    }

    #[test]
    fn ingest_non_git_dir_walks_and_scopes() {
        // A plain (NON-git) folder is walked directly - mimir parity for a loose docs
        // folder. It is keyed by its basename, dotfiles/heavy dirs are never read, and
        // the same incremental create/no-op/retract holds.
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("LooseDocs"); // NOT a git repo
        std::fs::create_dir_all(proj.join("notes")).unwrap();
        write(&proj, "notes/design.md", "the vapor smoothing chamber uses acetone reflux");
        write(&proj, ".secret", "API_KEY=doNotIngest"); // dotfile -> never walked
        write(&proj, "node_modules/junk.js", "console.log('ignore me')"); // heavy dir -> skipped
        let db = tmp.path().join("n.db");
        let mut store = EventStore::new(&db).unwrap();

        let s = ingest_repos(&mut store, &[proj.clone()], "test", None).unwrap();
        assert!(s.created >= 1, "a non-git folder's text files are ingested");
        assert!(
            recall(&store, "vapor smoothing acetone", 5)
                .unwrap()
                .iter()
                .any(|h| h.entity_id == "LooseDocs:notes/design.md#0"),
            "a non-git folder is recallable, keyed by its basename"
        );
        assert!(
            recall(&store, "API_KEY doNotIngest", 5).unwrap().is_empty(),
            "a dotfile in a non-git folder is never ingested (no .gitignore to lean on)"
        );
        assert!(
            recall(&store, "console.log ignore", 5).unwrap().is_empty(),
            "a heavy dir (node_modules) is never walked"
        );

        // re-ingest unchanged -> no-op
        let n_before = store.get_all_events().unwrap().len();
        let s = ingest_repos(&mut store, &[proj.clone()], "test", None).unwrap();
        assert_eq!(s.created + s.revised + s.retracted, 0, "re-ingest of a non-git folder is a no-op");
        assert_eq!(store.get_all_events().unwrap().len(), n_before);

        // delete the file -> its chunk is retracted
        std::fs::remove_file(proj.join("notes/design.md")).unwrap();
        let s = ingest_repos(&mut store, &[proj.clone()], "test", None).unwrap();
        assert_eq!(s.retracted, 1, "a vanished file in a non-git folder is retracted");
        assert!(
            recall(&store, "vapor smoothing acetone", 5)
                .unwrap()
                .iter()
                .all(|h| h.entity_id != "LooseDocs:notes/design.md#0"),
            "deleted non-git content no longer surfaces"
        );
    }

    #[test]
    fn ingest_project_override_pins_key() {
        // The NAS use case: the source folder is named "Gadget V1" but the
        // canonical key (how you open the project) is "ProjC". --project
        // pins it regardless of the folder name.
        use crate::recall::{recall_scoped, RecallScope};
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("Gadget V1"); // NON-git, NAS-style name
        std::fs::create_dir_all(&proj).unwrap();
        write(&proj, "README.md", "the gadget vapor finishing station");
        let db = tmp.path().join("p.db");
        let mut store = EventStore::new(&db).unwrap();

        let s = ingest_repos(&mut store, &[proj.clone()], "test", Some("ProjC")).unwrap();
        assert!(s.created >= 1);
        assert!(
            store
                .get_all_events()
                .unwrap()
                .iter()
                .any(|e| e.entity_id.starts_with("ProjC:README.md#")),
            "--project pins the key, not the folder basename"
        );
        let scope = RecallScope::current(Some("ProjC".to_string()));
        assert!(
            recall_scoped(&store, "gadget vapor finishing", 5, &scope)
                .unwrap()
                .iter()
                .any(|h| h.entity_id.starts_with("ProjC:")),
            "recall in the pinned project finds the ingested folder"
        );
    }

    #[test]
    fn ingest_subdir_with_project_pins_only_that_subtree() {
        // --project on a strict SUBDIR of a git repo must ingest ONLY that subtree
        // (tracked-only, rebased to it), never slurp the whole parent repo under the
        // pinned key, and never read a gitignored secret.
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("BigRepo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_repo(&repo_dir);
        write(&repo_dir, "keep.rs", "fn root_level_widget() {}");
        write(&repo_dir, "sub/inside.md", "the pinned subfolder note about turbo mode");
        write(&repo_dir, "sub/secret.env", "API_KEY=leaked");
        write(&repo_dir, ".gitignore", "secret.env\n");
        git(&repo_dir, &["add", "keep.rs", "sub/inside.md", ".gitignore"]); // secret.env NOT tracked
        let db = tmp.path().join("s.db");
        let mut store = EventStore::new(&db).unwrap();

        let s = ingest_repos(&mut store, &[repo_dir.join("sub")], "test", Some("Pinned")).unwrap();
        assert_eq!(s.created, 1, "only the subdir's tracked file is ingested, not the whole repo");
        let events = store.get_all_events().unwrap();
        assert!(
            events.iter().any(|e| e.entity_id == "Pinned:inside.md#0"),
            "chunk keyed by --project and rebased to the subdir (rel = inside.md, not sub/inside.md)"
        );
        assert!(
            events.iter().all(|e| !e.entity_id.contains("keep.rs")),
            "the parent repo's root files are NOT slurped under the pinned key"
        );
        assert!(
            recall(&store, "API_KEY leaked secret", 5).unwrap().is_empty(),
            "a gitignored secret in the subdir is never ingested (still tracked-only)"
        );
    }
}
