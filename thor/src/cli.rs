use crate::auditor::{detect_fork, verify_chain_integrity, DifferentialAuditor};
use crate::cas::compute_head_sets;
use crate::event_store::{Event, EventKind, EventStore, ResolveConflict};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "thor")]
#[command(about = "THOR M0/M1 - Central, lossless event store + recall courier", long_about = None)]
struct Cli {
    /// Store path. Defaults to the central per-user store
    /// (%LOCALAPPDATA%\thor\thor.db), so every subcommand shares one store.
    #[arg(long)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

/// The default central store, kept out of any repo so create/recall/courier all
/// agree on one location without a flag. Falls back to a cwd-relative file if
/// the platform dir is unavailable.
fn default_db_path() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let dir = Path::new(&local).join("thor");
        let _ = std::fs::create_dir_all(&dir);
        return dir.join("thor.db");
    }
    PathBuf::from("thor.db")
}

/// True iff `db` is a Windows UNC / network path (\\server\share or the verbatim
/// \\?\UNC\ form). Local disks (C:\, \\?\C:\) and relative paths are NOT network.
/// On non-Windows an NFS mount is indistinguishable from a local path by name, so
/// this returns false there (documented limitation; the NAS uses a local volume).
#[cfg(windows)]
fn is_network_path(db: &Path) -> bool {
    use std::path::{Component, Prefix};
    matches!(
        db.components().next(),
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    )
}
#[cfg(not(windows))]
fn is_network_path(_db: &Path) -> bool {
    false
}

/// Refuse to open the store over a network path. SQLite's WAL requires real
/// shared memory; over SMB/UNC the WAL index corrupts silently. The authority's
/// db must live on ONE machine's LOCAL disk - other machines get a replica via
/// `thor ship` / `thor recv`, never a shared network file.
fn refuse_network_db(db: &Path) -> Result<()> {
    if is_network_path(db) {
        anyhow::bail!(
            "refusing to open the THOR store over a network path ({}): SQLite WAL corrupts over SMB/UNC. \
             Keep the db on a LOCAL disk and replicate to other machines with `thor ship` / `thor recv` instead.",
            db.display()
        );
    }
    Ok(())
}

