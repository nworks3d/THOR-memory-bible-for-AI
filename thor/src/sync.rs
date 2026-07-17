//! Log-shipping HTTP transport (sync Slice 2): a bearer-gated receiver plus a
//! client shipper, built on the pure protocol core in event_store.rs
//! (ingest_batch / contiguous_tip / events_since).
//!
//! Topology (design 06, step 7): one authority (the NAS hub) ships its
//! append-only log to an opportunistic replica (the PC). The RECEIVER runs where
//! the replica lives (`thor recv`); the SHIPPER pushes a local store's backlog to
//! a remote receiver (`thor ship --to <url>`). Every endpoint requires the shared
//! bearer token (constant-time compared) - the transport carries no other auth,
//! so an unauthenticated request gets 401. The token proves ORIGIN; the per-event
//! prev_hash replay in ingest_batch proves integrity + continuity. Bind the
//! receiver to the LAN/tailnet (or front it with the same Cloudflare Access gate
//! as the MCP endpoint); the token is the sole transport-level gate otherwise.

use crate::event_store::{Event, EventStore, IngestOutcome, ShippedEvent};
use crate::inbox::InboxOp;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default UPPER BOUND on events per shipment batch (bytes also cap it, below).
pub const SHIP_BATCH: usize = 256;

/// Serialized-byte budget the shipper packs into one batch. Kept well under
/// MAX_BODY_BYTES so a batch built to the budget (plus JSON framing) always fits
/// the receiver. Count (SHIP_BATCH) and bytes are BOTH upper bounds; a lone event
/// larger than the budget still ships alone (bounded then by MAX_BODY_BYTES).
const SHIP_BYTE_BUDGET: usize = 4 * 1024 * 1024;

/// The receiver's request-body limit (axum DefaultBodyLimit). Above the shipper's
/// byte budget so a full budgeted batch fits with headroom, and a single large
/// memory still gets through. Shipper and receiver share this ONE number: a body
/// over it is a clear 413, never a silent stall.
const MAX_BODY_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone)]
struct HubState {
    store: Arc<Mutex<EventStore>>,
    token: Arc<String>,
    /// When set (a replica with THOR_CAPTURE_INBOX), the /inbox routes rotate and
    /// serve captures from this file so the authority can drain them over HTTP.
    inbox: Option<PathBuf>,
}

/// The receiver's resume cursor: its hole-proof contiguous tip.
#[derive(Serialize, Deserialize)]
pub struct CursorResponse {
    pub contiguous_seq: i64,
    pub tip_hash: String,
}

/// A shipment: a batch of events in chain order (the shipper sends the backlog
/// past the receiver's cursor).
#[derive(Serialize, Deserialize)]
pub struct ShipRequest {
    pub events: Vec<ShippedEvent>,
}

/// The receiver's answer to a shipment. `status` = "applied" (HTTP 200) or
/// "rejected" (HTTP 409); `contiguous_seq`/`tip_hash` are the receiver's tip
/// after the call, which the shipper uses as its next cursor.
#[derive(Serialize, Deserialize)]
pub struct ShipResponse {
    pub status: String,
    #[serde(default)]
    pub applied: usize,
    #[serde(default)]
    pub skipped: usize,
    pub contiguous_seq: i64,
    pub tip_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_seq: Option<i64>,
}

/// Constant-time byte compare, so a wrong token cannot be recovered from
/// early-exit timing. The length check leaks only the token LENGTH (not secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// True iff the request carries `Authorization: Bearer <token>` matching `token`.
/// The scheme is matched case-insensitively (RFC 7235); an empty configured token
/// NEVER authorizes (defense in depth - run_recv already refuses to start on an
/// empty token, but a blank token must not become a skeleton key even so).
fn authorized(headers: &HeaderMap, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, tok)| tok);
    match presented {
        Some(p) => ct_eq(p.as_bytes(), token.as_bytes()),
        None => false,
    }
}

