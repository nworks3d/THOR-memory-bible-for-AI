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
use crate::recall::recall;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const INSTRUCTIONS: &str = "THOR is the user's lossless memory. recall searches the current-head \
facts (read-only); get shows one entity's head(s); remember stores a new fact. Recall before \
remembering to avoid duplicates.";

#[derive(Clone)]
pub struct ThorServer {
    store: Arc<Mutex<EventStore>>,
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
}

#[tool_router]
impl ThorServer {
    pub fn new(store: EventStore) -> Self {
        Self::from_shared(Arc::new(Mutex::new(store)))
    }

    pub fn from_shared(store: Arc<Mutex<EventStore>>) -> Self {
        ThorServer { store, tool_router: Self::tool_router() }
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
        self.blocking(move |s| {
            let hits = recall(s, &args.query, args.limit.unwrap_or(8)).map_err(|e| format!("error: {e}"))?;
            if hits.is_empty() {
                return Ok(format!("No THOR hits for: {}", args.query));
            }
            let mut out = String::new();
            for hit in hits {
                let short = &hit.rev[..hit.rev.len().min(8)];
                let snip = crate::recall::snippet(&hit.body, 220, &args.query);
                let diverged = if hit.is_diverged { " [DIVERGED]" } else { "" };
                out.push_str(&format!("{} ({}{}): {}\n", hit.entity_id, short, diverged, snip));
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
        self.blocking(move |s| {
            let body = args.body.trim();
            if body.is_empty() {
                return Err("a non-empty 'body' is required".to_string());
            }
            let entity_id = args
                .entity_id
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("mcp-{}", Uuid::new_v4()));
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
                let service = ThorServer::from_shared(shared).serve(rmcp::transport::stdio()).await?;
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
    let app = mcp_http_router(move || Ok(ThorServer::from_shared(store.clone())));
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
            .recall(Parameters(RecallArgs { query: "deploy watcher".into(), limit: None }))
            .await;
        assert!(out.contains("e1"), "recall must surface the seeded fact: {out}");
    }

    #[tokio::test]
    async fn test_remember_then_recall_roundtrip() {
        let server = ThorServer::new(seed());
        let stored = server
            .remember(Parameters(RememberArgs { body: "a brand new zephyr fact".into(), entity_id: None }))
            .await;
        assert!(stored.starts_with("stored entity mcp-"), "got: {stored}");
        let rc = server
            .recall(Parameters(RecallArgs { query: "zephyr".into(), limit: None }))
            .await;
        assert!(rc.contains("zephyr"), "the new fact must be recallable: {rc}");
    }

    #[tokio::test]
    async fn test_remember_rejects_empty_body() {
        let server = ThorServer::new(seed());
        let out = server
            .remember(Parameters(RememberArgs { body: "   ".into(), entity_id: None }))
            .await;
        assert!(out.contains("non-empty"), "empty body must be rejected: {out}");
    }

    #[tokio::test]
    async fn test_http_router_builds() {
        // the Streamable-HTTP transport wires up without a live socket
        let _app = mcp_http_router(|| Ok(ThorServer::new(EventStore::in_memory().unwrap())));
    }
}
