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
/// agree on one location without a flag (see ledger::data_dir for the platform
/// resolution). `None` when no per-user location resolves - callers error out
/// instead of falling back to a cwd-relative file, which would plant store
/// files inside the user's repo and open a repo-shipped thor.db.
fn default_db_path() -> Option<PathBuf> {
    let dir = crate::ledger::data_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("thor.db"))
}

/// Summary of a `thor drain-inbox` run, returned so the caller (and tests) can
/// assert what happened without parsing stdout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainSummary {
    pub total: usize,
    pub applied: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Drain a replica's capture inbox into the authority store at `db`: replay each
/// captured remember/revise/retract as a real event. The footer is already baked
/// into each op's body (`remember` composed it before the divert), so this never
/// re-derives footer state - it re-runs the same dedup + append the live
/// `remember` would have, preserving the mobile-assigned entity id so revisions
/// chain correctly. Idempotent on create: a create whose entity/dup already exists
/// is skipped, so re-draining the same file is safe.
pub fn run_drain_inbox(db: &Path, inbox: &Path) -> anyhow::Result<DrainSummary> {
    let ops = crate::inbox::read_all(inbox)?;
    let mut store = EventStore::new(db)?;
    Ok(apply_ops(&mut store, &ops))
}

/// Pull a replica's captures over HTTP, apply them, and ack on a clean drain.
pub fn run_drain_http(db: &Path, base: &str, token: &str) -> anyhow::Result<DrainSummary> {
    let ops = crate::sync::pull_inbox(base, token)?;
    let mut store = EventStore::new(db)?;
    let s = apply_ops(&mut store, &ops);
    // Ack only a clean, non-empty drain: a failed apply leaves the batch on the
    // replica to retry (at-least-once). An empty pull created no draining file.
    if s.total > 0 && s.errors == 0 {
        crate::sync::ack_inbox(base, token)?;
    }
    Ok(s)
}