async fn cursor_handler(
    State(st): State<HubState>,
    headers: HeaderMap,
) -> Result<Json<CursorResponse>, StatusCode> {
    if !authorized(&headers, st.token.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let store = st.store.clone();
    let (contiguous_seq, tip_hash) = tokio::task::spawn_blocking(move || {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.contiguous_tip()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(CursorResponse { contiguous_seq, tip_hash }))
}

async fn append_handler(
    State(st): State<HubState>,
    headers: HeaderMap,
    Json(req): Json<ShipRequest>,
) -> Result<(StatusCode, Json<ShipResponse>), StatusCode> {
    if !authorized(&headers, st.token.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let store = st.store.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.ingest_batch(&req.events)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (code, resp) = match outcome {
        IngestOutcome::Applied { applied, skipped, contiguous_seq, tip_hash } => (
            StatusCode::OK,
            ShipResponse {
                status: "applied".to_string(),
                applied,
                skipped,
                contiguous_seq,
                tip_hash,
                reason: None,
                at_seq: None,
            },
        ),
        IngestOutcome::Rejected { reason, at_seq, contiguous_seq, tip_hash } => (
            StatusCode::CONFLICT,
            ShipResponse {
                status: "rejected".to_string(),
                applied: 0,
                skipped: 0,
                contiguous_seq,
                tip_hash,
                reason: Some(reason),
                at_seq: Some(at_seq),
            },
        ),
    };
    Ok((code, Json(resp)))
}

/// The draining slot next to the inbox: the pull rotates `inbox.jsonl` into
/// `inbox.jsonl.draining` (an atomic rename) so new captures land in a fresh file
/// while this frozen batch is served and, once the authority applied it, acked.
fn draining_path(inbox: &Path) -> PathBuf {
    let mut s = inbox.as_os_str().to_owned();
    s.push(".draining");
    PathBuf::from(s)
}

/// Rotate (once) and read the pending captures. If a prior batch was pulled but
/// never acked, the draining file still exists - re-serve it (at-least-once; the
/// authority's apply is idempotent on create). Empty when there are no captures.
fn rotate_and_read(inbox: &Path) -> anyhow::Result<Vec<InboxOp>> {
    let draining = draining_path(inbox);
    if !draining.exists() {
        if inbox.exists() {
            std::fs::rename(inbox, &draining)?;
        } else {
            return Ok(Vec::new());
        }
    }
    crate::inbox::read_all(&draining)
}

/// GET /inbox/pull - serve (and rotate) the replica's pending captures so the
/// authority can replay them. Bearer-gated. Empty when the server has no inbox.
async fn inbox_pull_handler(
    State(st): State<HubState>,
    headers: HeaderMap,
) -> Result<Json<Vec<InboxOp>>, StatusCode> {
    if !authorized(&headers, st.token.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(inbox) = st.inbox.clone() else {
        return Ok(Json(Vec::new()));
    };
    let ops = tokio::task::spawn_blocking(move || rotate_and_read(&inbox))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ops))
}

/// POST /inbox/ack - the authority applied the pulled batch; drop the draining
/// file so it is not served again. Bearer-gated. A no-op without an inbox.
async fn inbox_ack_handler(
    State(st): State<HubState>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    if !authorized(&headers, st.token.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if let Some(inbox) = st.inbox.clone() {
        let draining = draining_path(&inbox);
        tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_file(&draining);
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(StatusCode::OK)
}

/// The receiver's axum router: GET /ship/cursor + POST /ship/append, plus the
/// bearer-gated GET /inbox/pull + POST /inbox/ack capture-inbox drain routes.
/// Shared with the integration tests (no live socket needed).
fn ship_router(store: Arc<Mutex<EventStore>>, token: String, inbox: Option<PathBuf>) -> Router {
    let state = HubState { store, token: Arc::new(token), inbox };
    Router::new()
        .route("/ship/cursor", get(cursor_handler))
        .route("/ship/append", post(append_handler))
        .route("/inbox/pull", get(inbox_pull_handler))
        .route("/inbox/ack", post(inbox_ack_handler))
        // Explicit, shared body limit: a shipment over this is a clear 413, and
        // the shipper's SHIP_BYTE_BUDGET is set below it so budgeted batches fit.
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// The token, required from the environment. Refuse to run without it so an
/// endpoint is never accidentally opened with no auth in front of it.
fn require_token() -> anyhow::Result<String> {
    let t = std::env::var("THOR_TOKEN").unwrap_or_default();
    if t.trim().is_empty() {
        anyhow::bail!(
            "THOR_TOKEN is not set - the sync transport has no other auth; refusing to open an unauthenticated endpoint"
        );
    }
    Ok(t)
}

/// Run the receiver on `bind` (e.g. 0.0.0.0:5555). Blocking - owns a tokio
/// runtime. Ingests shipped batches into the store at `db`.
pub fn run_recv(db: &Path, bind: &str) -> anyhow::Result<()> {
    let token = require_token()?;
    // On a replica the mobile MCP diverts writes to THOR_CAPTURE_INBOX; the
    // /inbox routes let the authority drain them over the same bearer channel.
    let inbox = std::env::var("THOR_CAPTURE_INBOX")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let store = Arc::new(Mutex::new(EventStore::new(db)?));
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let app = ship_router(store, token, inbox);
        let listener = tokio::net::TcpListener::bind(bind).await?;
        println!("thor sync receiver listening on http://{bind}/ship (bearer-gated)");
        axum::serve(listener, app).await?;
        Ok::<(), anyhow::Error>(())
    })
}

/// Summary of a completed push.
pub struct PushSummary {
    pub applied: usize,
    pub batches: usize,
    pub final_cursor: i64,
}

/// How many events from the front of `events` fit in one shipment: at most
/// `count_cap`, and stopping before the serialized size would exceed
/// `byte_budget` - but ALWAYS at least 1, so a lone event larger than the budget
/// still ships (bounded by the receiver's MAX_BODY_BYTES) instead of stalling
/// replication forever. Count and bytes are both upper bounds.
fn plan_batch(events: &[Event], count_cap: usize, byte_budget: usize) -> usize {
    let mut n = 0usize;
    let mut bytes = 0usize;
    for e in events.iter().take(count_cap.max(1)) {
        let sz = serde_json::to_string(&e.to_shipped()).map(|s| s.len()).unwrap_or(0) + 2;
        if n > 0 && bytes + sz > byte_budget {
            break;
        }
        bytes += sz;
        n += 1;
    }
    n.max(1)
}

fn build_client() -> anyhow::Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?)
}

/// GET the receiver's hole-proof contiguous cursor. Shared by push_to and status.
fn get_cursor(client: &reqwest::blocking::Client, base: &str, auth: &str) -> anyhow::Result<i64> {
    let resp = client
        .get(format!("{base}/ship/cursor"))
        .header(header::AUTHORIZATION, auth)
        .send()?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("receiver rejected the token (401) - THOR_TOKEN must match on both sides");
    }
    Ok(resp.error_for_status()?.json::<CursorResponse>()?.contiguous_seq)
}

