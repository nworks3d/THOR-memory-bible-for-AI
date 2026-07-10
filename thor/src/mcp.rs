//! THOR MCP server: the agent's full stewardship surface - read (recall / get /
//! history / brief), write (remember with duplicate refusal + typed footers),
//! repair (revise / retract / resolve, all CAS-checked so a stale write returns
//! the fresh head-set instead of minting a silent branch), and curate (mark /
//! pin / unpin / reproject). Served on stdio (`thor mcp`, the local connector)
//! or Streamable-HTTP (`thor mcp --http <bind>`, the remote connector deployed
//! on the NAS - THOR's own server, port and DB, fully independent of mimir).
//! Built on rmcp (mimir's proven recipe).
//!
//! The store is sync (rusqlite); every tool hops through spawn_blocking so the
//! async transport is never blocked. One shared store behind a Mutex serves all
//! HTTP sessions (WAL + the Mutex handle concurrency).

use crate::cli::{render_get, render_history};
use crate::event_store::{EventKind, EventStore, MutateConflict, ResolveConflict};
use crate::recall::{recall_memories_scoped, RecallScope};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const INSTRUCTIONS: &str = "THOR is the user's lossless memory - and YOURS to maintain, not just \
to fill. recall searches the current-head facts (scoped to the current project + the global tier; \
all_projects:true for everything, project:\"<key>\" for one, kind:\"memory\" to exclude code \
chunks when you want notes/decisions). get shows one entity's full head(s); history its revision \
log. remember stores a NEW fact (recall first - near-duplicates are refused) and accepts \
fact_type (gotcha|decision|preference) + tags. When a fact you see is outdated or wrong: revise \
it (auto-fills parent_rev when single-headed) or retract it - do NOT remember a duplicate. When \
you meet a [DIVERGED] fact: resolve it (keep_rev wins) as soon as you know the right head. When \
an injected/recalled fact actually helped you: mark it (improves future ranking). pin/unpin \
manage the standing rules re-injected at every session start; brief shows what THOR knows about \
the current project (counts, recent facts, pins, diverged). reproject moves a mis-scoped fact to \
another project or global.";

/// The current project for a stdio server, from its launch cwd (Claude Code starts
/// the connector in the project dir). `None` for the HTTP server (no cwd).
fn startup_project() -> Option<String> {
    std::env::current_dir().ok().and_then(|c| crate::repo::project_key(&c))
}

