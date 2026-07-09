//! THOR MCP server. Serves recall/get/remember over MCP, either on stdio
//! (`thor mcp`, the local connector) or Streamable-HTTP (`thor mcp --http
//! <bind>`, the remote connector deployed on the NAS - THOR's own server, port
//! and DB, fully independent of mimir). Built on rmcp (mimir's proven recipe).
//!
//! The store is sync (rusqlite); every tool hops through spawn_blocking so the
//! async transport is never blocked. One shared store behind a Mutex serves all
//! HTTP sessions (WAL + the Mutex handle concurrency).

use crate::cli::render_get;
use crate::event_store::{EventKind, EventStore};
use crate::recall::{recall_scoped, RecallScope};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const INSTRUCTIONS: &str = "THOR is the user's lossless memory. recall searches the current-head \
facts (read-only), SCOPED to the current project + the global tier - pass all_projects:true to \
search every project, or project:\"<key>\" for a specific one. get shows one entity's head(s); \
remember stores a new fact (scoped to the current project by default; pass project:\"global\" for \
a cross-project fact). Recall before remembering to avoid duplicates.";

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
}

#[tool_router]
impl ThorServer {
    pub fn new(store: EventStore) -> Self {
        Self::from_shared(Arc::new(Mutex::new(store)), None)
    }

    pub fn from_shared(store: Arc<Mutex<EventStore>>, project: Option<String>) -> Self {
        ThorServer { store, project, tool_router: Self::tool_router() }
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

    #[tool(description = "Search THOR memory and return the best-matching CURRENT-HEAD facts, ranked. Read-only.")]
    async fn recall(&self, Parameters(args): Parameters<RecallArgs>) -> String {
        let server_project = self.project.clone();
        self.blocking(move |s| {
            // Scope: all_projects > explicit project > the server's current project.
            let scope = if args.all_projects {
                RecallScope::everything()
            } else if args.project.is_some() {
                RecallScope::current(args.project.clone())
            } else {
                RecallScope::current(server_project.clone())
            };
            let hits = recall_scoped(s, &args.query, args.limit.unwrap_or(8), &scope)
                .map_err(|e| format!("error: {e}"))?;
            if hits.is_empty() {
                return Ok(format!("No THOR hits for: {}", args.query));
            }
            let mut out = String::new();
            for hit in hits {
                let short = &hit.rev[..hit.rev.len().min(8)];
                let snip = crate::recall::snippet(&hit.body, 220, &args.query);
                let diverged = if hit.is_diverged { " [DIVERGED]" } else { "" };
                let tag = if crate::repo::is_global(hit.project.as_deref()) {
                    "[global]".to_string()
                } else {
                    format!("[proj:{}]", hit.project.as_deref().unwrap_or("?"))
                };
                out.push_str(&format!("{} {} ({}{}): {}\n", tag, hit.entity_id, short, diverged, snip));
            }
            Ok(out)
        })
        .await
    }

    #[tool(description = "Show the current head(s) of one THOR entity by id (DIVERGED shows every contested head).")]
    async fn get(&self, Parameters(args): Parameters<GetArgs>) -> String {
        self.blocking(move |s| {
            let events = s.get_all_events().map_err(|e| format!("error: {e}"))?;
            Ok(render_get(&args.entity_id, &events))
        })
        .await
    }

    #[tool(description = "Store a new fact in THOR as a fact_created. Returns the entity id and rev.")]
    async fn remember(&self, Parameters(args): Parameters<RememberArgs>) -> String {
        let server_project = self.project.clone();
        self.blocking(move |s| {
            let body = args.body.trim();
            if body.is_empty() {
                return Err("a non-empty 'body' is required".to_string());
            }
            let entity_id = match args.entity_id.filter(|s| !s.is_empty()) {
                // A caller-supplied id is used verbatim (its prefix defines scope).
                Some(id) => id,
                None => {
                    // Mint scope: explicit arg ("global" -> global) else the server's
                    // project. A scoped memory is `<key>:mem-<uuid>`, global `mcp-<uuid>`.
                    let mint = match args.project {
                        Some(p) if p.eq_ignore_ascii_case("global") => None,
                        Some(p) => Some(p),
                        None => server_project,
                    };
                    crate::repo::memory_entity_id(mint.as_deref(), &Uuid::new_v4().to_string())
                }
            };
            let ev = s
                .append_event("mcp", "mcp-session", "mcp", EventKind::FactCreated, &entity_id, None, body)
                .map_err(|e| format!("error: {e}"))?;
            Ok(format!("stored entity {} rev {}", entity_id, ev.this_hash))
        })
        .await
    }
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
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        match http {
            Some(bind) => serve_http(shared, &bind).await,
            None => {
                let service =
                    ThorServer::from_shared(shared, startup_project()).serve(rmcp::transport::stdio()).await?;
                service.waiting().await?;
                Ok(())
            }
        }
    })
}

/// Serve the tools over Streamable-HTTP at /mcp. This transport carries NO auth -
/// bind to localhost on the NAS and front it with the Cloudflare Access gate,
/// exactly like mimir's remote MCP.
async fn serve_http(store: Arc<Mutex<EventStore>>, bind: &str) -> anyhow::Result<()> {
    // HTTP/remote has no cwd: unscoped by default (a cwd-less remote consumer is
    // cross-project by nature), honoring an explicit `project` arg per call.
    let app = mcp_http_router(move || Ok(ThorServer::from_shared(store.clone(), None)));
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

    #[tokio::test]
    async fn test_recall_tool_returns_hit() {
        let server = ThorServer::new(seed());
        let out = server
            .recall(Parameters(RecallArgs { query: "deploy watcher".into(), limit: None, all_projects: false, project: None }))
            .await;
        assert!(out.contains("e1"), "recall must surface the seeded fact: {out}");
    }

    #[tokio::test]
    async fn test_remember_then_recall_roundtrip() {
        let server = ThorServer::new(seed());
        let stored = server
            .remember(Parameters(RememberArgs { body: "a brand new zephyr fact".into(), entity_id: None, project: None }))
            .await;
        assert!(stored.starts_with("stored entity mcp-"), "got: {stored}");
        let rc = server
            .recall(Parameters(RecallArgs { query: "zephyr".into(), limit: None, all_projects: false, project: None }))
            .await;
        assert!(rc.contains("zephyr"), "the new fact must be recallable: {rc}");
    }

    #[tokio::test]
    async fn test_remember_rejects_empty_body() {
        let server = ThorServer::new(seed());
        let out = server
            .remember(Parameters(RememberArgs { body: "   ".into(), entity_id: None, project: None }))
            .await;
        assert!(out.contains("non-empty"), "empty body must be rejected: {out}");
    }

    #[tokio::test]
    async fn test_http_router_builds() {
        // the Streamable-HTTP transport wires up without a live socket
        let _app = mcp_http_router(|| Ok(ThorServer::new(EventStore::in_memory().unwrap())));
    }
}