/// Pull (and rotate) a replica's pending captures over the bearer channel. The
/// authority applies them, then calls `ack_inbox` so they are not served again.
pub fn pull_inbox(base: &str, token: &str) -> anyhow::Result<Vec<InboxOp>> {
    let base = base.trim_end_matches('/');
    let client = build_client()?;
    let resp = client
        .get(format!("{base}/inbox/pull"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .send()?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("receiver rejected the token (401) - THOR_TOKEN must match on both sides");
    }
    Ok(resp.error_for_status()?.json::<Vec<InboxOp>>()?)
}

/// Ack a drained batch: the replica drops its draining file so it is not re-served.
pub fn ack_inbox(base: &str, token: &str) -> anyhow::Result<()> {
    let base = base.trim_end_matches('/');
    let client = build_client()?;
    let resp = client
        .post(format!("{base}/inbox/ack"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .send()?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("receiver rejected the token (401) - THOR_TOKEN must match on both sides");
    }
    resp.error_for_status()?;
    Ok(())
}

fn now_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn short(h: &str) -> &str {
    &h[..h.len().min(8)]
}

/// Push a local store's backlog to a remote receiver at `base` (e.g.
/// http://replica:5555). Reads the remote's contiguous cursor, then ships
/// events_since(cursor) in `batch`-sized chunks, advancing on each 200 and
/// resuming from the receiver's returned tip on a 409. Bails if a 409 does not
/// move the cursor forward (a fork or hole shipping cannot heal) or on 401.
pub fn push_to(store: &EventStore, base: &str, token: &str, batch: usize) -> anyhow::Result<PushSummary> {
    let base = base.trim_end_matches('/');
    let batch = batch.max(1);
    let client = build_client()?;
    let auth = format!("Bearer {token}");

    // 1. Where is the receiver? Its hole-proof contiguous cursor.
    let mut cursor = get_cursor(&client, base, &auth)?;

    // 2. Ship the backlog past the cursor, batch by batch.
    let mut total_applied = 0usize;
    let mut batches = 0usize;
    loop {
        let pending = store.events_since(cursor)?;
        if pending.is_empty() {
            break;
        }
        // Our own tip bounds any cursor the receiver reports: in the single-
        // authority model the receiver's log is a prefix of ours, so a cursor
        // past our tip means it is not our replica (or is lying) - never trust it
        // to skip our events.
        let our_tip = store.get_next_seq()? - 1;
        let take = plan_batch(&pending, batch, SHIP_BYTE_BUDGET);
        let chunk: Vec<ShippedEvent> = pending[..take].iter().map(|e| e.to_shipped()).collect();

        let resp = client
            .post(format!("{base}/ship/append"))
            .header(header::AUTHORIZATION, &auth)
            .json(&ShipRequest { events: chunk })
            .send()?;
        batches += 1;
        let status = resp.status();

        if status == reqwest::StatusCode::OK {
            let body: ShipResponse = resp.json()?;
            total_applied += body.applied;
            // Progress is defined SOLELY by the cursor strictly advancing (and
            // staying within our own log). A conforming receiver that applied
            // anything always advances its contiguous tip, so a 200 that does not
            // advance - or that jumps backward or past our tip - can never make
            // progress: bail instead of spinning or skipping.
            if body.contiguous_seq <= cursor || body.contiguous_seq > our_tip {
                anyhow::bail!(
                    "protocol violation: receiver acked 200 but reported contiguous_seq {} (our cursor was {}, our tip is {}) - refusing to loop or skip data",
                    body.contiguous_seq, cursor, our_tip
                );
            }
            cursor = body.contiguous_seq;
        } else if status == reqwest::StatusCode::CONFLICT {
            let body: ShipResponse = resp.json()?;
            // A usable resume tip must be strictly ahead of our cursor AND within
            // our own log; anything else is a fork/hole (or a lying receiver) that
            // shipping cannot heal.
            if body.contiguous_seq <= cursor || body.contiguous_seq > our_tip {
                anyhow::bail!(
                    "receiver rejected at seq {:?}; its tip (seq {}) is not a usable resume point (our cursor {}, our tip {}): {} - shipping cannot heal this (fork or hole); run fsck on both stores",
                    body.at_seq,
                    body.contiguous_seq,
                    cursor,
                    our_tip,
                    body.reason.unwrap_or_default()
                );
            }
            // The receiver is genuinely further along (and within our own log):
            // resume from its tip.
            cursor = body.contiguous_seq;
        } else if status == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!("receiver rejected the token (401) mid-push - THOR_TOKEN must match on both sides");
        } else if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
            anyhow::bail!(
                "receiver returned 413 Payload Too Large: a shipment exceeded the receiver's body limit ({MAX_BODY_BYTES} bytes). A single event may be larger than that - raise MAX_BODY_BYTES on both sides, or split the fact."
            );
        } else {
            let text = resp.text().unwrap_or_default();
            anyhow::bail!("receiver returned {status}: {text}");
        }
    }

    Ok(PushSummary { applied: total_applied, batches, final_cursor: cursor })
}

