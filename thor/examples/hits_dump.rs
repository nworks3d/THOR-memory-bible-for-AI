//! Hit-dump harness for the THOR-vs-mimir benchmark: runs a query battery
//! through a REAL production recall path and writes the returned hits as JSON,
//! ready for a blind judge pass (see BENCHMARKS.md, Method). Reproducible: the
//! published numbers come from exactly these channels, never a re-implementation.
//!
//! Input: a JSON array of objects; the query text is taken from the first of
//! `--query-field`, "query", "q", or "drift_prompt" that is present. All other
//! fields pass through to the output, plus a "hits" array of strings.
//!
//!   cargo run --release --features semantic --example hits_dump -- \
//!     --queries <in.json> --out <out.json> \
//!     [--limit 5] [--scope all|global|project:<key>] [--full] \
//!     [--channel fused|courier] [--cwd <dir>]
//!
//! Channels:
//! - `fused` (default): the deliberate-recall path (`recall_fused_scoped`,
//!   what MCP recall serves) with the given scope; `--full` emits full bodies
//!   (the multi-project test judges full chunks), else 500-char snippets.
//! - `courier`: the as-deployed auto-injection path (the UserPromptSubmit
//!   hook), scoped by `--cwd` exactly like a live session; each query gets a
//!   fresh session id so the suppression ledger never carries over. The hits
//!   array holds the raw injection block (or is empty when the courier stays
//!   silent - silence is a result, not an error).

use std::collections::HashMap;
use std::path::PathBuf;
use thor::event_store::EventStore;
use thor::recall::RecallScope;

fn main() -> anyhow::Result<()> {
    let mut queries_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut query_field: Option<String> = None;
    let mut limit: usize = 5;
    let mut scope_arg = "all".to_string();
    let mut full = false;
    let mut channel = "fused".to_string();
    let mut cwd: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--queries" => queries_path = args.next().map(PathBuf::from),
            "--out" => out_path = args.next().map(PathBuf::from),
            "--query-field" => query_field = args.next(),
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(5),
            "--scope" => scope_arg = args.next().unwrap_or_else(|| "all".into()),
            "--full" => full = true,
            "--channel" => channel = args.next().unwrap_or_else(|| "fused".into()),
            "--cwd" => cwd = args.next(),
            other => anyhow::bail!("unknown argument '{}'", other),
        }
    }
    let queries_path = queries_path.ok_or_else(|| anyhow::anyhow!("--queries is required"))?;
    let out_path = out_path.ok_or_else(|| anyhow::anyhow!("--out is required"))?;

    let db = thor::ledger::data_dir()
        .ok_or_else(|| anyhow::anyhow!("no data dir (LOCALAPPDATA/XDG_DATA_HOME/HOME unset)"))?
        .join("thor.db");

    let items: Vec<serde_json::Value> =
        serde_json::from_reader(std::fs::File::open(&queries_path)?)?;
    let scope = match scope_arg.as_str() {
        "all" => RecallScope::everything(),
        "global" => RecallScope::current(None),
        s => match s.strip_prefix("project:") {
            Some(key) => RecallScope::current(Some(key.to_string())),
            None => anyhow::bail!("--scope must be all, global, or project:<key>"),
        },
    };

    let store = EventStore::new(&db)?;
    #[cfg(feature = "semantic")]
    let mut embedder = thor::embed::Embedder::load_default().ok();
    #[cfg_attr(not(feature = "semantic"), allow(unused_variables))]
    let vecs = {
        #[cfg(feature = "semantic")]
        {
            thor::vectors::VectorStore::open(&thor::vectors::default_vectors_path(&db)).ok()
        }
        #[cfg(not(feature = "semantic"))]
        {
            Option::<()>::None
        }
    };

    let mut out: Vec<serde_json::Value> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let mut fields: Vec<&str> = Vec::new();
        if let Some(f) = query_field.as_deref() {
            fields.push(f);
        }
        fields.extend(["query", "q", "drift_prompt"]);
        let query = fields
            .iter()
            .find_map(|f| item.get(*f).and_then(|v| v.as_str()))
            .unwrap_or("");
        if query.is_empty() {
            continue;
        }
        let hits: Vec<String> = match channel.as_str() {
            "courier" => {
                // Session id unique PER PROCESS RUN: a fixed id would let the
                // suppression ledger of a previous harness run rotate this
                // run's injections (measured: it silently halved coverage on a
                // re-run) - every invocation must look like a fresh session.
                let hook = serde_json::json!({
                    "prompt": query,
                    "session_id": format!("bench-{}-{}", std::process::id(), i),
                    "cwd": cwd,
                });
                thor::courier::injection_for_hook_json(&db, &hook.to_string())
                    .map(|block| vec![block])
                    .unwrap_or_default()
            }
            _ => {
                let hits = {
                    #[cfg(feature = "semantic")]
                    {
                        match (embedder.as_mut(), vecs.as_ref()) {
                            (Some(e), Some(v)) => {
                                let qvec = e.embed_one(query)?;
                                thor::recall::recall_fused_scoped(
                                    &store,
                                    query,
                                    &qvec,
                                    v,
                                    limit,
                                    thor::recall::FUSION_LAMBDA,
                                    &scope,
                                )?
                            }
                            _ => thor::recall::recall_scoped(&store, query, limit, &scope)?,
                        }
                    }
                    #[cfg(not(feature = "semantic"))]
                    {
                        thor::recall::recall_scoped(&store, query, limit, &scope)?
                    }
                };
                hits.iter()
                    .map(|h| {
                        if full {
                            format!("{}\n{}", h.entity_id, h.body)
                        } else {
                            format!("{}: {}", h.entity_id, thor::recall::snippet(&h.body, 500, query))
                        }
                    })
                    .collect()
            }
        };
        let mut row: HashMap<String, serde_json::Value> = item
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        row.insert("hits".to_string(), serde_json::json!(hits));
        out.push(serde_json::json!(row));
        if (i + 1) % 25 == 0 {
            eprintln!("{}/{}", i + 1, items.len());
        }
    }

    std::fs::write(&out_path, serde_json::to_string_pretty(&out)?)?;
    eprintln!("wrote {} rows to {}", out.len(), out_path.display());
    Ok(())
}