#[derive(Subcommand)]
enum Commands {
    Create {
        entity_id: String,
        #[arg(long, default_value = "test_session")]
        session_id: String,
        #[arg(long, default_value = "test_lineage")]
        lineage_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        body: String,
    },
    Revise {
        entity_id: String,
        parent_rev: String,
        body: String,
        #[arg(long, default_value = "test_session")]
        session_id: String,
        #[arg(long, default_value = "test_lineage")]
        lineage_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
    },
    Get {
        entity_id: String,
    },
    History {
        entity_id: String,
    },
    Recall {
        query: String,
    },
    Resolve {
        entity_id: String,
        keep_rev: String,
        #[arg(long)]
        discarded: Vec<String>,
        #[arg(long, default_value = "test_session")]
        session_id: String,
        #[arg(long, default_value = "test_lineage")]
        lineage_id: String,
    },
    Fsck,
    /// Per-hook recall courier: reads the UserPromptSubmit hook JSON on stdin
    /// and prints a THOR recall block to stdout. Hard fail-open, always exit 0.
    Courier,
    /// Run as an MCP stdio server (newline-delimited JSON-RPC on stdin/stdout),
    /// exposing recall/get/remember. Register with: claude mcp add thor -- <thor.exe> mcp
    Mcp {
        /// Serve Streamable-HTTP on <bind> (e.g. 127.0.0.1:8078) for the remote
        /// NAS connector, instead of local stdio. Bind to localhost and front it
        /// with an auth gate (Cloudflare Access), exactly like mimir's remote MCP.
        #[arg(long)]
        http: Option<String>,
    },
    /// Moment-of-action Guard: reads a PreToolUse hook JSON on stdin and, if a
    /// rulebook rule matches the tool call, emits an advisory additionalContext.
    /// Hard fail-open, always exit 0.
    Guard {
        #[arg(long)]
        rulebook: Option<PathBuf>,
    },
    /// Response Guard: reads a Stop-hook JSON on stdin and, if the assistant's
    /// last message matches a response rule (e.g. it asked the user to do
    /// something it could do itself), emits {"decision":"block","reason":...}
    /// so the model reconsiders before yielding. Hard fail-open, always exit 0.
    StopGuard {
        #[arg(long)]
        rulebook: Option<PathBuf>,
    },
    /// Install THOR's hooks into Claude Code settings.json (one command, no hand
    /// editing) - the Guard hooks by default, +courier with --with-courier.
    /// Safe: backs up, refuses invalid JSON, only ADDS THOR entries, idempotent.
    Install {
        /// settings.json to write (default: the global ~/.claude/settings.json).
        #[arg(long)]
        settings: Option<PathBuf>,
        /// Also install the PreToolUse command guard (project-specific rulebook;
        /// scope it to a project's .claude/settings.json rather than global).
        #[arg(long)]
        with_guard: bool,
        /// Also install the UserPromptSubmit recall courier (runs alongside mimir).
        #[arg(long)]
        with_courier: bool,
        /// Also install a SessionStart hook that runs `thor backup --repo <path>`
        /// (daily GitHub backup, debounced 20h). Point it at a clone of the repo.
        #[arg(long)]
        backup_repo: Option<PathBuf>,
    },
    /// Export the whole event log as canonical JSONL for the GitHub backup.
    /// Writes to --out, or stdout if omitted. Each export is a near-pure git
    /// append (the log only grows), so daily commits delta-compress to nothing.
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Restore the store from an exported JSONL log: replays into a FRESH store
    /// (--db must be empty) and verifies every reconstructed hash equals the
    /// recorded one, so a backup that cannot faithfully rebuild the store fails.
    Restore {
        #[arg(long)]
        from: PathBuf,
    },
    /// Automated backup: export + git commit + push into <repo>/thor/ (debounced
    /// to once per 20h). Point --repo at a clone of the memory-backup repo; THOR
    /// only ever touches thor/ and pulls --rebase first, so it never collides
    /// with mimir's root-level backup in the same repo.
    Backup {
        #[arg(long)]
        repo: PathBuf,
        /// Back up even if the last one was under 20h ago.
        #[arg(long)]
        force: bool,
    },
    /// Seed the store from a JSONL snapshot (e.g. exported read-only from mimir).
    /// Idempotent per entity_id.
    Import { path: PathBuf },
    /// Log-shipping receiver: serve the bearer-gated /ship endpoints so a remote
    /// THOR can push its event log into THIS store (the replica). Requires
    /// THOR_TOKEN in the environment - the transport has no other auth.
    Recv {
        /// Bind address, e.g. 0.0.0.0:5555.
        #[arg(long)]
        http: String,
    },
    /// Log-shipping sender: push this store's backlog to a remote receiver
    /// (e.g. --to http://replica:5555). Token from --token or THOR_TOKEN.
    /// With --watch it runs the reconcile tick (re-ship every --interval seconds).
    Ship {
        #[arg(long)]
        to: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long, default_value_t = crate::sync::SHIP_BATCH)]
        batch: usize,
        /// Keep shipping on a timer (the reconcile tick) instead of once.
        #[arg(long)]
        watch: bool,
        /// Reconcile interval in seconds (used with --watch).
        #[arg(long, default_value_t = 60)]
        interval: u64,
    },
    /// Show the sync status: this store's contiguous tip, and (with --to) the
    /// replica's tip + current lag, or that it is unreachable (honest degraded RPO).
    Status {
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Build or refresh the precomputed dense vectors sidecar (thor-vectors.db)
    /// used by the semantic recall layer. ACTION is `build` (full rebuild),
    /// `sync` (embed only new events - index maintenance), or `status`. Requires
    /// a build with `--features semantic`; otherwise it prints a note and exits.
    Vectors {
        /// build | sync | status
        action: String,
        /// Directory holding the five model files (default: %LOCALAPPDATA%\thor\model).
        #[arg(long)]
        model_dir: Option<PathBuf>,
        /// Force a full rebuild even if the stored model id already matches.
        #[arg(long)]
        force: bool,
    },
    /// Warm resident embedder for the recall courier: loads the model once and
    /// serves query embeddings on a localhost port (feature `semantic`). Started
    /// automatically (detached) by the courier when needed; can also be launched
    /// at SessionStart to be warm before the first prompt.
    EmbedDaemon,
    /// Pre-warm the recall embedder (feature `semantic`): if no live daemon is
    /// answering, spawn one detached, then return immediately. Idempotent and
    /// non-blocking - safe to run at SessionStart so the first prompt is already
    /// warm. A no-op on a bm25-only build.
    Warm,
}