/// Rolling state of the reconcile tick: what we last synced and, if the replica
/// is down, since when. The offline window is set ONCE on the first failure after
/// a success and cleared on recovery, so the alarm is information ("offline since
/// T"), not a permanent scream that resets every tick.
#[derive(Default, Debug, Clone)]
pub struct ReconcileState {
    pub last_ok_seq: i64,
    pub last_ok_epoch: u64,
    pub offline_since: Option<u64>,
    pub consecutive_failures: u32,
}

/// The outcome of one reconcile tick.
#[derive(Debug)]
pub enum TickResult {
    Synced { cursor: i64, applied: usize },
    Failed { error: String },
}

/// Fold one tick into the state and return an honest one-line status. Success
/// clears the offline window (noting recovery); failure sets or KEEPS it (never
/// resetting the since-T), so degraded RPO is stated plainly instead of alarmed
/// afresh every tick.
fn observe(state: &mut ReconcileState, outcome: &TickResult, now: u64) -> String {
    match outcome {
        TickResult::Synced { cursor, applied } => {
            let recovered = state.offline_since.take();
            state.last_ok_seq = *cursor;
            state.last_ok_epoch = now;
            state.consecutive_failures = 0;
            match recovered {
                Some(since) => format!(
                    "synced: replica at seq {cursor} (+{applied} this tick); RECOVERED after ~{}s offline",
                    now.saturating_sub(since)
                ),
                None => format!("synced: replica at seq {cursor} (+{applied} this tick)"),
            }
        }
        TickResult::Failed { error } => {
            if state.offline_since.is_none() {
                state.offline_since = Some(now);
            }
            state.consecutive_failures += 1;
            let since = state.offline_since.unwrap_or(now);
            format!(
                "replica offline since epoch {since} (~{}s ago, {} consecutive failures) - RPO degraded, last synced seq {}: {}",
                now.saturating_sub(since),
                state.consecutive_failures,
                state.last_ok_seq,
                error
            )
        }
    }
}