#[derive(Clone)]
pub struct ThorServer {
    store: Arc<Mutex<EventStore>>,
    /// Project the server was launched in (`None` = unscoped / HTTP). Recall
    /// defaults to this; remember tags new facts with it.
    project: Option<String>,
    /// Store path, for the sidecars that live NEXT to the db (pins). An empty
    /// path (in-memory test store) keeps pin/unpin working in a temp cwd.
    db: PathBuf,
    #[allow(dead_code)] // read only inside the #[tool_handler] macro expansion
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RecallArgs {
    /// What to search for (natural language or keywords).
    pub query: String,
    /// Max hits (default 8).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Search every project, not just the current one + the global tier.
    #[serde(default)]
    pub all_projects: bool,
    /// Scope to a specific project key (that project + global). Overrides the
    /// server's current project.
    #[serde(default)]
    pub project: Option<String>,
    /// "memory" = only hand-written facts (notes/gotchas/decisions), never repo
    /// code chunks. Omit for everything.
    #[serde(default)]
    pub kind: Option<String>,
    /// true = rescore the top of the result pool with the local cross-encoder
    /// before returning (slower - one transformer pass per hit - but much
    /// better paraphrase ordering). Use it as a deliberate second try when the
    /// normal order looks wrong. Silently keeps the normal order when the
    /// reranker model is not installed.
    #[serde(default)]
    pub rerank: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct GetArgs {
    /// The entity id to show the current head(s) of.
    pub entity_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RememberArgs {
    /// The fact to store (concise, self-contained).
    pub body: String,
    /// Optional entity id; a new one is minted if omitted.
    #[serde(default)]
    pub entity_id: Option<String>,
    /// Project scope for the new fact: a project key, or "global" for a
    /// cross-project fact. Omitted = the server's current project.
    #[serde(default)]
    pub project: Option<String>,
    /// Constraint class: "gotcha", "decision", or "preference". Typed facts get
    /// a footer, a tag in recall/injection, and (guard/brief) priority.
    #[serde(default)]
    pub fact_type: Option<String>,
    /// Free-form tags, stored in the footer for later search.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// WHEN should this fact fire? The exact task words a future prompt would
    /// contain when this fact matters: commands ("docker compose"), file names
    /// ("deploy-watcher.sh"), error strings ("subsystem request failed").
    /// Stored as a fires-when footer field; a query hitting these words gets a
    /// deliberate ranking boost toward this fact.
    #[serde(default)]
    pub triggers: Option<Vec<String>>,
    /// Exact file paths or command strings this fact GOVERNS (e.g.
    /// "deploy/watcher.sh", "docker compose up"). The moment-of-action guard
    /// surfaces the fact verbatim when a tool call touches one - exact match,
    /// no ranking involved. Use for hard constraints tied to a specific file
    /// or command.
    #[serde(default)]
    pub anchors: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ReviseArgs {
    /// The entity id to update.
    pub entity_id: String,
    /// The corrected, full replacement body.
    pub body: String,
    /// The head rev this update is based on. Omit when the entity has a single
    /// head (auto-filled); required when it is DIVERGED. A stale value is
    /// rejected with the fresh head-set instead of creating a silent branch.
    #[serde(default)]
    pub parent_rev: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RetractArgs {
    /// The entity id to retract (recall stops surfacing it; history is kept).
    pub entity_id: String,
    /// The head rev being retracted (omit when single-headed).
    #[serde(default)]
    pub parent_rev: Option<String>,
    /// Why it is retracted (kept in the log).
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ResolveArgs {
    /// The DIVERGED entity to settle.
    pub entity_id: String,
    /// The head rev that wins; every other contested head is discarded.
    pub keep_rev: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct EntityArgs {
    /// The entity id.
    pub entity_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MarkArgs {
    /// The entity id.
    pub entity_id: String,
    /// true = this fact was NOISE here (injected/recalled but only
    /// distracting): demotes its promotion and feeds decay, locally.
    /// Default false = useful.
    #[serde(default)]
    pub noise: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ReprojectArgs {
    /// The entity id to move (memories only, never repo chunks).
    pub entity_id: String,
    /// Target project key, or "global" for the cross-project tier.
    pub project: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct BriefArgs {
    /// Project key to brief on. Omitted = the server's current project.
    #[serde(default)]
    pub project: Option<String>,
}

#[tool_router]
impl ThorServer {
    pub fn new(store: EventStore, db: PathBuf) -> Self {
        Self::from_shared(Arc::new(Mutex::new(store)), None, db)
    }

    pub fn from_shared(store: Arc<Mutex<EventStore>>, project: Option<String>, db: PathBuf) -> Self {
        ThorServer { store, project, db, tool_router: Self::tool_router() }
    }

    /// Run a sync store closure off the async runtime; its Ok/Err String is the
    /// tool's returned text either way (tool errors are surfaced as content).
    fn blocking<F>(&self, f: F) -> impl std::future::Future<Output = String>
    where
        F: FnOnce(&mut EventStore) -> Result<String, String> + Send + 'static,
    {
        let store = self.store.clone();
        async move {
            tokio::task::spawn_blocking(move || {
                let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                f(&mut s).unwrap_or_else(|e| e)
            })
            .await
            .unwrap_or_else(|e| format!("error: task panicked: {e}"))
        }
    }

    #[tool(description = "Search THOR memory and return the best-matching CURRENT-HEAD facts, ranked. Read-only. kind:\"memory\" excludes repo code chunks.")]
    async fn recall(&self, Parameters(args): Parameters<RecallArgs>) -> String {
        let server_project = self.project.clone();
        let db = self.db.clone();
        self.blocking(move |s| {
            // Scope: all_projects > explicit project > the server's current project.
            let scope = if args.all_projects {
                RecallScope::everything()
            } else if args.project.is_some() {
                RecallScope::current(args.project.clone())
            } else {
                RecallScope::current(server_project.clone())
            };
            let limit = args.limit.unwrap_or(8);
            // A rerank pass rescoring only `limit` hits could never RESCUE a
            // gold buried just below it - fetch the rescore pool size instead,
            // reorder, then cut back to the requested limit.
            #[cfg(feature = "semantic")]
            let fetch = if args.rerank { limit.max(crate::rerank::RERANK_TOP_N) } else { limit };
            #[cfg(not(feature = "semantic"))]
            let fetch = limit;
            let memories_only = args.kind.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("memory"));
            #[allow(unused_mut)]
            let mut hits = if memories_only {
                recall_memories_scoped(s, &args.query, fetch, &scope).map_err(|e| format!("error: {e}"))?
            } else {
                // Fused parity with the courier: a deliberate agent query gets
                // the same semantic score-fusion path (bm25 on non-semantic
                // builds or any semantic failure - recall_for degrades itself).
                crate::courier::recall_for(&db, s, &args.query, &scope, fetch)
            };
            if hits.is_empty() {
                return Ok(format!("No THOR hits for: {}", args.query));
            }
            let mut rerank_note = "";
            if args.rerank {
                #[cfg(feature = "semantic")]
                {
                    let (reordered, applied) = crate::rerank::rerank_hits(&args.query, hits);
                    hits = reordered;
                    if !applied {
                        rerank_note = "(rerank skipped: reranker model unavailable or nothing to reorder - fused order)\n";
                    }
                }
                #[cfg(not(feature = "semantic"))]
                {
                    rerank_note = "(rerank unavailable: non-semantic build - fused order)\n";
                }
            }
            hits.truncate(limit);
            // Freshness context: the stdio server's launch cwd (the project the
            // agent is working in). The HTTP server has no meaningful cwd and
            // freshness passes through - it never re-reads another machine's disk.
            let cwd = std::env::current_dir().ok().map(|c| c.display().to_string());
            let fresh_project = cwd.as_deref().and_then(|c| crate::repo::project_key(Path::new(c)));
            let mut out = String::from(rerank_note);
            for hit in hits {
                // A served hit is an access: counted in the LOCAL ledger (decay
                // signal for consolidate), never in the synced hash-chained log.
                crate::ledger::increment(&db, "access", &hit.entity_id);
                let short = &hit.rev[..hit.rev.len().min(8)];
                let (fresh_tag, snip) = crate::courier::serve_deliberate(
                    &hit.entity_id, &hit.body, &args.query, fresh_project.as_deref(), cwd.as_deref(),
                );
                let diverged = if hit.is_diverged { " [DIVERGED]" } else { "" };
                let ty = hit.fact_type.map(|t| format!(" [{}]", t.as_str())).unwrap_or_default();
                let tag = if crate::repo::is_global(hit.project.as_deref()) {
                    "[global]".to_string()
                } else {
                    format!("[proj:{}]", hit.project.as_deref().unwrap_or("?"))
                };
                out.push_str(&format!(
                    "{}{} {} ({}{}{}): {}\n",
                    tag, ty, hit.entity_id, short, diverged, fresh_tag, snip
                ));
            }
            out.push_str("(full body: get <entity_id>; helped you? mark <entity_id>)\n");
            Ok(out)
        })
        .await
    }

    #[tool(description = "Show the current head(s) of one THOR entity by id (DIVERGED shows every contested head).")]
    async fn get(&self, Parameters(args): Parameters<GetArgs>) -> String {
        let db = self.db.clone();
        self.blocking(move |s| {
            let events = s.get_all_events().map_err(|e| format!("error: {e}"))?;
            // Only an EXISTING entity counts as an access - a typo'd id must not
            // seed a phantom counter in the ledger.
            if events.iter().any(|e| e.entity_id == args.entity_id) {
                crate::ledger::increment(&db, "access", &args.entity_id);
            }
            Ok(render_get(&args.entity_id, &events))
        })
        .await
    }

    #[tool(description = "Show one entity's full revision history (seq, kind, rev, parent per event).")]
    async fn history(&self, Parameters(args): Parameters<EntityArgs>) -> String {
        self.blocking(move |s| {
            let events = s.get_events_by_entity(&args.entity_id).map_err(|e| format!("error: {e}"))?;
            Ok(render_history(&args.entity_id, &events))
        })
        .await
    }

    #[tool(description = "Store a NEW fact (fact_created). Recall first; a near-duplicate of a live fact is refused with a pointer to it. Accepts fact_type (gotcha|decision|preference), tags, and triggers - ask yourself WHEN this fact should fire and pass those exact task words (commands, file names, error strings) so future recall boosts it at the right moment.")]
    async fn remember(&self, Parameters(args): Parameters<RememberArgs>) -> String {
        let server_project = self.project.clone();
        self.blocking(move |s| {
            let body = args.body.trim();
            if body.is_empty() {
                return Err("a non-empty 'body' is required".to_string());
            }
            let mint = match args.project {
                Some(p) if p.eq_ignore_ascii_case("global") => None,
                Some(p) => {
                    // Same validation as init/reproject/ingest: a key with ':' or
                    // '#' would mis-split the minted id and silently mis-scope
                    // the fact (owner_project takes the prefix before the FIRST ':').
                    crate::repo::validate_project_key(&p)?;
                    Some(p)
                }
                None => server_project,
            };
            let entity_id = match args.entity_id.filter(|s| !s.is_empty()) {
                // A caller-supplied id is used verbatim (its prefix defines scope) -
                // but never a chunk id: repo chunks are managed by ingest alone, and
                // a fact_created on one would mint a contested head ingest can never
                // reconcile. (Existing MEMORY ids are refused atomically below.)
                Some(id) => {
                    if crate::repo::is_chunk_id(&id) {
                        return Err(format!(
                            "{} is a repo chunk id (managed by ingest); omit entity_id to mint a \
                             memory id",
                            id
                        ));
                    }
                    id
                }
                // A scoped memory is `<key>:mem-<uuid>`, global `mcp-<uuid>`.
                None => crate::repo::memory_entity_id(mint.as_deref(), &Uuid::new_v4().to_string()),
            };
            // Typed footer, stamped at write time via the footer module (the
            // format's single owner - the same code every parser reads back).
            // Only when the caller passed a type or tags.
            let clean_body = body.to_string();
            let mut body = body.to_string();
            let triggers = args.triggers.unwrap_or_default();
            let anchors = args.anchors.unwrap_or_default();
            if args.fact_type.is_some()
                || args.tags.as_deref().is_some_and(|t| !t.is_empty())
                || !triggers.is_empty()
                || !anchors.is_empty()
            {
                let scope_label = crate::repo::owner_project(&entity_id)
                    .map(str::to_string)
                    .unwrap_or_else(|| "global".into());
                let footer = crate::footer::compose(
                    args.fact_type.as_deref().unwrap_or("note"),
                    &args.tags.unwrap_or_default(),
                    &scope_label,
                    &triggers,
                    &anchors,
                );
                body.push_str("\n\n");
                body.push_str(&footer);
            }
            // Near-duplicate refusal + entity-exists refusal, ATOMIC with the
            // append (same immediate write lock), so a concurrent writer process
            // cannot slip an equal fact between check and append. Same
            // normalization as recall's near-duplicate collapse (footer-stripped,
            // so a typed fact's footer cannot defeat it), scoped to where the NEW
            // fact will live - the same body deliberately stored for a different
            // project is a legitimate write, not a duplicate.
            let scope = RecallScope::current(crate::repo::owner_project(&entity_id).map(str::to_string));
            let prefix = crate::recall::dedup_prefix(&clean_body);
            match s.append_created_unique("mcp", "mcp-session", "mcp", &entity_id, &body, |_, project, head_body| {
                scope.allows(project) && crate::recall::dedup_prefix(head_body) == prefix
            }) {
                Ok(ev) => Ok(format!("stored entity {} rev {}", entity_id, ev.this_hash)),
                Err(e) => match e.downcast_ref::<MutateConflict>() {
                    Some(c) if c.reason.starts_with("near-duplicate") => Err(format!(
                        "NOT stored: near-duplicate of {} (rev {}). Use revise(entity_id:\"{}\") to \
                         update it, mark(entity_id:\"{}\") if it just proved useful, or reword if it is \
                         genuinely a different fact.",
                        c.entity_id,
                        c.current_heads.first().map(|r| &r[..r.len().min(8)]).unwrap_or(""),
                        c.entity_id,
                        c.entity_id
                    )),
                    Some(c) => Err(format!(
                        "NOT stored: {} - {}. Current head-set: {:?}.",
                        c.entity_id, c.reason, c.current_heads
                    )),
                    None => Err(format!("error: {e}")),
                },
            }
        })
        .await
    }

    #[tool(description = "Update an existing fact with a corrected body (fact_revised). Single-headed facts auto-fill parent_rev; a stale parent_rev is rejected with the fresh head-set (no silent branch). Prefer this over remember for a fact that CHANGED.")]
    async fn revise(&self, Parameters(args): Parameters<ReviseArgs>) -> String {
        self.blocking(move |s| {
            let body = args.body.trim();
            if body.is_empty() {
                return Err("a non-empty 'body' is required".to_string());
            }
            match s.append_mutate_checked(
                "mcp",
                "mcp-session",
                "mcp",
                EventKind::FactRevised,
                &args.entity_id,
                args.parent_rev.as_deref(),
                body,
            ) {
                Ok(ev) => Ok(format!("revised {} -> rev {}", args.entity_id, ev.this_hash)),
                Err(e) => Err(render_mutate_err(e)),
            }
        })
        .await
    }

    #[tool(description = "Retract a wrong or obsolete fact (fact_retracted): recall stops surfacing it, the log keeps its history. Same CAS rules as revise.")]
    async fn retract(&self, Parameters(args): Parameters<RetractArgs>) -> String {
        self.blocking(move |s| {
            // Never store an empty retraction body: a retracted rev STAYS a head,
            // and when it is one side of a DIVERGED entity the courier renders its
            // body - a blank, unlabeled line is not something an agent can
            // reconcile against.
            let body = args
                .reason
                .filter(|r| !r.trim().is_empty())
                .unwrap_or_else(|| "[retracted via mcp]".to_string());
            match s.append_mutate_checked(
                "mcp",
                "mcp-session",
                "mcp",
                EventKind::FactRetracted,
                &args.entity_id,
                args.parent_rev.as_deref(),
                &body,
            ) {
                Ok(ev) => Ok(format!("retracted {} (rev {})", args.entity_id, ev.this_hash)),
                Err(e) => Err(render_mutate_err(e)),
            }
        })
        .await
    }

    #[tool(description = "Settle a DIVERGED entity: keep_rev becomes the single head, every other contested head is discarded. Run when get/recall shows [DIVERGED] and you know which head is right.")]
    async fn resolve(&self, Parameters(args): Parameters<ResolveArgs>) -> String {
        self.blocking(move |s| {
            // Derive the discard set from the current heads; append_resolve
            // re-verifies the citation under the write lock, so a concurrent
            // change comes back as a conflict instead of a wrong resolve.
            let events = s.get_all_events().map_err(|e| format!("error: {e}"))?;
            let heads = crate::cas::compute_head_sets(&events);
            let current = heads
                .get(&args.entity_id)
                .map(|h| h.heads.clone())
                .unwrap_or_default();
            if current.is_empty() {
                return Err(format!("unknown entity: {}", args.entity_id));
            }
            let discarded: Vec<String> =
                current.iter().filter(|r| **r != args.keep_rev).cloned().collect();
            match s.append_resolve("mcp", "mcp-session", "mcp", &args.entity_id, &args.keep_rev, &discarded) {
                Ok(_) => Ok(format!("resolved {}: kept rev {}", args.entity_id, args.keep_rev)),
                Err(e) => match e.downcast_ref::<ResolveConflict>() {
                    Some(c) => Err(format!(
                        "resolve rejected: {}. Current head-set: {:?} - re-run citing exactly these.",
                        c.reason, c.current_heads
                    )),
                    None => Err(format!("error: {e}")),
                },
            }
        })
        .await
    }

    #[tool(description = "Mark a fact as USEFUL (it actually answered your question / prevented a mistake), or with noise:true as NOISE (it was injected/recalled but only distracted here). Marking honestly improves your own future recall: useful feeds the promotion prior, noise demotes and feeds decay. Useful is a synced head-neutral event; noise stays in the local ledger.")]
    async fn mark(&self, Parameters(args): Parameters<MarkArgs>) -> String {
        let db = self.db.clone();
        self.blocking(move |s| {
            if s.get_events_by_entity(&args.entity_id).map_err(|e| format!("error: {e}"))?.is_empty() {
                return Err(format!("unknown entity: {}", args.entity_id));
            }
            if args.noise {
                // "Noise for me during this task" is a LOCAL judgement, not an
                // institutional fact: it lives in the ledger, never the synced
                // log (see crate::strength for how it counts against echoes).
                crate::ledger::increment(&db, "noise", &args.entity_id);
                return Ok(format!("marked {} as noise (local)", args.entity_id));
            }
            s.append_event("mcp", "mcp-session", "mcp", EventKind::FactEchoed, &args.entity_id, None, "")
                .map_err(|e| format!("error: {e}"))?;
            Ok(format!("marked {} as useful", args.entity_id))
        })
        .await
    }

    #[tool(description = "Pin a fact: its full body is then re-injected at EVERY session start and right after a compaction (<thor-brief>). Use for standing rules the user states (\"never X on prod\").")]
    async fn pin(&self, Parameters(args): Parameters<EntityArgs>) -> String {
        let db = self.db.clone();
        self.blocking(move |s| {
            if s.get_events_by_entity(&args.entity_id).map_err(|e| format!("error: {e}"))?.is_empty() {
                return Err(format!("unknown entity: {}", args.entity_id));
            }
            // One write transaction: a concurrent pin (CLI, another session)
            // can no longer be dropped by a last-write-wins overwrite.
            let mut already = false;
            let pins = crate::ledger::mutate_pins(&db, |mut pins| {
                if pins.contains(&args.entity_id) {
                    already = true;
                } else {
                    pins.push(args.entity_id.clone());
                }
                pins
            })
            .map_err(|e| format!("error: {e}"))?;
            if already {
                return Ok(format!("already pinned: {}", args.entity_id));
            }
            Ok(format!("pinned {} ({} pin(s) total)", args.entity_id, pins.len()))
        })
        .await
    }

    #[tool(description = "Remove a fact from the pinned session-start brief.")]
    async fn unpin(&self, Parameters(args): Parameters<EntityArgs>) -> String {
        let db = self.db.clone();
        self.blocking(move |_s| {
            let mut found = false;
            crate::ledger::mutate_pins(&db, |mut pins| {
                let before = pins.len();
                pins.retain(|p| p != &args.entity_id);
                found = pins.len() != before;
                pins
            })
            .map_err(|e| format!("error: {e}"))?;
            if !found {
                return Ok(format!("not pinned: {}", args.entity_id));
            }
            Ok(format!("unpinned {}", args.entity_id))
        })
        .await
    }

    #[tool(description = "Move a mis-scoped fact to another project or to \"global\" (sync-safe fact_reprojected event). Memories only - repo chunks are managed by ingest.")]
    async fn reproject(&self, Parameters(args): Parameters<ReprojectArgs>) -> String {
        self.blocking(move |s| {
            if crate::repo::is_chunk_id(&args.entity_id) {
                return Err(format!(
                    "{} is a repo chunk (managed by ingest); reproject applies to memories",
                    args.entity_id
                ));
            }
            if s.get_events_by_entity(&args.entity_id).map_err(|e| format!("error: {e}"))?.is_empty() {
                return Err(format!("unknown entity: {}", args.entity_id));
            }
            let (body, target) = if args.project.eq_ignore_ascii_case("global") {
                (r#"{"project":null}"#.to_string(), "global".to_string())
            } else {
                crate::repo::validate_project_key(&args.project)?;
                (serde_json::json!({ "project": args.project }).to_string(), args.project.clone())
            };
            s.append_event("mcp", "mcp-session", "mcp", EventKind::FactReprojected, &args.entity_id, None, &body)
                .map_err(|e| format!("error: {e}"))?;
            Ok(format!("reprojected {} -> {}", args.entity_id, target))
        })
        .await
    }

    #[tool(description = "What does THOR know here? Live-fact counts (memories vs code chunks, per type), the most recent memories, the pinned standing rules, and any DIVERGED facts needing a resolve. Start-of-task orientation.")]
    async fn brief(&self, Parameters(args): Parameters<BriefArgs>) -> String {
        let server_project = self.project.clone();
        let db = self.db.clone();
        self.blocking(move |s| {
            let project = args.project.or(server_project);
            let events = s.get_all_events().map_err(|e| format!("error: {e}"))?;
            Ok(render_overview(&events, &db, project.as_deref()))
        })
        .await
    }
}

/// Typed-conflict rendering for revise/retract: the agent gets the fresh
/// head-set to retry with, instead of an opaque error.
fn render_mutate_err(e: anyhow::Error) -> String {
    match e.downcast_ref::<MutateConflict>() {
        Some(c) => format!(
            "rejected: {}. Current head-set for {}: {:?}. Re-read the fact (get) and retry with \
             the right parent_rev, or resolve the divergence first.",
            c.reason, c.entity_id, c.current_heads
        ),
        None => format!("error: {e}"),
    }
}

/// The brief tool's body: counts, recent memories, pins, diverged - the
/// orientation an agent needs to decide whether memory is worth interrogating.
fn render_overview(events: &[crate::event_store::Event], db: &Path, project: Option<&str>) -> String {
    use std::collections::HashMap;
    let scope = RecallScope::current(project.map(str::to_string));
    let heads = crate::cas::compute_head_sets(events);
    let projects = crate::cas::compute_projects(events);
    let by_hash: HashMap<&str, &crate::event_store::Event> =
        events.iter().map(|e| (e.this_hash.as_str(), e)).collect();

    let mut memories = 0usize;
    let mut chunks = 0usize;
    let mut typed: HashMap<&'static str, usize> = HashMap::new();
    let mut diverged: Vec<&str> = Vec::new();
    // (entity, max head seq, body, type) for the recent-memories list
    let mut recent: Vec<(&str, i64, &str, Option<crate::repo::FactType>)> = Vec::new();

    for (id, hs) in &heads {
        let effective = projects.get(id).and_then(|o| o.as_deref());
        if !scope.allows(effective) {
            continue;
        }
        let mut live_body: Option<(&str, i64)> = None;
        for rev in &hs.heads {
            if let Some(ev) = by_hash.get(rev.as_str()) {
                if !matches!(ev.kind, EventKind::FactRetracted) {
                    let best = live_body.map(|(_, s)| s).unwrap_or(i64::MIN);
                    if ev.seq > best {
                        live_body = Some((ev.body.as_str(), ev.seq));
                    }
                }
            }
        }
        let (body, seq) = match live_body {
            Some(x) => x,
            None => continue, // fully retracted: not a live fact
        };
        if crate::repo::is_chunk_id(id) {
            chunks += 1;
            continue;
        }
        memories += 1;
        let ty = crate::repo::fact_type(body);
        if let Some(t) = ty {
            *typed.entry(t.as_str()).or_default() += 1;
        }
        if hs.is_diverged {
            diverged.push(id);
        }
        recent.push((id, seq, body, ty));
    }

    recent.sort_by(|a, b| b.1.cmp(&a.1));
    diverged.sort();

    let mut out = format!(
        "THOR brief [project: {}]\nlive facts in scope: {} memories + {} code chunks",
        project.unwrap_or("global"),
        memories,
        chunks
    );
    if !typed.is_empty() {
        let mut t: Vec<_> = typed.into_iter().collect();
        t.sort();
        out.push_str(&format!(
            " ({})",
            t.iter().map(|(k, v)| format!("{}: {}", k, v)).collect::<Vec<_>>().join(", ")
        ));
    }
    out.push('\n');
    if !recent.is_empty() {
        out.push_str("recent memories:\n");
        for (id, _, body, ty) in recent.iter().take(5) {
            let tag = ty.map(|t| format!("[{}] ", t.as_str())).unwrap_or_default();
            out.push_str(&format!("  {}{}: {}\n", tag, id, crate::recall::snippet(body, 120, "")));
        }
    }
    let pins = crate::ledger::read_pins(db);
    if let Some(brief) =
        crate::cli::render_brief(events, &pins, &scope, "brief", project)
    {
        out.push_str(&brief);
        out.push('\n');
    }
    if !diverged.is_empty() {
        out.push_str(&format!(
            "DIVERGED (resolve these): {}\n",
            diverged.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

#[tool_handler]
impl ServerHandler for ThorServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(INSTRUCTIONS)
    }
}

/// Entry point for `thor mcp`. `http = Some(bind)` serves Streamable-HTTP for
/// remote clients (the NAS connector); otherwise stdio (the local connector).
/// Blocking - owns the tokio runtime.
pub fn run_mcp(db: &Path, http: Option<String>) {
    if let Err(e) = run(db, http) {
        eprintln!("thor mcp: fatal: {e}");
    }
}

fn run(db: &Path, http: Option<String>) -> anyhow::Result<()> {
    let shared = Arc::new(Mutex::new(EventStore::new(db)?));
    let db = db.to_path_buf();
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        match http {
            Some(bind) => serve_http(shared, db, &bind).await,
            None => {
                let service = ThorServer::from_shared(shared, startup_project(), db)
                    .serve(rmcp::transport::stdio())
                    .await?;
                service.waiting().await?;
                Ok(())
            }
        }
    })
}

/// Serve the tools over Streamable-HTTP at /mcp. This transport carries NO auth -
/// bind to localhost on the NAS and front it with the Cloudflare Access gate,
/// exactly like mimir's remote MCP.
async fn serve_http(store: Arc<Mutex<EventStore>>, db: PathBuf, bind: &str) -> anyhow::Result<()> {
    // HTTP/remote has no cwd: unscoped by default (a cwd-less remote consumer is
    // cross-project by nature), honoring an explicit `project` arg per call.
    let app = mcp_http_router(move || Ok(ThorServer::from_shared(store.clone(), None, db.clone())));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("thor MCP (streamable-http) listening on http://{bind}/mcp");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the axum router that serves the MCP tools over Streamable-HTTP at /mcp.
fn mcp_http_router<F>(make_server: F) -> axum::Router
where
    F: Fn() -> std::result::Result<ThorServer, std::io::Error> + Send + Sync + 'static,
{
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    let service = StreamableHttpService::new(
        make_server,
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    axum::Router::new().nest_service("/mcp", service)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> EventStore {
        let mut store = EventStore::in_memory().unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "the deploy watcher gotcha")
            .unwrap();
        store
    }

    fn server_with(store: EventStore) -> (ThorServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        (ThorServer::new(store, db), dir)
    }

    fn recall_args(query: &str) -> RecallArgs {
        RecallArgs {
            query: query.into(),
            limit: None,
            all_projects: false,
            project: None,
            kind: None,
            rerank: false,
        }
    }

    #[tokio::test]
    async fn test_recall_rerank_without_model_keeps_fused_order_with_note() {
        // No reranker model in the test environment: the fused hit must still
        // be served, with an honest note instead of an error.
        let (server, _d) = server_with(seed());
        let mut args = recall_args("deploy watcher");
        args.rerank = true;
        let out = server.recall(Parameters(args)).await;
        assert!(out.contains("e1"), "hit still served: {out}");
        assert!(out.contains("rerank skipped") || out.contains("rerank unavailable"),
            "honest degradation note: {out}");
    }

    fn remember_args(body: &str) -> RememberArgs {
        RememberArgs {
            body: body.into(),
            entity_id: None,
            project: None,
            fact_type: None,
            tags: None,
            triggers: None,
            anchors: None,
        }
    }

    #[tokio::test]
    async fn test_recall_tool_returns_hit() {
        let (server, _d) = server_with(seed());
        let out = server.recall(Parameters(recall_args("deploy watcher"))).await;
        assert!(out.contains("e1"), "recall must surface the seeded fact: {out}");
    }

    #[tokio::test]
    async fn test_get_and_recall_serves_count_access_in_ledger() {
        let (server, d) = server_with(seed());
        let db = d.path().join("thor.db");
        // a miss must not seed a phantom counter
        let _ = server.get(Parameters(GetArgs { entity_id: "nope".into() })).await;
        assert_eq!(crate::ledger::counter(&db, "access", "nope"), 0);
        // one get + one recall serve = two accesses
        let _ = server.get(Parameters(GetArgs { entity_id: "e1".into() })).await;
        let out = server.recall(Parameters(recall_args("deploy watcher"))).await;
        assert!(out.contains("e1"), "precondition: recall must serve e1: {out}");
        assert_eq!(crate::ledger::counter(&db, "access", "e1"), 2);
    }

    #[tokio::test]
    async fn test_recall_kind_memory_excludes_chunks() {
        let mut store = seed();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "Proj:src/widget.rs#0", None, "widget widget code chunk")
            .unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "Proj:mem-1", None, "GOTCHA: widget must stay red")
            .unwrap();
        let (server, _d) = server_with(store);
        let mut args = recall_args("widget");
        args.all_projects = true;
        args.kind = Some("memory".into());
        let out = server.recall(Parameters(args)).await;
        assert!(out.contains("Proj:mem-1"), "the memory surfaces: {out}");
        assert!(out.contains("[gotcha]"), "typed tag rendered: {out}");
        assert!(!out.contains("widget.rs#0"), "kind:memory must exclude chunks: {out}");
    }

    #[tokio::test]
    async fn test_remember_then_recall_roundtrip_with_typed_footer() {
        let (server, _d) = server_with(seed());
        let stored = server
            .remember(Parameters(RememberArgs {
                body: "a brand new zephyr fact".into(),
                entity_id: None,
                project: None,
                fact_type: Some("decision".into()),
                tags: Some(vec!["zephyr".into(), "test".into()]),
                triggers: None,
                anchors: None,
            }))
            .await;
        assert!(stored.starts_with("stored entity mcp-"), "got: {stored}");
        let rc = server.recall(Parameters(recall_args("zephyr"))).await;
        assert!(rc.contains("zephyr"), "the new fact must be recallable: {rc}");
        assert!(rc.contains("[decision]"), "the typed footer classifies the fact: {rc}");
    }

    #[tokio::test]
    async fn test_remember_refuses_near_duplicate() {
        let (server, _d) = server_with(seed());
        let out = server
            .remember(Parameters(remember_args("The deploy   watcher GOTCHA")))
            .await;
        assert!(out.contains("NOT stored"), "a near-duplicate must be refused: {out}");
        assert!(out.contains("e1"), "the refusal points at the existing fact: {out}");
        assert!(out.contains("revise"), "the refusal teaches the repair path: {out}");
        // a genuinely different fact still stores
        let ok = server.remember(Parameters(remember_args("an entirely different filament note"))).await;
        assert!(ok.starts_with("stored entity"), "{ok}");
    }

    #[tokio::test]
    async fn test_remember_dup_check_survives_typed_footer_and_respects_scope() {
        let (server, _d) = server_with(seed());
        // store a SHORT typed fact (footer bleeds into a naive 120-char prefix)
        let first = server
            .remember(Parameters(RememberArgs {
                body: "GOTCHA: never open the db over SMB".into(),
                entity_id: None,
                project: None,
                fact_type: Some("gotcha".into()),
                tags: Some(vec!["db".into()]),
                triggers: None,
                anchors: None,
            }))
            .await;
        assert!(first.starts_with("stored entity"), "{first}");
        // the exact same body again: the footer must NOT defeat the refusal
        let dup = server
            .remember(Parameters(RememberArgs {
                body: "GOTCHA: never open the db over SMB".into(),
                entity_id: None,
                project: None,
                fact_type: Some("gotcha".into()),
                tags: None,
                triggers: None,
                anchors: None,
            }))
            .await;
        assert!(dup.contains("NOT stored"), "typed footer must not defeat dup detection: {dup}");
        // ...but the same body deliberately scoped to a PROJECT is a legitimate
        // write (the global fact would surface there anyway, so THAT is refused;
        // a fact living only in ProjA must be storable for ProjB).
        let a = server
            .remember(Parameters(RememberArgs {
                body: "ProjA-only rule: pin the flux version".into(),
                entity_id: None,
                project: Some("ProjA".into()),
                fact_type: None,
                tags: None,
                triggers: None,
                anchors: None,
            }))
            .await;
        assert!(a.starts_with("stored entity"), "{a}");
        let b = server
            .remember(Parameters(RememberArgs {
                body: "ProjA-only rule: pin the flux version".into(),
                entity_id: None,
                project: Some("ProjB".into()),
                fact_type: None,
                tags: None,
                triggers: None,
                anchors: None,
            }))
            .await;
        assert!(
            b.starts_with("stored entity"),
            "the same body for ANOTHER project is not a duplicate (ProjA's fact is out of ProjB's scope): {b}"
        );
    }

    #[tokio::test]
    async fn test_remember_validates_project_key() {
        let (server, _d) = server_with(seed());
        let out = server
            .remember(Parameters(RememberArgs {
                body: "a fact for a malformed key".into(),
                entity_id: None,
                project: Some("acme:widgets".into()),
                fact_type: None,
                tags: None,
                triggers: None,
                anchors: None,
            }))
            .await;
        assert!(
            out.contains("invalid project key"),
            "a ':' in the key would silently mis-scope the fact (owner = prefix before FIRST ':'): {out}"
        );
    }

    #[tokio::test]
    async fn test_remember_refuses_existing_entity_and_chunk_ids() {
        let (server, _d) = server_with(seed());
        // Existing entity: create is never an upsert - a second parentless root
        // would silently ADD a contested head (DIVERGED).
        let out = server
            .remember(Parameters(RememberArgs {
                body: "an entirely different corrected body".into(),
                entity_id: Some("e1".into()),
                project: None,
                fact_type: None,
                tags: None,
                triggers: None,
                anchors: None,
            }))
            .await;
        assert!(out.contains("NOT stored"), "{out}");
        assert!(out.contains("already exists"), "{out}");
        let get = server.get(Parameters(GetArgs { entity_id: "e1".into() })).await;
        assert!(!get.contains("DIVERGED"), "a refused remember must not mint a branch: {get}");
        // Chunk id: managed by ingest alone.
        let chunk = server
            .remember(Parameters(RememberArgs {
                body: "text for a chunk".into(),
                entity_id: Some("Proj:src/a.rs#0".into()),
                project: None,
                fact_type: None,
                tags: None,
                triggers: None,
                anchors: None,
            }))
            .await;
        assert!(chunk.contains("chunk"), "{chunk}");
    }

    #[tokio::test]
    async fn test_remember_rejects_empty_body() {
        let (server, _d) = server_with(seed());
        let out = server.remember(Parameters(remember_args("   "))).await;
        assert!(out.contains("non-empty"), "empty body must be rejected: {out}");
    }

    #[tokio::test]
    async fn test_revise_lifecycle_and_stale_parent_conflict() {
        let (server, _d) = server_with(seed());
        // revise with auto-filled parent (single head)
        let out = server
            .revise(Parameters(ReviseArgs {
                entity_id: "e1".into(),
                body: "the deploy watcher gotcha, updated".into(),
                parent_rev: None,
            }))
            .await;
        assert!(out.starts_with("revised e1"), "{out}");
        // the old text is no longer a head; the new one recalls
        let rc = server.recall(Parameters(recall_args("deploy watcher"))).await;
        assert!(rc.contains("updated"), "{rc}");
        // a stale parent_rev is rejected with the fresh head-set, never a branch
        let stale = server
            .revise(Parameters(ReviseArgs {
                entity_id: "e1".into(),
                body: "racing write".into(),
                parent_rev: Some("0000000000000000000000000000000000000000000000000000000000000000".into()),
            }))
            .await;
        assert!(stale.contains("rejected"), "stale parent must conflict: {stale}");
        assert!(stale.contains("head-set"), "the fresh head-set is returned: {stale}");
        let get = server.get(Parameters(GetArgs { entity_id: "e1".into() })).await;
        assert!(!get.contains("DIVERGED"), "a rejected revise must not mint a branch: {get}");
        // revising an unknown entity errors (never creates a stray head)
        let err = server
            .revise(Parameters(ReviseArgs { entity_id: "nope".into(), body: "x".into(), parent_rev: None }))
            .await;
        assert!(err.contains("unknown entity"), "got: {err}");
    }

    #[tokio::test]
    async fn test_retract_hides_from_recall_keeps_history() {
        let (server, _d) = server_with(seed());
        let out = server
            .retract(Parameters(RetractArgs { entity_id: "e1".into(), parent_rev: None, reason: Some("obsolete".into()) }))
            .await;
        assert!(out.starts_with("retracted e1"), "{out}");
        let rc = server.recall(Parameters(recall_args("deploy watcher"))).await;
        assert!(rc.contains("No THOR hits"), "a retracted fact must stop surfacing: {rc}");
        let hist = server.history(Parameters(EntityArgs { entity_id: "e1".into() })).await;
        assert!(hist.contains("fact_retracted"), "history keeps the full trail: {hist}");
        assert!(hist.contains("fact_created"), "{hist}");
    }

    #[tokio::test]
    async fn test_resolve_settles_diverged_entity() {
        let mut store = seed();
        // force a divergence via the raw primitive
        store
            .append_event("s", "l", "a", EventKind::FactRevised, "e1", Some("stale-parent"), "branch B")
            .unwrap();
        let heads: Vec<String> = {
            let events = store.get_all_events().unwrap();
            let mut h: Vec<String> =
                crate::cas::compute_head_sets(&events)["e1"].heads.iter().cloned().collect();
            h.sort();
            h
        };
        assert_eq!(heads.len(), 2, "sanity: diverged");
        let (server, _d) = server_with(store);
        let out = server
            .resolve(Parameters(ResolveArgs { entity_id: "e1".into(), keep_rev: heads[0].clone() }))
            .await;
        assert!(out.starts_with("resolved e1"), "{out}");
        let get = server.get(Parameters(GetArgs { entity_id: "e1".into() })).await;
        assert!(!get.contains("DIVERGED"), "resolve must settle the entity: {get}");
    }

    #[tokio::test]
    async fn test_mark_appends_echo_and_validates_entity() {
        let (server, d) = server_with(seed());
        let out = server.mark(Parameters(MarkArgs { entity_id: "e1".into(), noise: false })).await;
        assert!(out.contains("marked e1"), "{out}");
        let hist = server.history(Parameters(EntityArgs { entity_id: "e1".into() })).await;
        assert!(hist.contains("fact_echoed"), "the echo lands in the log: {hist}");
        let bad = server.mark(Parameters(MarkArgs { entity_id: "nope".into(), noise: false })).await;
        assert!(bad.contains("unknown entity"), "{bad}");

        // noise: LOCAL ledger counter, never a log event
        let db = d.path().join("thor.db");
        let out = server.mark(Parameters(MarkArgs { entity_id: "e1".into(), noise: true })).await;
        assert!(out.contains("noise"), "{out}");
        assert_eq!(crate::ledger::counter(&db, "noise", "e1"), 1);
        let hist = server.history(Parameters(EntityArgs { entity_id: "e1".into() })).await;
        assert_eq!(hist.matches("fact_echoed").count(), 1, "noise never lands in the synced log");
        let bad = server.mark(Parameters(MarkArgs { entity_id: "nope".into(), noise: true })).await;
        assert!(bad.contains("unknown entity"), "noise mark validates existence too: {bad}");
    }

    #[tokio::test]
    async fn test_pin_unpin_and_brief() {
        let mut store = seed();
        store
            .append_event(
                "s", "l", "a", EventKind::FactCreated, "rule-1", None,
                "HARDE REGEL: never force-recreate on prod",
            )
            .unwrap();
        let (server, _d) = server_with(store);
        let out = server.pin(Parameters(EntityArgs { entity_id: "rule-1".into() })).await;
        assert!(out.starts_with("pinned rule-1"), "{out}");
        let brief = server.brief(Parameters(BriefArgs { project: None })).await;
        assert!(brief.contains("2 memories"), "counts live facts: {brief}");
        assert!(brief.contains("<thor-brief>"), "pinned block present: {brief}");
        assert!(brief.contains("never force-recreate on prod"), "full pinned body: {brief}");
        assert!(brief.contains("[preference]"), "typed count/tag: {brief}");
        let out = server.unpin(Parameters(EntityArgs { entity_id: "rule-1".into() })).await;
        assert!(out.starts_with("unpinned"), "{out}");
        let brief = server.brief(Parameters(BriefArgs { project: None })).await;
        assert!(!brief.contains("<thor-brief>"), "unpinned -> no pinned block: {brief}");
    }

    #[tokio::test]
    async fn test_reproject_moves_scope_and_refuses_chunks() {
        let (server, _d) = server_with(seed());
        let out = server
            .reproject(Parameters(ReprojectArgs { entity_id: "e1".into(), project: "ProjA".into() }))
            .await;
        assert!(out.contains("e1 -> ProjA"), "{out}");
        // now scoped: visible in ProjA, hidden in ProjB
        let mut in_a = recall_args("deploy watcher");
        in_a.project = Some("ProjA".into());
        assert!(server.recall(Parameters(in_a)).await.contains("e1"));
        let mut in_b = recall_args("deploy watcher");
        in_b.project = Some("ProjB".into());
        assert!(server.recall(Parameters(in_b)).await.contains("No THOR hits"));
        // chunks are refused
        let chunk = server
            .reproject(Parameters(ReprojectArgs { entity_id: "P:src/a.rs#0".into(), project: "X".into() }))
            .await;
        assert!(chunk.contains("chunk"), "{chunk}");
    }

    #[tokio::test]
    async fn test_http_router_builds() {
        // the Streamable-HTTP transport wires up without a live socket
        let _app = mcp_http_router(|| {
            Ok(ThorServer::new(EventStore::in_memory().unwrap(), PathBuf::from("thor-test.db")))
        });
    }
}