/// Render the authoritative answer for one entity: its full head-set. A
/// diverged entity shows EVERY contested head with a DIVERGED marker; it is
/// never collapsed to one arbitrary revision.
pub fn render_get(entity_id: &str, all_events: &[Event]) -> String {
    let heads_map = compute_head_sets(all_events);
    let head_set = match heads_map.get(entity_id) {
        Some(head_set) => head_set,
        None => return format!("Entity {} not found\n", entity_id),
    };
    if head_set.heads.is_empty() {
        return format!("Entity {} has no current heads\n", entity_id);
    }

    let event_by_hash: HashMap<&str, &Event> = all_events
        .iter()
        .map(|event| (event.this_hash.as_str(), event))
        .collect();

    if head_set.is_diverged {
        let mut out = format!(
            "Entity: {}\nStatus: DIVERGED ({} contested heads)\n",
            entity_id,
            head_set.heads.len()
        );
        let mut heads: Vec<&String> = head_set.heads.iter().collect();
        heads.sort();
        for rev in heads {
            match event_by_hash.get(rev.as_str()) {
                Some(event) => out.push_str(&format!(
                    "  head rev {} (seq {}, kind {}):\n    {}\n",
                    rev,
                    event.seq,
                    event.kind.as_str(),
                    event.body
                )),
                None => out.push_str(&format!("  head rev {} (event not found)\n", rev)),
            }
        }
        out.push_str(
            "To pick a winner, run: thor resolve <entity_id> <keep_rev> --discarded <rev> \
             (one --discarded per other head; you must cite the FULL head-set above).\n",
        );
        out
    } else {
        let rev = head_set.heads.iter().next().unwrap();
        match event_by_hash.get(rev.as_str()) {
            Some(event) => format!(
                "Entity: {}\nRev: {}\nBody: {}\nKind: {}\n",
                entity_id,
                rev,
                event.body,
                event.kind.as_str()
            ),
            None => format!("Entity: {}\nRev: {} (event not found)\n", entity_id, rev),
        }
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let db = cli.db.clone().unwrap_or_else(default_db_path);
    // Every subcommand opens this one db; refuse a network path up front.
    refuse_network_db(&db)?;

    match cli.command {
        Commands::Create {
            entity_id,
            session_id,
            lineage_id,
            actor,
            body,
        } => {
            let mut store = EventStore::new(&db)?;
            let event = store.append_event(
                &session_id,
                &lineage_id,
                &actor,
                EventKind::FactCreated,
                &entity_id,
                None,
                &body,
            )?;
            println!("Created entity {} with rev {}", entity_id, event.this_hash);
            println!("Event UUID: {}", event.event_uuid);
        }
        Commands::Revise {
            entity_id,
            parent_rev,
            body,
            session_id,
            lineage_id,
            actor,
        } => {
            let mut store = EventStore::new(&db)?;
            let event = store.append_event(
                &session_id,
                &lineage_id,
                &actor,
                EventKind::FactRevised,
                &entity_id,
                Some(&parent_rev),
                &body,
            )?;
            println!("Revised entity {} with rev {}", entity_id, event.this_hash);
            println!("Event UUID: {}", event.event_uuid);
        }
        Commands::Get { entity_id } => {
            let store = EventStore::new(&db)?;
            let events = store.get_all_events()?;
            print!("{}", render_get(&entity_id, &events));
        }
        Commands::History { entity_id } => {
            let store = EventStore::new(&db)?;
            let events = store.get_events_by_entity(&entity_id)?;
            if events.is_empty() {
                println!("Entity {} has no history", entity_id);
            } else {
                println!("History for entity {}:", entity_id);
                for event in events {
                    println!(
                        "  seq={}, kind={}, rev={}, parent_rev={:?}",
                        event.seq, event.kind.as_str(), event.this_hash, event.parent_rev
                    );
                }
            }
        }
        Commands::Recall { query } => {
            let store = EventStore::new(&db)?;
            let hits = crate::recall::recall(&store, &query, 8)?;
            if hits.is_empty() {
                println!("No recall hits for: {}", query);
            } else {
                for hit in hits {
                    let short = &hit.rev[..hit.rev.len().min(8)];
                    let snip = crate::recall::snippet(&hit.body, 220, &query);
                    let diverged = if hit.is_diverged { " [DIVERGED]" } else { "" };
                    println!("{} ({}{}): {}", hit.entity_id, short, diverged, snip);
                }
            }
        }
        Commands::Resolve {
            entity_id,
            keep_rev,
            discarded,
            session_id,
            lineage_id,
        } => {
            let mut store = EventStore::new(&db)?;
            match store.append_resolve(&session_id, &lineage_id, "cli", &entity_id, &keep_rev, &discarded)
            {
                Ok(event) => {
                    println!("Resolved entity {} keeping rev {}", entity_id, keep_rev);
                    println!("Event UUID: {}", event.event_uuid);
                }
                Err(err) => match err.downcast_ref::<ResolveConflict>() {
                    Some(conflict) => {
                        println!("RESOLVE REJECTED: {}", conflict.reason);
                        println!("Current head-set for {}:", entity_id);
                        for rev in &conflict.current_heads {
                            println!("  {}", rev);
                        }
                        println!(
                            "Nothing was written. Re-run resolve citing exactly this head-set: \
                             keep one rev and pass every other rev via --discarded."
                        );
                    }
                    None => return Err(err),
                },
            }
        }
        Commands::Fsck => {
            let store = EventStore::new(&db)?;
            let events = store.get_all_events()?;

            if let Err(e) = verify_chain_integrity(&events) {
                println!("CHAIN INTEGRITY ERROR: {}", e);
                return Ok(());
            }
            println!("Chain integrity: OK");

            if let Err(e) = detect_fork(&events) {
                println!("FORK DETECTION ERROR: {}", e);
                return Ok(());
            }
            println!("Fork detection: OK");

            if let Err(e) = DifferentialAuditor::verify_consistency(&events) {
                println!("AUDITOR ERROR: {}", e);
                return Ok(());
            }
            println!("Differential auditor: OK");

            if let Err(e) = crate::event_store::verify_fts_projection(store.conn()) {
                println!("FTS PROJECTION ERROR: {}", e);
                return Ok(());
            }
            println!("FTS projection: OK");

            println!("fsck: all checks passed");
        }
        Commands::Courier => {
            // Never propagate: the courier must always let the prompt through.
            crate::courier::run_courier(&db);
        }
        Commands::Mcp { http } => {
            crate::mcp::run_mcp(&db, http);
        }
        Commands::Guard { rulebook } => {
            let path = rulebook.unwrap_or_else(crate::guard::default_rulebook_path);
            crate::guard::run_guard(&path);
        }
        Commands::StopGuard { rulebook } => {
            let path = rulebook.unwrap_or_else(crate::guard::default_response_rulebook_path);
            crate::guard::run_stop_guard(&path);
        }
        Commands::Install { settings, with_guard, with_courier, backup_repo } => {
            let path = settings.unwrap_or_else(crate::install::default_settings_path);
            crate::install::run_install(&path, with_guard, with_courier, backup_repo.as_deref())?;
        }
        Commands::Export { out } => {
            let store = EventStore::new(&db)?;
            match out {
                Some(p) => {
                    let mut f = std::fs::File::create(&p)?;
                    let n = crate::backup::export_jsonl(&store, &mut f)?;
                    eprintln!("exported {n} events to {}", p.display());
                }
                None => {
                    let mut so = std::io::stdout();
                    crate::backup::export_jsonl(&store, &mut so)?;
                }
            }
        }
        Commands::Restore { from } => {
            let mut store = EventStore::new(&db)?;
            let f = std::fs::File::open(&from)?;
            let n = crate::backup::restore_jsonl(&mut store, std::io::BufReader::new(f))?;
            println!("restored {n} events into {} (every replay hash verified)", db.display());
        }
        Commands::Backup { repo, force } => {
            let store = EventStore::new(&db)?;
            println!("{}", crate::backup::backup_to_repo(&store, &repo, force)?);
        }
        Commands::Import { path } => {
            let mut store = EventStore::new(&db)?;
            let stats = crate::importer::import_jsonl(&mut store, &path)?;
            println!(
                "Imported {} facts into {} (skipped {} already present, {} malformed).",
                stats.imported,
                db.display(),
                stats.skipped_existing,
                stats.skipped_malformed
            );
        }
        Commands::Recv { http } => {
            crate::sync::run_recv(&db, &http)?;
        }
        Commands::Ship { to, token, batch, watch, interval } => {
            let token = token
                .or_else(|| std::env::var("THOR_TOKEN").ok())
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("no token: pass --token or set THOR_TOKEN"))?;
            if watch {
                crate::sync::run_reconcile(&db, &to, &token, interval)?;
            } else {
                let store = EventStore::new(&db)?;
                let summary = crate::sync::push_to(&store, &to, &token, batch)?;
                println!(
                    "shipped {} event(s) in {} batch(es); receiver now at contiguous_seq {}",
                    summary.applied, summary.batches, summary.final_cursor
                );
            }
        }
        Commands::Status { to, token } => {
            let token = token
                .or_else(|| std::env::var("THOR_TOKEN").ok())
                .filter(|t| !t.trim().is_empty());
            crate::sync::print_status(&db, to.as_deref(), token.as_deref())?;
        }
        Commands::Vectors { action, model_dir, force } => {
            #[cfg(feature = "semantic")]
            {
                run_vectors(&db, &action, model_dir, force)?;
            }
            #[cfg(not(feature = "semantic"))]
            {
                let _ = (&action, &model_dir, force);
                eprintln!(
                    "thor was built WITHOUT the `semantic` feature: recall is bm25-only and there \
                     is no vectors sidecar to build. Rebuild with `cargo build --release --features semantic`."
                );
            }
        }
        Commands::EmbedDaemon => {
            #[cfg(feature = "semantic")]
            {
                crate::embed_daemon::run_embed_daemon(&db)?;
            }
            #[cfg(not(feature = "semantic"))]
            {
                eprintln!("thor was built WITHOUT the `semantic` feature: there is no embed daemon.");
            }
        }
        Commands::Warm => {
            #[cfg(feature = "semantic")]
            {
                // A live daemon answers a trivial probe in well under the client
                // budget; only spawn (detached) when nothing is up. client_embed
                // self-heals a stale portfile on failure, so the spawn targets a
                // clean slate.
                if crate::embed_daemon::client_embed(&db, "warm").is_none() {
                    crate::embed_daemon::ensure_daemon(&db);
                }
            }
            #[cfg(not(feature = "semantic"))]
            {
                // bm25-only build has no resident embedder; warming is a no-op so
                // the same SessionStart hook is harmless on any build.
            }
        }
    }

    Ok(())
}