fn reconcile_once(db: &Path, url: &str, token: &str) -> anyhow::Result<PushSummary> {
    // Reopen per tick so newly-written local events are picked up.
    let store = EventStore::new(db)?;
    push_to(&store, url, token, SHIP_BATCH)
}

/// The reconcile tick: every `interval_secs`, push the local backlog and print an
/// honest status line. Never returns; a transient failure is logged (offline
/// since T), not fatal - a partition heals on a later tick with no new write.
pub fn run_reconcile(db: &Path, url: &str, token: &str, interval_secs: u64) -> anyhow::Result<()> {
    let interval = Duration::from_secs(interval_secs.max(1));
    let mut state = ReconcileState::default();
    println!("thor reconcile: shipping to {url} every {interval_secs}s (Ctrl-C to stop)");
    loop {
        let outcome = match reconcile_once(db, url, token) {
            Ok(sum) => TickResult::Synced { cursor: sum.final_cursor, applied: sum.applied },
            Err(e) => TickResult::Failed { error: e.to_string() },
        };
        println!("{}", observe(&mut state, &outcome, now_epoch()));
        std::thread::sleep(interval);
    }
}

/// Print an honest, live sync status: this store's contiguous tip and - with a
/// receiver URL - the replica's tip and the current lag, or that the replica is
/// unreachable (RPO degraded). Live probe, no persisted state, no standing alarm.
pub fn print_status(db: &Path, url: Option<&str>, token: Option<&str>) -> anyhow::Result<()> {
    let store = EventStore::new(db)?;
    let (local_seq, local_hash) = store.contiguous_tip()?;
    println!("local:   contiguous_seq {local_seq} (tip {})", short(&local_hash));

    let Some(url) = url else {
        println!("(no --to given: local status only)");
        return Ok(());
    };
    let base = url.trim_end_matches('/');
    let auth = format!("Bearer {}", token.unwrap_or(""));
    let client = build_client()?;
    match get_cursor(&client, base, &auth) {
        Ok(remote_seq) => {
            let lag = local_seq - remote_seq;
            if lag > 0 {
                println!("replica: contiguous_seq {remote_seq} (reachable) - LAG {lag} event(s) not yet replicated");
            } else if lag == 0 {
                println!("replica: contiguous_seq {remote_seq} (reachable) - in sync");
            } else {
                println!(
                    "replica: contiguous_seq {remote_seq} (reachable) - AHEAD by {} (not a pure replica of this store)",
                    -lag
                );
            }
        }
        Err(e) => {
            println!("replica: UNREACHABLE - RPO degraded; recent local writes exist only here until it returns ({e})");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventKind;

    #[test]
    fn test_ct_eq() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab")); // different length
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn test_authorized() {
        let mut h = HeaderMap::new();
        assert!(!authorized(&h, "tok"), "missing header is unauthorized");
        h.insert(header::AUTHORIZATION, "Bearer tok".parse().unwrap());
        assert!(authorized(&h, "tok"), "matching bearer is authorized");
        assert!(!authorized(&h, "other"), "wrong token is unauthorized");
        assert!(!authorized(&h, ""), "an empty configured token never authorizes");

        // the auth scheme is case-insensitive (RFC 7235)
        let mut hl = HeaderMap::new();
        hl.insert(header::AUTHORIZATION, "bearer tok".parse().unwrap());
        assert!(authorized(&hl, "tok"), "a lowercase bearer scheme is accepted");

        // a bare token without the Bearer scheme is unauthorized
        let mut h2 = HeaderMap::new();
        h2.insert(header::AUTHORIZATION, "tok".parse().unwrap());
        assert!(!authorized(&h2, "tok"));
    }

    #[test]
    fn test_plan_batch_bounds_by_count_and_bytes() {
        let mut s = EventStore::in_memory().unwrap();
        for i in 0..10 {
            s.append_event("s", "l", "a", EventKind::FactCreated, &format!("e{i}"), None, &"x".repeat(100))
                .unwrap();
        }
        let evs = s.get_all_events().unwrap();
        // ample budget: the count cap applies
        assert_eq!(plan_batch(&evs, 3, 10_000_000), 3, "count cap applies when bytes are ample");
        // tight budget: fewer than the full set fit, but always >= 1
        let n = plan_batch(&evs, 100, 300);
        assert!(n >= 1 && n < evs.len(), "byte budget must cap the batch below the full set: got {n}");
        // a lone oversized event still ships (never zero -> never a permanent stall)
        assert_eq!(plan_batch(&evs[..1], 100, 1), 1, "one event ships even under a tiny budget");
    }

    #[test]
    fn test_observe_tracks_offline_window_honestly() {
        let mut st = ReconcileState::default();
        // first success: no offline window, seq recorded
        let l0 = observe(&mut st, &TickResult::Synced { cursor: 10, applied: 3 }, 1000);
        assert!(l0.contains("seq 10"));
        assert!(st.offline_since.is_none());
        assert_eq!(st.last_ok_seq, 10);

        // first failure opens the offline window
        observe(&mut st, &TickResult::Failed { error: "conn refused".into() }, 1060);
        assert_eq!(st.offline_since, Some(1060));
        assert_eq!(st.consecutive_failures, 1);

        // a second failure KEEPS the same window (not reset) and counts up
        let l2 = observe(&mut st, &TickResult::Failed { error: "conn refused".into() }, 1120);
        assert_eq!(st.offline_since, Some(1060), "the offline window must not reset each tick");
        assert_eq!(st.consecutive_failures, 2);
        assert!(l2.contains("offline"));

        // recovery clears the window and says so
        let l3 = observe(&mut st, &TickResult::Synced { cursor: 12, applied: 2 }, 1200);
        assert!(st.offline_since.is_none());
        assert_eq!(st.consecutive_failures, 0);
        assert!(l3.to_lowercase().contains("recovered"), "recovery must be surfaced: {l3}");
    }

    fn seed_authority() -> (EventStore, Vec<String>) {
        let mut a = EventStore::in_memory().unwrap();
        let e1 = a.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "first").unwrap();
        a.append_event("s", "l", "act", EventKind::FactRevised, "e1", Some(&e1.this_hash), "second").unwrap();
        a.append_event("s", "l", "act", EventKind::FactCreated, "e2", None, "third").unwrap();
        let hashes = a.get_all_events().unwrap().iter().map(|e| e.this_hash.clone()).collect();
        (a, hashes)
    }

    async fn start_receiver(replica: Arc<Mutex<EventStore>>, token: &str) -> (String, tokio::task::JoinHandle<()>) {
        let app = ship_router(replica, token.to_string(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn test_inbox_pull_ack_roundtrip_over_http() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox.jsonl");
        crate::inbox::append(
            &inbox,
            &InboxOp {
                op: "create".into(),
                entity_id: "acme:mem-http".into(),
                body: "captured over http\n\n[memory/note | project: acme]".into(),
                parent_rev: None,
                ts: "0".into(),
                capture_id: "c1".into(),
            },
        )
        .unwrap();

        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let app = ship_router(replica, "secret".to_string(), Some(inbox.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // Pull rotates the live inbox away and returns the frozen batch.
        let b = base.clone();
        let ops = tokio::task::spawn_blocking(move || pull_inbox(&b, "secret").unwrap()).await.unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].entity_id, "acme:mem-http");
        assert!(!inbox.exists(), "pull rotated the live inbox away");
        assert!(draining_path(&inbox).exists(), "the frozen batch waits for ack");

        // A second pull WITHOUT an ack re-serves the same batch (at-least-once).
        let b = base.clone();
        let again = tokio::task::spawn_blocking(move || pull_inbox(&b, "secret").unwrap()).await.unwrap();
        assert_eq!(again.len(), 1, "an un-acked batch is re-served");

        // Ack drops the draining file; the next pull is empty.
        let b = base.clone();
        tokio::task::spawn_blocking(move || ack_inbox(&b, "secret").unwrap()).await.unwrap();
        assert!(!draining_path(&inbox).exists(), "ack cleared the batch");
        let b = base.clone();
        let empty = tokio::task::spawn_blocking(move || pull_inbox(&b, "secret").unwrap()).await.unwrap();
        assert!(empty.is_empty(), "nothing left after the ack");

        // A wrong token is refused (401).
        let b = base.clone();
        let unauth = tokio::task::spawn_blocking(move || pull_inbox(&b, "wrong")).await.unwrap();
        assert!(unauth.is_err(), "a bad token must be rejected");

        server.abort();
    }

    #[tokio::test]
    async fn test_ship_roundtrip_makes_replica_hash_identical() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_receiver(replica.clone(), "secret").await;

        // ship (blocking client) off the async runtime; batch=2 to force >1 batch
        let (applied, authority_hashes) = tokio::task::spawn_blocking(move || {
            let (authority, hashes) = seed_authority();
            let summary = push_to(&authority, &base, "secret", 2).unwrap();
            (summary.applied, hashes)
        })
        .await
        .unwrap();

        assert_eq!(applied, 3, "all three events must be applied");
        let replica_hashes: Vec<String> = {
            let s = replica.lock().unwrap();
            s.get_all_events().unwrap().iter().map(|e| e.this_hash.clone()).collect()
        };
        assert_eq!(replica_hashes, authority_hashes, "replica must be hash-identical to the authority");
        server.abort();
    }

    #[tokio::test]
    async fn test_ship_is_idempotent_on_reship() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_receiver(replica.clone(), "secret").await;

        let second_applied = tokio::task::spawn_blocking(move || {
            let (authority, _) = seed_authority();
            let first = push_to(&authority, &base, "secret", 10).unwrap();
            assert_eq!(first.applied, 3);
            // shipping again must apply nothing (receiver already has it all)
            push_to(&authority, &base, "secret", 10).unwrap().applied
        })
        .await
        .unwrap();

        assert_eq!(second_applied, 0, "a re-ship applies nothing");
        assert_eq!(replica.lock().unwrap().get_all_events().unwrap().len(), 3);
        server.abort();
    }

    #[tokio::test]
    async fn test_ship_rejects_wrong_token() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_receiver(replica.clone(), "right-token").await;

        let result = tokio::task::spawn_blocking(move || {
            let (authority, _) = seed_authority();
            push_to(&authority, &base, "WRONG-token", 10)
        })
        .await
        .unwrap();

        assert!(result.is_err(), "a wrong token must fail the push (401)");
        assert!(
            replica.lock().unwrap().get_all_events().unwrap().is_empty(),
            "nothing may be ingested under a bad token"
        );
        server.abort();
    }
}