/// Apply captured inbox ops to an already-open authority store. Shared by the
/// file drain and the HTTP pull drain.
pub fn apply_ops(store: &mut EventStore, ops: &[crate::inbox::InboxOp]) -> DrainSummary {
    let mut s = DrainSummary { total: ops.len(), ..Default::default() };
    for op in ops {
        match op.op.as_str() {
            "create" => {
                // Reconstruct the exact dedup the live remember used: the footer is
                // already in op.body, so dedup_prefix strips it the same way, scoped
                // to where the fact lives.
                let scope = crate::recall::RecallScope::current(
                    crate::repo::owner_project(&op.entity_id).map(str::to_string),
                );
                let prefix = crate::recall::dedup_prefix(&op.body);
                match store.append_created_unique(
                    "drain-inbox",
                    "drain-inbox",
                    "mcp",
                    &op.entity_id,
                    &op.body,
                    |_, project, head_body| {
                        scope.allows(project) && crate::recall::dedup_prefix(head_body) == prefix
                    },
                ) {
                    Ok(ev) => {
                        s.applied += 1;
                        println!("created {} rev {}", op.entity_id, &ev.this_hash[..8.min(ev.this_hash.len())]);
                    }
                    Err(e) => {
                        // A create conflict = the fact is already on the authority
                        // (dup or same id): idempotent skip so a re-drain is safe.
                        if e.downcast_ref::<crate::event_store::MutateConflict>().is_some() {
                            s.skipped += 1;
                            println!("skip (already present): {}", op.entity_id);
                        } else {
                            s.errors += 1;
                            eprintln!("ERROR create {}: {e}", op.entity_id);
                        }
                    }
                }
            }
            "revise" | "retract" => {
                let kind = if op.op == "revise" {
                    EventKind::FactRevised
                } else {
                    EventKind::FactRetracted
                };
                match store.append_mutate_checked(
                    "drain-inbox",
                    "drain-inbox",
                    "mcp",
                    kind,
                    &op.entity_id,
                    op.parent_rev.as_deref(),
                    &op.body,
                ) {
                    Ok(ev) => {
                        s.applied += 1;
                        println!("{} {} rev {}", op.op, op.entity_id, &ev.this_hash[..8.min(ev.this_hash.len())]);
                    }
                    Err(e) => {
                        s.errors += 1;
                        eprintln!("ERROR {} {}: {e}", op.op, op.entity_id);
                    }
                }
            }
            other => {
                s.errors += 1;
                eprintln!("ERROR unknown inbox op '{other}' for {}", op.entity_id);
            }
        }
    }
    s
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
        /// Search every project, not just the current one + the global tier.
        #[arg(long)]
        all_projects: bool,
        /// Scope to a specific project key (default: the current directory's project).
        #[arg(long)]
        project: Option<String>,
        /// Rescore the top hits with the local cross-encoder (semantic build +
        /// downloaded reranker model; slower, better paraphrase ordering).
        /// Keeps the normal order when the model is unavailable.
        #[arg(long)]
        rerank: bool,
    },
    /// Ingest a folder's text files into the store as recall chunks, incrementally
    /// (new -> create, changed -> revise, deleted -> retract). No path = the current
    /// directory. A GIT repo reads tracked files only (`git ls-files`), so gitignored
    /// secrets are never ingested; a plain NON-git folder is walked directly (dotfiles
    /// and heavy dirs skipped - use it for loose docs folders, like mimir's non-git doc
    /// collections). Chunk ids are `<project>:<rel>#<n>`, which is how recall keeps one
    /// project from bleeding into another.
    Ingest {
        /// Folder path(s) to ingest (default: the current directory).
        paths: Vec<PathBuf>,
        /// Spawn detached and return at once (non-blocking; for SessionStart).
        #[arg(long)]
        detach: bool,
        /// Ingest as GLOBAL cross-cutting knowledge (the `@global` tier, available in
        /// every project) instead of scoping to this repo's own project.
        #[arg(long)]
        global: bool,
        /// Force the PROJECT KEY for every ingested chunk instead of deriving it from
        /// the folder (a `.thor` marker, else the basename). Pins a stable key when the
        /// folder name differs from how you open the project (e.g. a backup/source folder
        /// whose basename is not the project's key). Conflicts with --global.
        #[arg(long, conflicts_with = "global")]
        project: Option<String>,
    },
    /// Set up THOR for a project: write a `.thor` marker (a stable project key) and
    /// ingest its tracked files. Makes the project "known" so SessionStart refreshes
    /// it silently instead of prompting.
    Init {
        /// Project path (default: the current directory).
        path: Option<PathBuf>,
        /// Project key to write (default: the repo-root basename).
        #[arg(long)]
        key: Option<String>,
    },
    /// Reassign a fact's PROJECT scope (appends a fact_reprojected event; sync-safe).
    Reproject {
        /// The entity id to reproject (omit with --batch to read ids from stdin).
        entity_id: Option<String>,
        /// Reassign to this project key.
        #[arg(long)]
        project: Option<String>,
        /// Make the fact global (cross-project). Mutually exclusive with --project.
        #[arg(long)]
        global: bool,
        /// Read newline-separated entity ids from stdin instead of the argument.
        #[arg(long)]
        batch: bool,
        /// Allow reprojecting a chunk-shaped id (normally managed by ingest).
        #[arg(long)]
        force: bool,
    },
    /// Backfill project scope for legacy unprefixed memories from their mimir import
    /// footer (`... | project: <name> | ...`). Dry-run unless --apply.
    BackfillProjects {
        /// Actually append the reproject events (default: dry-run preview only).
        #[arg(long)]
        apply: bool,
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
    Fsck {
        /// Rebuild the full-text index from the log. The repair for a damaged
        /// index (the "FTS integrity" check below); nothing is lost, the index
        /// is derived from the log. Deliberately manual: the automatic heal
        /// cannot see this damage, and rewriting an index while the disk under
        /// it is suspect is the wrong reflex.
        #[arg(long)]
        rebuild_fts: bool,
    },
    /// Per-hook recall courier: reads the UserPromptSubmit hook JSON on stdin
    /// and prints a THOR recall block to stdout. Hard fail-open, always exit 0.
    Courier,
    /// Run as an MCP stdio server (newline-delimited JSON-RPC on stdin/stdout),
    /// exposing the full stewardship toolset (recall/get/history/remember/revise/
    /// retract/resolve/mark/pin/unpin/reproject/brief). Register with:
    /// claude mcp add thor -- <thor.exe> mcp
    Mcp {
        /// Serve Streamable-HTTP on <bind> (e.g. 127.0.0.1:8078) for the remote
        /// NAS connector, instead of local stdio. Bind to localhost and front it
        /// with an auth gate (Cloudflare Access), exactly like mimir's remote MCP.
        #[arg(long)]
        http: Option<String>,
    },
    /// Run the warm per-prompt injection daemon: the same HTTP server as
    /// `mcp --http`, on a zero-config loopback bind. While it runs, the
    /// courier hook answers from the warm store in single-digit ms instead of
    /// paying a cold process start; without it nothing changes (fail-open).
    Daemon {
        /// Bind address (loopback only - /inject carries prompt text, no auth).
        #[arg(long, default_value_t = crate::mcp::DEFAULT_DAEMON_BIND.to_string())]
        bind: String,
    },
    /// SessionStart-safe warm start: when the daemon's /health does not
    /// answer, spawn `thor daemon` detached (debounced) and return at once.
    EnsureDaemon,
    /// Read-only health check across THOR's surfaces: store, semantic
    /// model/sidecar, injection daemon warm/cold, and any flags present.
    Doctor,
    /// (Re)build the derived symbol sidecar (thor-symbols.db): which names
    /// each code chunk defines and calls. Powers where_used/impact and the
    /// deliberate-recall symbol bonus. Safe to delete and rebuild any time.
    Symbols,
    /// PreCompact hook: one advisory per session, just before a compaction,
    /// nudging the agent to persist durable decisions via remember. Installed
    /// by `thor install --with-courier`. Always exits 0.
    PreCompact,
    /// Prepare a stewardship review: write the consolidate report plus the
    /// proven conservative review rubric to a file an agent session can work
    /// through with the MCP tools (revise/retract/pin). Makes NO store writes
    /// itself - every action stays an explicit, auditable event.
    Steward,
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
        /// Also install a SessionStart hook that ensures the warm injection
        /// daemon is running (`thor ensure-daemon`). Without it the courier
        /// simply uses its cold path; the daemon only makes prompts faster.
        #[arg(long)]
        with_daemon: bool,
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
        /// Upper bound on events per HTTP shipment (a byte budget caps it too).
        /// Honoured by both the one-shot form and --watch.
        #[arg(long, default_value_t = crate::sync::SHIP_BATCH)]
        batch: usize,
        /// Keep shipping on a timer (the reconcile tick) instead of once.
        #[arg(long)]
        watch: bool,
        /// Reconcile interval in seconds (used with --watch).
        #[arg(long, default_value_t = 60)]
        interval: u64,
    },
    /// Drain a replica's capture inbox into THIS (authority) store: replay each
    /// captured remember/revise/retract as a real event in this log. Run from the
    /// PC's ship job. Pull over HTTP with --from, or apply a local file with --inbox.
    DrainInbox {
        /// Apply a local inbox jsonl file (a rotated copy fetched out of band).
        #[arg(long)]
        inbox: Option<PathBuf>,
        /// Pull over HTTP from a replica's bearer-gated /inbox endpoint
        /// (e.g. --from http://replica:5555). Token from --token or THOR_TOKEN.
        #[arg(long)]
        from: Option<String>,
        /// Bearer token for --from.
        #[arg(long)]
        token: Option<String>,
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
    /// SessionStart helper: refresh a KNOWN project (has a `.thor` marker) in the
    /// background, or emit a `<thor-setup>` cue for an un-onboarded git project so
    /// the agent offers to set it up. Prints nothing for a scratch dir. Run as a
    /// SessionStart hook.
    SessionStart,
    /// List GLOBAL memories with no project signal added since the last review, so
    /// the agent can propose reprojecting the project-specific ones. `--mark` records
    /// that the current tip has been reviewed (advances the watermark).
    ReviewScope {
        #[arg(long)]
        mark: bool,
    },
    /// Mark a fact as USEFUL (appends a head-neutral fact_echoed event). Feeds the
    /// courier's promotion prior, so a fact that actually helped wins close ranking
    /// calls. With --noise: the fact was injected but only distracted - a LOCAL
    /// counter (never synced) that demotes its promotion and feeds decay.
    Mark {
        entity_id: String,
        /// Mark as noise instead of useful (local ledger, not the synced log).
        #[arg(long)]
        noise: bool,
    },
    /// Pin a fact: `thor session-start` then re-injects its full body at every
    /// session start (and right after a compaction) via a <thor-brief> block - the
    /// memory version of CLAUDE.md, per project, without editing any file. The
    /// whole pin list is one row in the local ledger sidecar (thor-ledger.db,
    /// next to the store), never part of the synced log.
    Pin {
        /// The entity id to pin (omit with --list).
        entity_id: Option<String>,
        /// List the current pins.
        #[arg(long)]
        list: bool,
    },
    /// Remove a pinned fact from the session-start brief.
    Unpin { entity_id: String },
    /// Metabolism report: duplicate twins (same normalized prefix the
    /// remember/import gates refuse on), decay candidates (untyped, never
    /// marked, never read, long inactive) and same-topic clusters for agent
    /// review. Report-only by default; exits 1 when anything is found (CI
    /// gate), 0 when clean. Lossless: nothing is ever deleted.
    Consolidate {
        /// Retract the duplicate twins from the report (keeps the pinned /
        /// import-synced / oldest copy). Decay and cluster candidates are
        /// NEVER applied mechanically - confirm those one by one via
        /// retract/revise/supersede.
        #[arg(long)]
        apply_dedup: bool,
        /// Decay age floor in EVENTS behind the log tip (the hash-chained log
        /// carries no wall clock, so age is measured in activity).
        #[arg(long, default_value_t = crate::consolidate::DEFAULT_MIN_AGE_EVENTS)]
        min_age_events: i64,
    },
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
            Some(event) => {
                // Freshness for a current-project chunk read deliberately: warn
                // when the stored snapshot no longer matches the file on disk
                // (get shows the STORED body - the log is the record - but the
                // agent must know it is looking at yesterday's code).
                let cwd = std::env::current_dir().ok().map(|c| c.display().to_string());
                let project = cwd.as_deref().and_then(|c| crate::repo::project_key(Path::new(c)));
                let fresh_note = match crate::courier::freshness(
                    entity_id, &event.body, project.as_deref(), cwd.as_deref(),
                ) {
                    crate::courier::Freshness::Refreshed(_) => {
                        "Freshness: [refreshed] the file changed since ingest - this stored chunk is outdated (re-ingest or read the file)\n"
                    }
                    crate::courier::Freshness::Stale => {
                        "Freshness: [stale?] the file or chunk no longer exists on disk\n"
                    }
                    crate::courier::Freshness::Current => "",
                };
                format!(
                    "Entity: {}\nRev: {}\nBody: {}\nKind: {}\n{}",
                    entity_id,
                    rev,
                    event.body,
                    event.kind.as_str(),
                    fresh_note
                )
            }
            None => format!("Entity: {}\nRev: {} (event not found)\n", entity_id, rev),
        }
    }
}

/// Render one entity's full revision history (shared by the CLI and the MCP
/// history tool). `events` are the entity's own events, in seq order.
pub fn render_history(entity_id: &str, events: &[Event]) -> String {
    if events.is_empty() {
        return format!("Entity {} has no history\n", entity_id);
    }
    let mut out = format!("History for entity {}:\n", entity_id);
    for event in events {
        out.push_str(&format!(
            "  seq={}, kind={}, rev={}, parent_rev={:?}\n",
            event.seq,
            event.kind.as_str(),
            event.this_hash,
            event.parent_rev
        ));
    }
    out
}