/// Build/sync/status the dense vectors sidecar. A model-id mismatch (or --force)
/// triggers a full rebuild; `sync` otherwise embeds only events past the sidecar
/// tip (index maintenance for newly-remembered facts). Fails loudly if the model
/// files are absent, so a half-built sidecar is never silently produced.
#[cfg(feature = "semantic")]
fn run_vectors(db: &Path, action: &str, model_dir: Option<PathBuf>, force: bool) -> Result<()> {
    use crate::embed::{self, Embedder, MODEL_ID};
    use crate::vectors::{default_vectors_path, VectorStore};

    let vpath = default_vectors_path(db);
    let mut vecs = VectorStore::open(&vpath)?;

    match action {
        "status" => {
            println!("vectors sidecar : {}", vpath.display());
            println!("  model_id      : {}", vecs.model_id().unwrap_or_else(|| "(none)".into()));
            println!("  expected      : {}", MODEL_ID);
            println!("  stored vectors: {}", vecs.count()?);
            println!("  tip seq       : {}", vecs.max_seq()?);
            return Ok(());
        }
        "build" | "sync" => {}
        other => anyhow::bail!("unknown vectors action '{}': use build | sync | status", other),
    }

    let model_dir = model_dir.unwrap_or_else(embed::default_model_dir);
    if !embed::model_present(&model_dir) {
        anyhow::bail!(
            "model files not found in {} (need: {}). Put THOR's own copy there, or pass --model-dir.",
            model_dir.display(),
            embed::MODEL_FILES.join(", ")
        );
    }

    // A model mismatch (or --force, or an explicit `build`) means any stored
    // vectors are stale: rebuild from scratch. Otherwise `sync` embeds only the
    // events past the sidecar tip.
    let stored = vecs.model_id();
    let mismatch = stored.as_deref() != Some(MODEL_ID);
    let full = force || action == "build" || mismatch;
    if mismatch && action == "sync" && stored.is_some() {
        eprintln!(
            "model id changed ({} -> {}): doing a full rebuild instead of sync.",
            stored.as_deref().unwrap_or("(none)"),
            MODEL_ID
        );
    }

    let store = EventStore::new(db)?;
    let events = store.get_all_events()?;

    if full {
        vecs.clear()?;
        vecs.set_model_id(MODEL_ID)?;
    }
    let tip = if full { 0 } else { vecs.max_seq()? };

    let todo: Vec<&Event> = events.iter().filter(|e| e.seq > tip).collect();
    if todo.is_empty() {
        println!("vectors up to date ({} stored, tip seq {}).", vecs.count()?, vecs.max_seq()?);
        return Ok(());
    }
    println!("embedding {} event(s) with {} ...", todo.len(), MODEL_ID);
    let mut embedder = Embedder::load(&model_dir)?;

    const BATCH: usize = 256;
    let mut done = 0usize;
    for chunk in todo.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|e| e.body.clone()).collect();
        let vs = embedder.embed_many(&texts)?;
        let rows: Vec<(i64, Vec<f32>)> = chunk.iter().map(|e| e.seq).zip(vs).collect();
        vecs.upsert_batch(&rows)?;
        done += chunk.len();
        println!("  {}/{}", done, todo.len());
    }
    println!("done: {} vectors, tip seq {}.", vecs.count()?, vecs.max_seq()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_get_single_head() {
        let mut store = EventStore::in_memory().unwrap();
        let rev_a = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body A")
            .unwrap()
            .this_hash;
        let rev_b = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(&rev_a), "body B",
            )
            .unwrap()
            .this_hash;

        let events = store.get_all_events().unwrap();
        let out = render_get("e1", &events);
        assert!(out.contains(&rev_b), "the single head is surfaced");
        assert!(out.contains("body B"));
        assert!(!out.contains(&rev_a), "a replaced rev is not surfaced");
        assert!(!out.contains("DIVERGED"));
    }

    #[test]
    fn test_render_get_surfaces_all_heads_when_diverged() {
        let mut store = EventStore::in_memory().unwrap();
        let rev_a = store
            .append_event("s1", "l1", "a1", EventKind::FactCreated, "e1", None, "body A")
            .unwrap()
            .this_hash;
        let rev_b = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some(&rev_a), "body B",
            )
            .unwrap()
            .this_hash;
        // a revise citing a stale parent: real divergence
        let rev_c = store
            .append_event(
                "s1", "l1", "a1", EventKind::FactRevised, "e1", Some("stale-parent"), "body C",
            )
            .unwrap()
            .this_hash;

        let events = store.get_all_events().unwrap();
        let out = render_get("e1", &events);
        assert!(out.contains("DIVERGED"), "contested marker is surfaced");
        assert!(out.contains(&rev_b), "first head is surfaced");
        assert!(out.contains(&rev_c), "second head is surfaced");
        assert!(out.contains("body B"));
        assert!(out.contains("body C"));
    }

    #[test]
    fn test_render_get_unknown_entity() {
        let out = render_get("missing", &[]);
        assert!(out.contains("not found"));
    }

    #[cfg(windows)]
    #[test]
    fn test_refuses_unc_but_allows_local_paths() {
        assert!(is_network_path(Path::new(r"\\server\share\thor.db")), "a UNC/SMB path must be flagged");
        assert!(is_network_path(Path::new(r"\\?\UNC\server\share\thor.db")), "a verbatim UNC path must be flagged");
        assert!(!is_network_path(Path::new(r"C:\Users\me\AppData\Local\thor\thor.db")), "a local disk path is fine");
        assert!(!is_network_path(Path::new(r"\\?\C:\local\verbatim\thor.db")), "a verbatim LOCAL path is fine");
        assert!(!is_network_path(Path::new("thor.db")), "a relative local path is fine");
        assert!(refuse_network_db(Path::new(r"\\server\share\thor.db")).is_err(), "the guard must reject a UNC db");
        assert!(refuse_network_db(Path::new(r"C:\local\thor.db")).is_ok(), "the guard must allow a local db");
    }
}