/// Render the pinned-facts brief: the guaranteed re-orientation block for a
/// session start - especially right after a compaction, when the context is
/// empty and prompt-recall has nothing to match against ("ga verder"). Full
/// bodies (not 220-char snippets), scope-filtered to the current project + the
/// global tier, every contested head shown. None when nothing is pinned/in scope.
pub fn render_brief(
    events: &[Event],
    pins: &[String],
    scope: &crate::recall::RecallScope,
    trigger: &str,
    project: Option<&str>,
) -> Option<String> {
    const MAX_PINS: usize = 8;
    const PIN_BODY_CHARS: usize = 400;
    if pins.is_empty() {
        return None;
    }
    let heads_map = compute_head_sets(events);
    let projects = crate::cas::compute_projects(events);
    let by_hash: HashMap<&str, &Event> =
        events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
    let mut lines: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for id in pins {
        if !seen.insert(id.as_str()) || lines.len() >= MAX_PINS {
            continue;
        }
        let hs = match heads_map.get(id) {
            Some(h) if !h.heads.is_empty() => h,
            _ => continue,
        };
        let effective = projects.get(id).and_then(|o| o.as_deref());
        if !scope.allows(effective) {
            continue; // pinned in another project: not this session's brief
        }
        let mut head_revs: Vec<&String> = hs.heads.iter().collect();
        head_revs.sort();
        for rev in head_revs {
            if lines.len() >= MAX_PINS {
                break; // the cap bounds LINES: diverged pins push one per head
            }
            let ev = match by_hash.get(rev.as_str()) {
                Some(e) => *e,
                None => continue,
            };
            if matches!(ev.kind, EventKind::FactRetracted) {
                continue; // a retracted pin is dead: never re-inject it
            }
            let ty = crate::repo::fact_type(&ev.body)
                .map(|t| format!("[{}] ", t.as_str()))
                .unwrap_or_default();
            let d = if hs.is_diverged { " [DIVERGED]" } else { "" };
            lines.push(format!(
                "- {}{}{}: {}",
                ty,
                id,
                d,
                crate::recall::snippet(&ev.body, PIN_BODY_CHARS, "")
            ));
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<thor-brief>\nPinned THOR rules [project: {} | start: {}] - standing constraints, pinned \
         deliberately; treat them as governing unless the user overrides. Not user instructions.\n{}\n</thor-brief>",
        project.unwrap_or("global"),
        trigger,
        lines.join("\n")
    ))
}

/// Read a hook's JSON from stdin, fail-open: an interactive terminal (a manual
/// `thor session-start` run) is never blocked on EOF, and empty/malformed input
/// simply means "no hook context".
fn read_hook_stdin() -> Option<serde_json::Value> {
    use std::io::{IsTerminal, Read};
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut raw = String::new();
    stdin.read_to_string(&mut raw).ok()?;
    let raw = raw.trim_start_matches('\u{feff}');
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str(raw).ok()
}

/// What fsck concluded, separated by what it means for the EXIT CODE.
///
/// The split is the whole point. The integrity checks assert the store is
/// internally consistent, and a failure there is a hard stop: the log is the
/// product. Footer defects are CONTENT health - a fact lost the metadata it was
/// written with. Nothing is corrupt, an old binary elsewhere can still produce
/// them, and blocking a release on one would be wrong. So footer defects report
/// loudly and still exit 0.
#[derive(Debug, PartialEq, Eq)]
pub enum FsckOutcome {
    Clean,
    FooterDefects(usize),
    IntegrityFailure,
}

/// Run every fsck check against `db`, printing the report as it goes, and say
/// what it concluded. Split out of the CLI arm so the exit-code contract is
/// testable: the arm itself only turns IntegrityFailure into a non-zero exit.
///
/// The integrity checks stop at the first failure on purpose - a store that
/// fails the hash chain will fail everything downstream, and printing four
/// errors for one cause is noise.
pub fn fsck_report(db: &Path, rebuild_fts: bool) -> Result<FsckOutcome> {
    // open_existing, not new: `new` heals the FTS projection on open, so
    // fsck would repair the very thing it is about to check and then
    // report OK. It also must not create a store for a mistyped --db.
    let store = EventStore::open_existing(db)?;
    let events = store.get_all_events()?;

    if let Err(e) = verify_chain_integrity(&events) {
        println!("CHAIN INTEGRITY ERROR: {}", e);
        return Ok(FsckOutcome::IntegrityFailure);
    }
    println!("Chain integrity: OK");

    if let Err(e) = detect_fork(&events) {
        println!("FORK DETECTION ERROR: {}", e);
        return Ok(FsckOutcome::IntegrityFailure);
    }
    println!("Fork detection: OK");

    if let Err(e) = DifferentialAuditor::verify_consistency(&events) {
        println!("AUDITOR ERROR: {}", e);
        return Ok(FsckOutcome::IntegrityFailure);
    }
    println!("Differential auditor: OK");

    if let Err(e) = crate::event_store::verify_fts_projection(store.conn()) {
        println!("FTS PROJECTION ERROR: {}", e);
        return Ok(FsckOutcome::IntegrityFailure);
    }
    println!("FTS projection: OK");

    // The row-set check above cannot see a damaged index block: the counts and
    // the rowid set stay perfect while MATCH silently returns fewer rows.
    if let Err(e) = crate::event_store::verify_fts_integrity(store.conn()) {
        println!("FTS INTEGRITY ERROR: {}", e);
        if rebuild_fts {
            match crate::event_store::rebuild_fts(store.conn()) {
                Ok(n) => {
                    println!("FTS index rebuilt from the log ({} rows). Re-run fsck to confirm.", n);
                    return Ok(FsckOutcome::Clean);
                }
                Err(e) => println!("FTS rebuild failed: {}", e),
            }
        } else {
            println!("Repair with: thor fsck --rebuild-fts (the index is derived; nothing is lost)");
        }
        return Ok(FsckOutcome::IntegrityFailure);
    }
    println!("FTS integrity: OK");

    // Footer integrity is CONTENT health, not log integrity: the checks
    // above assert the store is internally consistent, this one asserts
    // that live facts still carry the metadata they were written with.
    // A wiped footer can therefore never fail fsck - nothing is corrupt,
    // and an old binary elsewhere writing one must not block a release.
    let defects = crate::footer::defects(&events);
    if defects.is_empty() {
        println!("Footer integrity: OK");
        println!("fsck: all checks passed");
        return Ok(FsckOutcome::Clean);
    }
    print_footer_defects(&defects);
    println!(
        "fsck: integrity checks passed; {} fact(s) need a footer repair (see above)",
        defects.len()
    );
    Ok(FsckOutcome::FooterDefects(defects.len()))
}

/// The fsck report for damaged footers. Written for someone who has never met
/// the footer before: what broke, why it matters, and the exact way out - a
/// bare "2 defects" would leave the reader with a store they cannot repair.
fn print_footer_defects(defects: &[crate::footer::Defect]) {
    use crate::footer::Defect;

    let wiped = defects.iter().filter(|d| matches!(d, Defect::Wiped { .. })).count();
    let malformed = defects.len() - wiped;
    println!(
        "Footer integrity: {} DEGRADED fact(s) - {} wiped, {} malformed (the log is intact: \
         this is content, not corruption)",
        defects.len(),
        wiped,
        malformed
    );
    println!(
        "  A fact's footer is the last line of its body ([memory/<type> | tags: ... | \
         fires-when: ... | anchors: ... | project: ...]). It carries the type, the tags, the \
         fires-when boost and the Guard's anchors. Without it a fact stays findable through \
         recall, but it never fires at the moment of action again - which is usually why it was \
         written."
    );
    for d in defects {
        match d {
            Defect::Wiped { entity_id, rev, from_rev, footer } => {
                println!("\n  WIPED     {} (head {})", entity_id, &rev[..12.min(rev.len())]);
                println!("    its footer, last seen at rev {}:", &from_rev[..12.min(from_rev.len())]);
                println!("    {}", footer);
                println!("    Repair - re-attach that footer to the CURRENT body:");
                println!("      1. thor get {}", entity_id);
                println!("      2. copy the body EXACTLY as it stands (do not rewrite it)");
                println!("      3. thor revise {} {} \"<body>", entity_id, rev);
                println!("         <blank line>");
                println!("         {}\"", footer);
                println!(
                    "    (Only facts damaged by a pre-0.9.2 binary land here: a revise now \
                     carries the footer across by itself.)"
                );
            }
            Defect::Malformed { entity_id, rev, reason } => {
                println!("\n  MALFORMED {} (head {})", entity_id, &rev[..12.min(rev.len())]);
                println!("    {}", reason);
                println!("    Repair: thor get {} , fix the footer line, then thor revise {} {} \"<body>\"",
                    entity_id, entity_id, rev);
            }
        }
    }
    println!(
        "\n  A DIVERGED fact (two heads) must be resolved first - `thor resolve` - before a \
         repair can land on it."
    );
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let db = match cli.db.clone().or_else(default_db_path) {
        Some(db) => db,
        None => anyhow::bail!(
            "no THOR store location: LOCALAPPDATA, XDG_DATA_HOME and HOME are all unset. \
             Pass --db <path> explicitly."
        ),
    };
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
            // Same path as the MCP tool: CAS-checked (a stale parent is refused
            // instead of silently branching) and it carries the fact's footer
            // across a content-only edit, so a body written without one does not
            // strip the type, tags, fires-when and the guard's anchors.
            let event = store.append_mutate_checked(
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
            print!("{}", render_history(&entity_id, &events));
        }
        Commands::Recall { query, all_projects, project, rerank } => {
            let store = EventStore::new(&db)?;
            // Scope: --all-projects = everything; --project <key> = that project +
            // global; default = the current directory's project + global.
            let scope = if all_projects {
                crate::recall::RecallScope::everything()
            } else if project.is_some() {
                crate::recall::RecallScope::current(project)
            } else {
                crate::recall::RecallScope::current(
                    std::env::current_dir().ok().and_then(|c| crate::repo::project_key(&c)),
                )
            };
            let limit = 8;
            // Rerank rescoring only `limit` hits could never rescue a gold
            // buried just below it: fetch the rescore pool, reorder, cut back.
            #[cfg(feature = "semantic")]
            let fetch = if rerank { limit.max(crate::rerank::RERANK_TOP_N) } else { limit };
            #[cfg(not(feature = "semantic"))]
            let fetch = limit;
            #[allow(unused_mut)]
            let mut hits = crate::recall::recall_scoped(&store, &query, fetch, &scope)?;
            if rerank {
                #[cfg(feature = "semantic")]
                {
                    let (reordered, applied) = crate::rerank::rerank_hits(&query, hits);
                    hits = reordered;
                    if !applied {
                        println!("(rerank skipped: reranker model unavailable or nothing to reorder)");
                    }
                }
                #[cfg(not(feature = "semantic"))]
                println!("(rerank unavailable: non-semantic build)");
            }
            hits.truncate(limit);
            if hits.is_empty() {
                println!("No recall hits for: {}", query);
            } else {
                // Freshness context: the CLI runs in the project dir, so a
                // current-project chunk is re-read live ([refreshed]/[stale?]).
                let cwd = std::env::current_dir().ok().map(|c| c.display().to_string());
                let fresh_project =
                    cwd.as_deref().and_then(|c| crate::repo::project_key(Path::new(c)));
                for hit in hits {
                    let short = &hit.rev[..hit.rev.len().min(8)];
                    let (fresh_tag, snip) = crate::courier::serve_deliberate(
                        &store, &hit.entity_id, &hit.body, &query, fresh_project.as_deref(), cwd.as_deref(),
                    );
                    let diverged = if hit.is_diverged { " [DIVERGED]" } else { "" };
                    let tag = if crate::repo::is_global(hit.project.as_deref()) {
                        "[global]".to_string()
                    } else {
                        format!("[proj:{}]", hit.project.as_deref().unwrap_or("?"))
                    };
                    println!("{} {} ({}{}{}): {}", tag, hit.entity_id, short, diverged, fresh_tag, snip);
                }
            }
        }
        Commands::Ingest { paths, detach, global, project } => {
            let paths =
                if paths.is_empty() { vec![std::env::current_dir()?] } else { paths };
            // Validate a user-supplied key exactly like init/reproject, so a ':' or
            // '#' can never mint a mis-scoped chunk id.
            if let Some(k) = project.as_deref() {
                crate::repo::validate_project_key(k).map_err(|e| anyhow::anyhow!(e))?;
            }
            // --global wins over --project (clap already rejects both together); an
            // explicit --project pins a canonical key, else derive per folder.
            let override_key: Option<String> =
                if global { Some(crate::repo::GLOBAL_KEY.to_string()) } else { project };
            if detach {
                spawn_detached_ingest(&db, &paths, override_key.as_deref())?;
            } else {
                run_ingest(&db, &paths, override_key.as_deref())?;
            }
        }
        Commands::Init { path, key } => {
            let path = path.map(Ok).unwrap_or_else(std::env::current_dir)?;
            run_init(&db, &path, key)?;
        }
        Commands::Reproject { entity_id, project, global, batch, force } => {
            run_reproject(&db, entity_id, project, global, batch, force)?;
        }
        Commands::BackfillProjects { apply } => {
            run_backfill_projects(&db, apply)?;
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
        Commands::Fsck { rebuild_fts } => {
            // A detected integrity failure MUST leave a non-zero exit code, or
            // fsck cannot gate anything: a scheduled job, a release step or a CI
            // run would all read "broken hash chain" as success. `thor
            // consolidate` already sets this precedent below.
            match fsck_report(&db, rebuild_fts)? {
                FsckOutcome::IntegrityFailure => std::process::exit(1),
                FsckOutcome::Clean | FsckOutcome::FooterDefects(_) => {}
            }
        }
        Commands::Courier => {
            // Never propagate: the courier must always let the prompt through.
            crate::courier::run_courier(&db);
        }
        Commands::Mcp { http } => {
            crate::mcp::run_mcp(&db, http);
        }
        Commands::Daemon { bind } => {
            // Discoverable alias for the warm injection daemon: the same
            // HTTP server as `thor mcp --http`, with a zero-config loopback
            // default. Publishes THOR-DAEMON.flag for courier/doctor.
            crate::mcp::run_mcp(&db, Some(bind));
        }
        Commands::EnsureDaemon => {
            crate::daemon_client::ensure_daemon(&db);
        }
        Commands::Doctor => {
            crate::doctor::print_doctor(&db);
        }
        Commands::Symbols => {
            let store = EventStore::new(&db)?;
            let mut sy = crate::symbols::SymbolStore::open_default(&db)
                .map_err(|e| anyhow::anyhow!("symbols sidecar: {e}"))?;
            let stats = sy.rebuild(&store)?;
            println!(
                "symbols rebuilt: {} source chunks -> {} definitions, {} call edges ({})",
                stats.chunks, stats.symbols, stats.edges,
                crate::symbols::default_symbols_path(&db).display()
            );
        }
        Commands::PreCompact => {
            crate::courier::run_pre_compact(&db);
        }
        Commands::Steward => {
            let store = EventStore::new(&db)?;
            let events = store.get_all_events()?;
            let tip = events.last().map(|e| e.seq).unwrap_or(0);
            let report = crate::consolidate::build_report(
                &store,
                &db,
                &events,
                &crate::consolidate::Options { min_age_events: 2000 },
            );
            let dir = db.parent().unwrap_or_else(|| std::path::Path::new(".")).join("steward");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("steward-{tip}.md"));
            let mut out = String::new();
            out.push_str(&format!(
                "# THOR stewardship review (store tip seq {tip})

                 The proven conservative rubric (V5 decay review: 280 candidates -> 216 keep,
                 61 retype, 1 retract; zero damage). Work through the report below in an agent
                 session armed with the THOR MCP tools. Every action is an ordinary event in
                 the hash-chained log - nothing here is mechanical or irreversible.

                 ## Rubric (per candidate, in order)

                 1. KEEP (default, no action): anything with plausible future reuse value.
                    Reference data, formulas, business facts, hardware knowledge, preferences,
                    gotchas, design rationale: ALWAYS keep. Doubt = keep.
                 2. RETYPE (revise): durable reusable knowledge without a typed footer - add
                    [memory/<gotcha|decision|preference> | tags | fires-when (6-10 single words,
                    bilingual NL+EN task vocabulary, body-derived) | project] with a blank line
                    before it; body text stays byte-identical. REPLACE an existing footer, never
                    stack a second one (the double-footer incident is documented).
                 3. RETRACT: ONLY an evidently dead session/status log whose outcome is fully
                    covered by current state and which contains zero unique durable knowledge.
                 4. Duplicate twins: `thor consolidate --apply-dedup` handles those separately.
                 5. Clusters are leads, not verdicts: resolve contradictions via revise/resolve.

                 Run this review roughly every ~2000 store events, or after a heavy write burst.

                 ## Consolidate report

"
            ));
            out.push_str(&crate::consolidate::render_report(&report));
            std::fs::write(&path, &out)?;
            crate::consolidate::print_report(&report);
            println!("
steward review prepared: {}", path.display());
            println!("(open it in an agent session with the THOR MCP tools; no writes were made)");
        }
        Commands::Guard { rulebook } => {
            let path = rulebook.unwrap_or_else(crate::guard::default_rulebook_path);
            crate::guard::run_guard(&db, &path);
        }
        Commands::StopGuard { rulebook } => {
            let path = rulebook.unwrap_or_else(crate::guard::default_response_rulebook_path);
            crate::guard::run_stop_guard(&db, &path);
        }
        Commands::Install { settings, with_guard, with_courier, with_daemon, backup_repo } => {
            let path = settings.unwrap_or_else(crate::install::default_settings_path);
            crate::install::run_install(&path, with_guard, with_courier, with_daemon, backup_repo.as_deref())?;
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
            crate::importer::refuse_when_seeded(&db)?;
            let mut store = EventStore::new(&db)?;
            let stats = crate::importer::import_jsonl(&mut store, &path)?;
            // Report EVERY counter: a status-driven run that only retracts used
            // to print "Imported 0 facts" and read as a no-op.
            println!(
                "Import into {}: {} created, {} revised, {} retracted \
                 ({} unchanged, {} duplicates refused, {} malformed, {} diverged skipped).",
                db.display(),
                stats.imported,
                stats.revised,
                stats.retracted,
                stats.skipped_existing,
                stats.skipped_duplicate,
                stats.skipped_malformed,
                stats.skipped_diverged
            );
            // Arm the one-time-seeding guard only when the run actually changed
            // the store: a no-op run (empty or mistyped file) must not lock out
            // the real seeding. A failed flag write is a hard error - reporting
            // success while the store is left unprotected would be exactly the
            // silent hole the guard exists to close.
            if stats.imported + stats.revised + stats.retracted > 0 {
                crate::importer::arm_seeded_flag(&db).map_err(|e| {
                    anyhow::anyhow!(
                        "import succeeded but SEEDED.flag could not be written next to {}: {e}\n\
                         The store is NOT protected against a re-import. Fix the cause and\n\
                         create the file by hand (any content), or re-run the import.",
                        db.display()
                    )
                })?;
                println!(
                    "SEEDED.flag armed next to the store - further imports will be refused \
                     (delete the flag file to deliberately allow another seeding)."
                );
            }
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
                crate::sync::run_reconcile(&db, &to, &token, batch, interval)?;
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
        Commands::DrainInbox { inbox, from, token } => {
            let s = match (inbox, from) {
                (Some(file), None) => run_drain_inbox(&db, &file)?,
                (None, Some(url)) => {
                    let token = token
                        .or_else(|| std::env::var("THOR_TOKEN").ok())
                        .filter(|t| !t.trim().is_empty())
                        .ok_or_else(|| anyhow::anyhow!("no token: pass --token or set THOR_TOKEN"))?;
                    run_drain_http(&db, &url, &token)?
                }
                _ => anyhow::bail!("pass exactly one of --inbox <file> or --from <url>"),
            };
            println!(
                "drain done: {} applied, {} skipped, {} error(s) of {} op(s)",
                s.applied, s.skipped, s.errors, s.total
            );
            if s.errors > 0 {
                anyhow::bail!("{} inbox op(s) failed to apply (kept on the replica for retry)", s.errors);
            }
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
        Commands::SessionStart => {
            // Hook context (fail-open): source tells us WHY the session starts -
            // "compact" means the context was just wiped, the one moment where
            // re-injection is the whole point.
            let hook = read_hook_stdin();
            let source = hook
                .as_ref()
                .and_then(|h| h.get("source"))
                .and_then(|v| v.as_str())
                .unwrap_or("startup")
                .to_string();
            let session_id = hook
                .as_ref()
                .and_then(|h| h.get("session_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cwd: Option<PathBuf> = hook
                .as_ref()
                .and_then(|h| h.get("cwd"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok());

            // Post-compaction: clear this session's courier ledger AND its
            // guard-seen entries, so everything relevant may (and will) inject
            // again into the now-empty context - including the file-touch
            // advisories, whose text was just destroyed with the context.
            if source == "compact" {
                crate::courier::clear_session_ledger(&db, &session_id);
                crate::guard::clear_session_guard_seen(&db, &session_id);
            }

            if let Some(cwd) = cwd.as_deref() {
                if crate::repo::thor_marker_key(cwd).is_some() {
                    // known project: refresh its ingest in the background (non-blocking)
                    let _ = spawn_detached_ingest(&db, &[cwd.to_path_buf()], None);
                } else if let Some(key) = crate::repo::project_key(cwd) {
                    // a git project with no marker: ask before indexing anything
                    println!(
                        "<thor-setup>\nYou are in project '{}', not set up in THOR yet (no .thor \
                         marker). Ask the user whether to set it up now with `thor init` (index its \
                         tracked files), and decide which docs are GLOBAL (cross-cutting, available \
                         in every project) versus project-specific. Do NOT index without the user's \
                         OK. Propose as global by default: CLAUDE.md, dev-loop.md, START-HERE.md and \
                         any conventions docs; keep source code project-scoped.\n</thor-setup>",
                        key
                    );
                }
                // scratch dir (project_key is None): print nothing.
            }
            if let Ok(store) = EventStore::new(&db) {
                if let Ok(events) = store.get_all_events() {
                    // Pinned brief: standing project rules, guaranteed present at
                    // every start (startup / resume / compact) - prompt-recall can
                    // never re-surface them after a compaction on its own, because
                    // a continuation prompt ("ga verder") shares no words with them.
                    let project = cwd.as_deref().and_then(crate::repo::project_key);
                    let pins = crate::ledger::read_pins(&db);
                    let scope = crate::recall::RecallScope::current(project.clone());
                    if let Some(brief) = render_brief(&events, &pins, &scope, &source, project.as_deref()) {
                        println!("{brief}");
                    }
                    // Scope-review nudge (independent of cwd, debounced once per window):
                    // surface no-signal global memories added since the last review so the
                    // agent can offer to reproject the project-specific ones.
                    let wm = crate::review::read_watermark(&db);
                    let cands = crate::review::candidates(&events, wm.reviewed_seq);
                    let now = crate::review::now_secs();
                    if !cands.is_empty() && now.saturating_sub(wm.prompted_at) >= crate::review::DEBOUNCE_SECS {
                        println!(
                            "<thor-scope-review>\n{} global memory(ies) were added without a project since the \
                             last review. Run `thor review-scope` to list them, decide WITH THE USER which belong \
                             to a project, `thor reproject <id> --project <key>` those (leave genuine globals), \
                             then `thor review-scope --mark`. Do not reproject without the user's OK.\n</thor-scope-review>",
                            cands.len()
                        );
                        crate::review::write_watermark(
                            &db,
                            crate::review::Watermark { reviewed_seq: wm.reviewed_seq, prompted_at: now },
                        );
                    }
                }
            }
        }
        Commands::Mark { entity_id, noise } => {
            let mut store = EventStore::new(&db)?;
            if store.get_events_by_entity(&entity_id)?.is_empty() {
                anyhow::bail!("unknown entity: {}", entity_id);
            }
            if noise {
                crate::ledger::increment(&db, "noise", &entity_id);
                println!("marked {} as noise (local ledger, not synced)", entity_id);
            } else {
                let ev = store.append_event("cli", "cli", "cli", EventKind::FactEchoed, &entity_id, None, "")?;
                println!("marked {} as useful (fact_echoed, seq {})", entity_id, ev.seq);
            }
        }
        Commands::Pin { entity_id, list } => {
            let pins = crate::ledger::read_pins(&db);
            match (entity_id, list) {
                (Some(id), false) => {
                    let store = EventStore::new(&db)?;
                    if store.get_events_by_entity(&id)?.is_empty() {
                        anyhow::bail!("unknown entity: {}", id);
                    }
                    // One write transaction (see ledger::mutate_pins): a pin from
                    // the MCP server at the same moment can no longer be dropped.
                    let mut already = false;
                    let pins = crate::ledger::mutate_pins(&db, |mut pins| {
                        if pins.contains(&id) {
                            already = true;
                        } else {
                            pins.push(id.clone());
                        }
                        pins
                    })?;
                    if already {
                        println!("already pinned: {}", id);
                    } else {
                        println!("pinned {} ({} pin(s) total) - it now re-injects at every session start", id, pins.len());
                    }
                }
                _ => {
                    if pins.is_empty() {
                        println!("no pinned facts. Pin one with: thor pin <entity_id>");
                    } else {
                        let store = EventStore::new(&db)?;
                        let events = store.get_all_events()?;
                        let heads = compute_head_sets(&events);
                        let by_hash: HashMap<&str, &Event> =
                            events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
                        println!("{} pinned fact(s):", pins.len());
                        for id in &pins {
                            let first = heads
                                .get(id)
                                .and_then(|hs| {
                                    let mut revs: Vec<&String> = hs.heads.iter().collect();
                                    revs.sort();
                                    revs.first().and_then(|r| by_hash.get(r.as_str())).copied()
                                })
                                .map(|ev| crate::recall::snippet(&ev.body, 100, ""))
                                .unwrap_or_else(|| "(no live head)".to_string());
                            println!("  {}: {}", id, first);
                        }
                    }
                }
            }
        }
        Commands::Unpin { entity_id } => {
            let mut found = false;
            crate::ledger::mutate_pins(&db, |mut pins| {
                let before = pins.len();
                pins.retain(|p| p != &entity_id);
                found = pins.len() != before;
                pins
            })?;
            if found {
                println!("unpinned {}", entity_id);
            } else {
                println!("not pinned: {}", entity_id);
            }
        }
        Commands::Consolidate { apply_dedup, min_age_events } => {
            let mut store = EventStore::new(&db)?;
            let events = store.get_all_events()?;
            let report = crate::consolidate::build_report(
                &store,
                &db,
                &events,
                &crate::consolidate::Options { min_age_events },
            );
            crate::consolidate::print_report(&report);
            if apply_dedup {
                if report.dups.is_empty() {
                    println!("\nnothing to apply: no duplicate twins in the report");
                } else {
                    let stats = crate::consolidate::apply_dedup(&db, &mut store, &report)?;
                    println!(
                        "\nretracted {} duplicate twin(s), {} skipped; re-run for the post-apply report",
                        stats.retracted, stats.skipped
                    );
                }
            } else if !report.is_clean() {
                // CI contract: a store with anything to digest exits nonzero.
                std::process::exit(1);
            }
        }
        Commands::ReviewScope { mark } => {
            let store = EventStore::new(&db)?;
            let events = store.get_all_events()?;
            if mark {
                let tip = crate::review::max_seq(&events);
                crate::review::write_watermark(
                    &db,
                    crate::review::Watermark { reviewed_seq: tip, prompted_at: crate::review::now_secs() },
                );
                println!("scope review marked done up to seq {tip}");
            } else {
                let wm = crate::review::read_watermark(&db);
                let cands = crate::review::candidates(&events, wm.reviewed_seq);
                if cands.is_empty() {
                    println!("no global memories to review (all attributed, or none new since the last review).");
                } else {
                    println!("{} global memory(ies) with no project signal since the last review:", cands.len());
                    for (id, first, seq) in &cands {
                        println!("  {} (seq {}): {}", id, seq, first);
                    }
                    println!(
                        "\nReproject the project-specific ones: thor reproject <id> --project <key> \
                         (leave genuine globals). Then run: thor review-scope --mark"
                    );
                }
            }
        }
    }

    Ok(())
}

/// TTL for the ingest lock: a fresh lock means another ingest is in flight, so a
/// second run (e.g. a rapid SessionStart) skips instead of racing the writer.
const INGEST_LOCK_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Ingest repos into the store, then (semantic build) sync the vector sidecar so
/// new chunks are searchable by meaning too. A lock keeps concurrent runs from
/// piling up on the single writer; a stale lock (> TTL) is ignored.
fn run_ingest(db: &Path, paths: &[PathBuf], project_override: Option<&str>) -> Result<()> {
    let lock = db.with_file_name("thor-ingest.lock");
    if let Ok(meta) = std::fs::metadata(&lock) {
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map(|e| e < INGEST_LOCK_TTL)
            .unwrap_or(false);
        if fresh {
            eprintln!("thor ingest: another ingest is in flight; skipping");
            return Ok(());
        }
    }
    let _ = std::fs::write(&lock, "");
    let result = (|| -> Result<()> {
        let mut store = EventStore::new(db)?;
        let s = crate::ingest::ingest_repos(&mut store, paths, "repo-ingest", project_override)?;
        let tag = match project_override {
            Some(crate::repo::GLOBAL_KEY) => " [global]".to_string(),
            Some(k) => format!(" [{}]", k),
            None => String::new(),
        };
        println!(
            "ingest{}: {} created, {} revised, {} unchanged, {} retracted ({} files; \
             skipped {} binary, {} truncated{})",
            tag,
            s.created,
            s.revised,
            s.unchanged,
            s.retracted,
            s.files,
            s.skipped_binary,
            s.skipped_big,
            if s.diverged_skipped > 0 {
                format!(", {} diverged", s.diverged_skipped)
            } else {
                String::new()
            },
        );
        #[cfg(feature = "semantic")]
        if s.created + s.revised + s.retracted > 0 {
            if let Err(e) = run_vectors(db, "sync", None, false) {
                eprintln!(
                    "thor ingest: vector sidecar sync skipped ({e}); recall still works via bm25"
                );
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&lock);
    // Derived sidecar refresh, best-effort: a symbols failure must never fail an
    // ingest (delete + `thor symbols` rebuilds it). It lives HERE rather than in
    // the Ingest match arm because `thor init` and the SessionStart detached
    // ingest also come through run_ingest, and a path that indexes code but
    // leaves the sidecar stale makes where_used/impact answer on nothing.
    // Deliberately outside the lock: the rebuild is a full pass over the stored
    // chunks, and widening the lock window would make a concurrent ingest skip
    // itself for longer.
    if result.is_ok() {
        if let Ok(store) = EventStore::new(db) {
            if let Ok(mut sy) = crate::symbols::SymbolStore::open_default(db) {
                let _ = sy.rebuild(&store);
            }
        }
    }
    result
}

/// Spawn `thor ingest <paths>` detached with null std handles so it outlives the
/// SessionStart hook and never blocks prompt submission.
fn spawn_detached_ingest(db: &Path, paths: &[PathBuf], project_override: Option<&str>) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--db").arg(db).arg("ingest");
    match project_override {
        Some(crate::repo::GLOBAL_KEY) => {
            cmd.arg("--global");
        }
        Some(k) => {
            cmd.arg("--project").arg(k);
        }
        None => {}
    }
    for p in paths {
        cmd.arg(p);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    cmd.spawn()?;
    Ok(())
}

/// Resolve a path to an absolute, git-friendly root (canonicalize + strip Windows'
/// verbatim prefix + walk up to the repo root).
fn resolve_repo_root(path: &Path) -> PathBuf {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let abs = crate::repo::clean_verbatim_prefix(&abs);
    crate::repo::find_repo_root(&abs).unwrap_or(abs)
}

/// `thor init`: write a `.thor` marker (the stable project key) at the repo root,
/// then ingest the project so it is immediately "known".
fn run_init(db: &Path, path: &Path, key: Option<String>) -> Result<()> {
    let root = resolve_repo_root(path);
    let key = key
        .or_else(|| root.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
        .ok_or_else(|| anyhow::anyhow!("cannot determine a project key for {}", root.display()))?;
    crate::repo::validate_project_key(&key).map_err(|e| anyhow::anyhow!(e))?;
    let marker = root.join(".thor");
    std::fs::write(&marker, format!("{}\n", key))?;
    println!("wrote {} (project key '{}')", marker.display(), key);
    run_ingest(db, &[root], None)
}

/// `thor reproject`: append fact_reprojected event(s) that reassign scope. Sync-safe
/// (the reassignment travels as an event); refuses chunk ids unless `--force`.
fn run_reproject(
    db: &Path,
    entity_id: Option<String>,
    project: Option<String>,
    global: bool,
    batch: bool,
    force: bool,
) -> Result<()> {
    if global && project.is_some() {
        anyhow::bail!("--global and --project are mutually exclusive");
    }
    let (body, target_desc) = if global {
        (r#"{"project":null}"#.to_string(), "global".to_string())
    } else if let Some(key) = project {
        crate::repo::validate_project_key(&key).map_err(|e| anyhow::anyhow!(e))?;
        (serde_json::json!({ "project": key }).to_string(), key)
    } else {
        anyhow::bail!("pass --project <key> or --global");
    };
    let ids: Vec<String> = if batch {
        use std::io::BufRead;
        std::io::stdin()
            .lock()
            .lines()
            .map_while(std::result::Result::ok)
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    } else {
        match entity_id {
            Some(id) => vec![id],
            None => anyhow::bail!("pass an entity_id, or --batch to read ids from stdin"),
        }
    };
    let mut store = EventStore::new(db)?;
    let mut n = 0;
    for id in &ids {
        // Trim control/whitespace residue: a CRLF text file fed through a shell
        // loop delivers "id\r", and an unchecked append would then mint a
        // reproject for a PHANTOM entity (happened live 2026-07-10: 153 stray
        // events against ids that never existed).
        let id = id.trim();
        if crate::repo::is_chunk_id(id) && !force {
            eprintln!("skip chunk-shaped id (managed by ingest; use --force to override): {}", id);
            continue;
        }
        // Same existence contract as the MCP reproject tool: never append scope
        // metadata for an entity the log does not know.
        if store.get_events_by_entity(id)?.is_empty() {
            eprintln!("skip unknown entity: {}", id);
            continue;
        }
        store.append_event("reproject", "reproject", "cli", EventKind::FactReprojected, id, None, &body)?;
        n += 1;
    }
    println!("reprojected {} entit{} to {}", n, if n == 1 { "y" } else { "ies" }, target_desc);
    Ok(())
}

/// Parse a mimir import footer's `| project: <name> |` field, if present
/// (shim: the footer format and its parsers live together in crate::footer).
fn parse_mimir_footer_project(body: &str) -> Option<String> {
    crate::footer::project(body)
}

/// `thor backfill-projects`: attribute legacy unprefixed memories to the project
/// named in their mimir import footer (deterministic, idempotent). Dry-run unless
/// `apply`. Memories with no footer / a global footer stay global.
fn run_backfill_projects(db: &Path, apply: bool) -> Result<()> {
    let mut store = EventStore::new(db)?;
    let events = store.get_all_events()?;
    let heads = compute_head_sets(&events);
    let projects = crate::cas::compute_projects(&events);
    let by_rev: HashMap<&str, &Event> = events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
    let mut planned: Vec<(String, String)> = Vec::new();
    for (eid, hs) in &heads {
        // only unprefixed (global-born) entities with no existing project override
        if crate::repo::owner_project(eid).is_some() {
            continue;
        }
        if projects.get(eid).and_then(|o| o.as_deref()).is_some() {
            continue;
        }
        if hs.heads.len() != 1 {
            continue;
        }
        let ev = match hs.heads.iter().next().and_then(|rev| by_rev.get(rev.as_str())) {
            Some(e) => *e,
            None => continue,
        };
        if let Some(proj) = parse_mimir_footer_project(&ev.body) {
            if proj != "global" {
                planned.push((eid.clone(), proj));
            }
        }
    }
    if planned.is_empty() {
        println!("backfill: nothing to attribute (no footers with a non-global project).");
        return Ok(());
    }
    planned.sort();
    println!("backfill: {} memor{} to reproject:", planned.len(), if planned.len() == 1 { "y" } else { "ies" });
    for (eid, proj) in &planned {
        println!("  {} -> {}", eid, proj);
    }
    if !apply {
        println!("(dry-run; re-run with --apply to write the reprojections)");
        return Ok(());
    }
    for (eid, proj) in &planned {
        let body = serde_json::json!({ "project": proj }).to_string();
        store.append_event("backfill", "backfill", "cli", EventKind::FactReprojected, eid, None, &body)?;
    }
    println!("backfill: applied {} reprojection(s).", planned.len());
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

    let default_dir = embed::default_model_dir();
    let model_dir = match model_dir {
        Some(explicit) => {
            // --model-dir is a one-shot override for THIS command. The per-prompt
            // courier and the resident embed daemon always read the default
            // directory, so a sidecar built from a model that only exists here is
            // one recall can never load - and that failure is silent, recall just
            // stays on bm25. Say it out loud rather than let a doomed build look
            // like it worked.
            eprintln!(
                "note: --model-dir applies to this command only - recall and `thor embed-daemon` \
                 load the model from {}.",
                default_dir
                    .as_deref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|| "(nowhere: LOCALAPPDATA, XDG_DATA_HOME and HOME are all unset)".into())
            );
            explicit
        }
        None => default_dir.ok_or_else(|| {
            anyhow::anyhow!(
                "no per-user data directory for the model (LOCALAPPDATA, XDG_DATA_HOME and HOME \
                 are all unset): set one, or pass --model-dir <dir>"
            )
        })?,
    };
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
    fn footer_parse() {
        assert_eq!(
            parse_mimir_footer_project(
                "body\n\n[memory/gotcha | tags: x y | project: SomeProj | mimir:01K]"
            ),
            Some("SomeProj".to_string())
        );
        assert_eq!(
            parse_mimir_footer_project("[... | project: global | mimir:z]"),
            Some("global".to_string())
        );
        assert_eq!(parse_mimir_footer_project("no footer here"), None);
    }

    #[test]
    fn reproject_flips_scope_and_backfill_from_footer() {
        use crate::recall::{recall_scoped, RecallScope};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            store
                .append_event("s", "l", "a", EventKind::FactCreated, "mcp-widget", None, "the widget setting is blue")
                .unwrap();
            store
                .append_event(
                    "s", "l", "a", EventKind::FactCreated, "01KDEVICE", None,
                    "device heater note\n\n[memory/gotcha | tags: firmware | project: SomeProj | mimir:01KDEVICE]",
                )
                .unwrap();
        }
        // reproject the global memory to ProjA; backfill the SomeProj memory from its footer
        run_reproject(&db, Some("mcp-widget".into()), Some("ProjA".into()), false, false, false).unwrap();
        run_backfill_projects(&db, true).unwrap();

        let store = EventStore::new(&db).unwrap();
        let in_scope = |q: &str, proj: &str| {
            recall_scoped(&store, q, 5, &RecallScope::current(Some(proj.to_string()))).unwrap()
        };
        assert!(in_scope("widget setting", "ProjA").iter().any(|h| h.entity_id == "mcp-widget"), "reproject -> ProjA");
        assert!(!in_scope("widget setting", "ProjB").iter().any(|h| h.entity_id == "mcp-widget"), "not in ProjB");
        assert!(in_scope("device heater", "SomeProj").iter().any(|h| h.entity_id == "01KDEVICE"), "backfill -> SomeProj");
        assert!(
            !in_scope("device heater", "OtherProj").iter().any(|h| h.entity_id == "01KDEVICE"),
            "the backfilled memory no longer bleeds into another project"
        );
    }

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

    #[test]
    fn test_render_brief_scope_types_and_retraction() {
        use crate::recall::RecallScope;
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event(
                "s", "l", "a", EventKind::FactCreated, "rule-global", None,
                "HARDE REGEL: nooit force-recreate op prod",
            )
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "ProjB:mem-1", None, "GOTCHA: B-only rule")
            .unwrap();
        let dead = store
            .append_event("s", "l", "a", EventKind::FactCreated, "dead-pin", None, "obsolete rule")
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactRetracted, "dead-pin", Some(&dead.this_hash), "")
            .unwrap();
        let events = store.get_all_events().unwrap();
        let pins = vec!["rule-global".to_string(), "ProjB:mem-1".to_string(), "dead-pin".to_string()];

        let scope_a = RecallScope::current(Some("ProjA".to_string()));
        let brief = render_brief(&events, &pins, &scope_a, "compact", Some("ProjA")).expect("brief renders");
        assert!(brief.contains("<thor-brief>"));
        assert!(brief.contains("start: compact"), "trigger stated: {brief}");
        assert!(brief.contains("[preference] rule-global"), "global pin, typed: {brief}");
        assert!(brief.contains("nooit force-recreate op prod"), "FULL body, not a 220-snippet: {brief}");
        assert!(!brief.contains("ProjB:mem-1"), "another project's pin stays out of scope: {brief}");
        assert!(!brief.contains("dead-pin"), "a retracted pin is never re-injected: {brief}");

        // no pins in scope -> no block at all
        let none = render_brief(&events, &["ProjB:mem-1".to_string()], &scope_a, "startup", Some("ProjA"));
        assert!(none.is_none(), "nothing in scope -> silence");
        assert!(render_brief(&events, &[], &scope_a, "startup", None).is_none(), "no pins -> silence");
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

    #[test]
    fn drain_applies_create_then_revise_preserving_id_and_footer() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let inbox = dir.path().join("inbox.jsonl");
        crate::inbox::append(&inbox, &crate::inbox::InboxOp {
            op: "create".into(),
            entity_id: "acme:mem-metrics".into(),
            body: "the metrics port is 9464\n\n[memory/gotcha | tags:  | project: acme]".into(),
            parent_rev: None,
            ts: "1".into(),
            capture_id: "c1".into(),
        }).unwrap();
        crate::inbox::append(&inbox, &crate::inbox::InboxOp {
            op: "revise".into(),
            entity_id: "acme:mem-metrics".into(),
            body: "the metrics port is 9464 (confirmed)\n\n[memory/gotcha | tags:  | project: acme]".into(),
            parent_rev: None,
            ts: "2".into(),
            capture_id: "c2".into(),
        }).unwrap();

        let summary = run_drain_inbox(&db, &inbox).unwrap();
        assert_eq!(summary, DrainSummary { total: 2, applied: 2, skipped: 0, errors: 0 });

        let store = EventStore::new(&db).unwrap();
        let events = store.get_all_events().unwrap();
        let heads = crate::cas::compute_head_sets(&events);
        let hs = heads.get("acme:mem-metrics").expect("entity landed on the authority");
        assert!(!hs.is_diverged && hs.heads.len() == 1, "one clean head, no fork");
        let head_hash = hs.heads.iter().next().unwrap();
        let head = events.iter().find(|e| &e.this_hash == head_hash).unwrap();
        assert!(head.body.contains("confirmed"), "revise applied onto the same id");
        assert!(head.body.contains("[memory/gotcha"), "footer preserved through the drain");
    }

    #[test]
    fn drain_is_idempotent_on_create() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let inbox = dir.path().join("inbox.jsonl");
        crate::inbox::append(&inbox, &crate::inbox::InboxOp {
            op: "create".into(),
            entity_id: "acme:mem-x".into(),
            body: "a fact\n\n[memory/note | project: acme]".into(),
            parent_rev: None,
            ts: "1".into(),
            capture_id: "c1".into(),
        }).unwrap();
        assert_eq!(run_drain_inbox(&db, &inbox).unwrap().applied, 1);
        // Re-draining the same file must not double-create: the create is skipped.
        let second = run_drain_inbox(&db, &inbox).unwrap();
        assert_eq!((second.applied, second.skipped, second.errors), (0, 1, 0));
        let store = EventStore::new(&db).unwrap();
        let heads = crate::cas::compute_head_sets(&store.get_all_events().unwrap());
        assert_eq!(heads.get("acme:mem-x").unwrap().heads.len(), 1, "still exactly one head");
    }

    /// The exit-code contract, and the reason it is worth a test: `thor fsck`
    /// used to print "CHAIN INTEGRITY ERROR" and then exit 0, so no scheduled
    /// job, release step or CI run could ever act on it. These three cases pin
    /// the split - integrity fails hard, content health does not.
    fn seed_store(dir: &std::path::Path) -> std::path::PathBuf {
        let db = dir.join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        for i in 0..5 {
            store
                .append_event(
                    "s",
                    "l",
                    "a",
                    EventKind::FactCreated,
                    &format!("e{i}"),
                    None,
                    &format!("a durable fact number {i}\n\n[memory/note | project: global]"),
                )
                .unwrap();
        }
        db
    }

    #[test]
    fn fsck_reports_clean_on_a_healthy_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = seed_store(dir.path());
        assert_eq!(fsck_report(&db, false).unwrap(), FsckOutcome::Clean);
    }

    #[test]
    fn fsck_reports_an_integrity_failure_when_the_chain_is_tampered_with() {
        let dir = tempfile::tempdir().unwrap();
        let db = seed_store(dir.path());
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("UPDATE event SET body_ch = 'tampered' WHERE seq = 3", [])
                .unwrap();
        }
        assert_eq!(
            fsck_report(&db, false).unwrap(),
            FsckOutcome::IntegrityFailure,
            "a rewritten body must fail the hash chain, and that must reach the exit code"
        );
    }

    #[test]
    fn fsck_does_not_fail_on_footer_defects_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // A footer followed by trailing text: content health, not corruption.
            store
                .append_event(
                    "s",
                    "l",
                    "a",
                    EventKind::FactCreated,
                    "e-broken-footer",
                    None,
                    "a fact\n\n[memory/note | project: global]\nKind: fact_created",
                )
                .unwrap();
        }
        match fsck_report(&db, false).unwrap() {
            FsckOutcome::FooterDefects(n) => assert_eq!(n, 1, "one defective footer"),
            other => panic!("footer defects must not be an integrity failure, got {other:?}"),
        }
    }

    /// `thor init` is the command SETUP.md tells a new user to run, so it must
    /// leave where_used/impact with a sidecar to answer from - not just a marker
    /// file. The refresh used to sit in the Ingest match arm, which init bypassed.
    #[test]
    fn init_and_ingest_both_refresh_the_symbol_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let proj = dir.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        std::fs::write(proj.join("lib.rs"), "pub fn pack_blocks(x: u32) {}\n").unwrap();
        std::fs::write(proj.join("main.rs"), "fn main() { pack_blocks(7); }\n").unwrap();

        run_init(&db, &proj, Some("initproj".to_string())).unwrap();
        {
            let sy = crate::symbols::SymbolStore::open_default(&db).unwrap();
            assert_eq!(sy.definers_of("pack_blocks", Some("initproj")), vec!["initproj:lib.rs#0"]);
            assert_eq!(sy.callers_of("pack_blocks", Some("initproj")), vec!["initproj:main.rs#0"]);
        }
        // The refresh is unconditional, so a DELETED sidecar is healed by the next
        // ingest even though that run changes nothing in the log.
        std::fs::remove_file(crate::symbols::default_symbols_path(&db)).unwrap();
        run_ingest(&db, &[proj.clone()], None).unwrap();
        let sy = crate::symbols::SymbolStore::open_default(&db).unwrap();
        assert_eq!(sy.definers_of("pack_blocks", Some("initproj")), vec!["initproj:lib.rs#0"]);
    }

    /// The pin help is the only place a user is ever told WHERE pins live, so a
    /// stale filename there sends people hunting for a file no install has. Pins
    /// moved into the SQLite ledger sidecar (ledger::ledger_path) and the help has
    /// to follow, which nothing else in the build checks.
    #[test]
    fn pin_help_names_the_ledger_sidecar_pins_really_live_in() {
        use clap::CommandFactory;
        let help = Cli::command()
            .find_subcommand_mut("pin")
            .expect("the pin subcommand exists")
            .render_long_help()
            .to_string();
        assert!(
            !help.contains("thor-pins.json"),
            "the help must not name the pre-SQLite pin file: {help}"
        );
        assert!(
            help.contains("thor-ledger.db"),
            "the help must name the ledger sidecar pins live in: {help}"
        );
    }
}
