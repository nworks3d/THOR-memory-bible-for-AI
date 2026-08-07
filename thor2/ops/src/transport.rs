//! HTTP transport for replicating the append-only log to a remote replica.
//!
//! The log is append-only with a hash chain, so replication reduces to one
//! question asked over and over: "where does the receiver's chain end?" - and
//! one answer: "here is everything after that point, in order." The receiver
//! (this module's `run_receiver`) never accepts anything that does not
//! continue its own chain; a gap, a fork, or a tampered payload is refused
//! with a reason and writes nothing (`EventStore::ingest_batch` already
//! guarantees the batch is atomic - see core/src/event_store.rs). The shipper
//! (`push_once`) asks the receiver where it stands (`remote_cursor`) and
//! sends only the events after that point.
//!
//! Auth is a single shared secret, presented as an HTTP bearer token and
//! compared in constant time so a wrong guess cannot be narrowed down by
//! response timing. The token is the ONLY protection this transport has: run
//! it on a LAN or a private tunnel, never expose it to the open internet.

use anyhow::bail;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thor_core::event_store::{Event, EventStore, IngestOutcome, ShippedEvent};
use thor_core::inbox::{self, InboxOp};

/// Default upper bound on events per shipment; a lone oversized event still
/// ships alone rather than stalling replication forever (see `plan_batch`).
pub const DEFAULT_BATCH: usize = 256;

#[derive(Clone)]
struct ReceiverState {
    store: Arc<Mutex<EventStore>>,
    token: Arc<String>,
    /// Where this receiver's capture inbox lives, when it is running as a
    /// replica that accepts writes (see `thor_core::inbox`). `None` on a
    /// plain mirror: it then answers the pull with `capture_enabled: false`
    /// rather than an empty batch, so a drain against the wrong receiver is
    /// reported instead of looking like a healthy nothing-to-do.
    inbox: Option<Arc<PathBuf>>,
}

/// The receiver's resume cursor: its hole-proof contiguous tip.
#[derive(Debug, Serialize, Deserialize)]
pub struct CursorResponse {
    pub contiguous_seq: i64,
    pub tip_hash: String,
}

/// A shipment: a batch of events in chain order.
#[derive(Serialize, Deserialize)]
pub struct ShipRequest {
    pub events: Vec<ShippedEvent>,
}

/// The receiver's answer to a shipment.
#[derive(Debug, Serialize, Deserialize)]
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

/// What a receiver answers to an inbox pull.
///
/// `capture_enabled` is the field that matters as much as the batch: a
/// receiver with no inbox configured can never hold a capture, and saying so
/// is a different answer from "nothing was captured this hour". Without it,
/// forgetting to switch a replica into capture mode reads as a permanently
/// healthy drain - the same defect class `push_once` catches with its AHEAD
/// and DIFFERENT-tip refusals.
#[derive(Debug, Serialize, Deserialize)]
pub struct InboxPullResponse {
    pub capture_enabled: bool,
    #[serde(default)]
    pub ops: Vec<InboxOp>,
}

/// Constant-time byte compare so a wrong token cannot be narrowed down by
/// early-exit timing. The length check leaks only the token LENGTH.
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

/// True iff `headers` carries `Authorization: Bearer <token>` matching
/// `token`. An empty configured token NEVER authorizes - a blank secret must
/// not become a skeleton key even if it somehow got configured that way.
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
    State(st): State<ReceiverState>,
    headers: HeaderMap,
) -> Result<Json<CursorResponse>, StatusCode> {
    if !authorized(&headers, &st.token) {
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
    State(st): State<ReceiverState>,
    headers: HeaderMap,
    Json(req): Json<ShipRequest>,
) -> Result<(StatusCode, Json<ShipResponse>), StatusCode> {
    if !authorized(&headers, &st.token) {
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

/// GET /inbox/pull - rotate the capture inbox and serve the pending batch so
/// the authority can replay it. Bearer-gated like everything else here.
///
/// The rotation happens on the READ, not on the ack: captures arriving while
/// the authority is applying this batch land in a fresh file and are never
/// swept into a batch that is already being processed. See
/// `thor_core::inbox::rotate_and_read` for why an un-acked batch is served
/// again rather than dropped.
async fn inbox_pull_handler(
    State(st): State<ReceiverState>,
    headers: HeaderMap,
) -> Result<Json<InboxPullResponse>, StatusCode> {
    if !authorized(&headers, &st.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(path) = st.inbox.clone() else {
        return Ok(Json(InboxPullResponse { capture_enabled: false, ops: Vec::new() }));
    };
    let ops = tokio::task::spawn_blocking(move || inbox::rotate_and_read(&path))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(InboxPullResponse { capture_enabled: true, ops }))
}

/// POST /inbox/ack - the authority applied the pulled batch, so the draining
/// file may go. Acking a receiver that has no inbox, or acking twice, is
/// success: the delivery is at-least-once, and an ack that errors on a
/// repeat would turn a harmless retry into a stuck drain.
async fn inbox_ack_handler(
    State(st): State<ReceiverState>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    if !authorized(&headers, &st.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(path) = st.inbox.clone() else {
        return Ok(StatusCode::NO_CONTENT);
    };
    tokio::task::spawn_blocking(move || inbox::drop_draining(&path))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The receiver's axum router: GET /sync/cursor (where do you stand) + POST
/// /sync/append (here is what comes next), plus GET /inbox/pull and POST
/// /inbox/ack when this receiver also captures writes. Exposed separately
/// from `run_receiver` so tests can drive it without a live socket.
fn receiver_router(store: Arc<Mutex<EventStore>>, token: String, inbox: Option<PathBuf>) -> Router {
    let state = ReceiverState { store, token: Arc::new(token), inbox: inbox.map(Arc::new) };
    Router::new()
        .route("/sync/cursor", get(cursor_handler))
        .route("/sync/append", post(append_handler))
        .route("/inbox/pull", get(inbox_pull_handler))
        .route("/inbox/ack", post(inbox_ack_handler))
        .with_state(state)
}

/// Run the receiver on `bind` (e.g. `0.0.0.0:5555`), ingesting shipped
/// batches into the store at `db`. Blocking - owns its own tokio runtime.
/// Refuses to start on an empty token so an endpoint is never accidentally
/// opened with no auth in front of it.
pub fn run_receiver(db: &Path, bind: &str, token: String, inbox: Option<PathBuf>) -> anyhow::Result<()> {
    if token.trim().is_empty() {
        bail!("a receiver needs a non-empty shared secret - refusing to open an unauthenticated endpoint");
    }
    let store = Arc::new(Mutex::new(EventStore::new(db)?));
    let capture = inbox.clone();
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let app = receiver_router(store, token, inbox);
        let listener = tokio::net::TcpListener::bind(bind).await?;
        println!("thor sync receiver listening on http://{bind}/sync (bearer-gated)");
        match &capture {
            Some(p) => println!("capture inbox enabled at {} - writes arriving here are queued for the authority to drain", p.display()),
            None => println!("capture inbox NOT enabled - this receiver is a mirror only, and a write reaching it has nowhere to go"),
        }
        axum::serve(listener, app).await?;
        Ok::<(), anyhow::Error>(())
    })
}

fn build_client() -> anyhow::Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?)
}

/// Ask the receiver at `base` where it stands: its hole-proof contiguous tip.
pub fn remote_cursor(base: &str, token: &str) -> anyhow::Result<CursorResponse> {
    let base = base.trim_end_matches('/');
    let client = build_client()?;
    let resp = client
        .get(format!("{base}/sync/cursor"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .send()?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("receiver rejected the shared secret (401) - it must match on both sides");
    }
    Ok(resp.error_for_status()?.json::<CursorResponse>()?)
}

/// Pull (and rotate) the capture inbox of the receiver at `base`.
///
/// The batch is NOT gone from the receiver after this: it sits in the
/// draining slot until `ack_inbox` confirms the authority applied it. Call
/// the ack only after the writes actually landed, never before.
pub fn pull_inbox(base: &str, token: &str) -> anyhow::Result<InboxPullResponse> {
    let base = base.trim_end_matches('/');
    let client = build_client()?;
    let resp = client
        .get(format!("{base}/inbox/pull"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .send()?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("receiver rejected the shared secret (401) - it must match on both sides");
    }
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!(
            "the receiver at {base} has no /inbox/pull route - it is running a build from before \
             capture existed, so nothing written there can ever reach this store. Update it."
        );
    }
    Ok(resp.error_for_status()?.json::<InboxPullResponse>()?)
}

/// Tell the receiver at `base` that the pulled batch was applied, so it may
/// drop the draining copy.
pub fn ack_inbox(base: &str, token: &str) -> anyhow::Result<()> {
    let base = base.trim_end_matches('/');
    let client = build_client()?;
    let resp = client
        .post(format!("{base}/inbox/ack"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .send()?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("receiver rejected the shared secret (401) on the ack - the batch stays queued");
    }
    resp.error_for_status()?;
    Ok(())
}

/// How many events from the front of `events` fit in one shipment - at most
/// `count_cap`, but always at least 1 so a lone oversized event still ships
/// instead of stalling replication forever.
fn plan_batch(events: &[Event], count_cap: usize) -> usize {
    events.len().min(count_cap.max(1)).max(if events.is_empty() { 0 } else { 1 })
}

/// Summary of a completed push.
#[derive(Debug, Clone, Copy)]
pub struct PushSummary {
    pub applied: usize,
    pub skipped: usize,
    pub batches: usize,
    pub final_cursor: i64,
}

/// Push `store`'s backlog to a remote receiver at `base`. Reads the
/// receiver's contiguous cursor, then ships `events_since(cursor)` in
/// `batch`-sized chunks, advancing the cursor on every 200 and resuming from
/// the receiver's returned tip on a 409. A 409 whose reported tip does not
/// strictly advance (or lands outside our own log) means the gap or fork
/// cannot be healed by shipping more - that is reported plainly, never
/// retried silently.
pub fn push_once(store: &EventStore, base: &str, token: &str, batch: usize) -> anyhow::Result<PushSummary> {
    let base = base.trim_end_matches('/');
    let batch = batch.max(1);
    let client = build_client()?;
    let auth = format!("Bearer {token}");

    let remote = remote_cursor(base, token)?;
    let (our_seq, our_hash) = store.contiguous_tip()?;

    // Two ways for this to be a no-op that reports success, and an hourly
    // scheduled ship would write "0 applied" into a log for weeks either way.
    // Both are caught here, before a single event moves, because the loop
    // below can only ever see "nothing to send" and break.
    if remote.contiguous_seq > our_seq {
        bail!(
            "the receiver is AHEAD of this store (it is at seq {}, we are at {}) - it is not a \
             replica of this store, and shipping cannot make it one. Check that --db and --to \
             are the pair you meant before doing anything else.",
            remote.contiguous_seq, our_seq
        );
    }
    if remote.contiguous_seq == our_seq && remote.tip_hash != our_hash {
        bail!(
            "the receiver is at the same height (seq {our_seq}) but a DIFFERENT tip ({} vs our \
             {}) - the two logs forked and there is nothing left to ship, so this would look \
             like a healthy no-op forever. Run the health check on both stores.",
            remote.tip_hash, our_hash
        );
    }

    let mut cursor = remote.contiguous_seq;
    let mut total_applied = 0usize;
    let mut total_skipped = 0usize;
    let mut batches = 0usize;

    loop {
        let pending = store.events_since(cursor)?;
        if pending.is_empty() {
            break;
        }
        let our_tip = store.get_next_seq()? - 1;
        let take = plan_batch(&pending, batch);
        let chunk: Vec<ShippedEvent> = pending[..take].iter().map(|e| e.to_shipped()).collect();

        let resp = client
            .post(format!("{base}/sync/append"))
            .header(header::AUTHORIZATION, &auth)
            .json(&ShipRequest { events: chunk })
            .send()?;
        batches += 1;
        let status = resp.status();

        if status == reqwest::StatusCode::OK {
            let body: ShipResponse = resp.json()?;
            total_applied += body.applied;
            total_skipped += body.skipped;
            if body.contiguous_seq <= cursor || body.contiguous_seq > our_tip {
                bail!(
                    "protocol violation: receiver acked 200 but reported contiguous_seq {} (our cursor was {}, our tip is {}) - refusing to loop or skip data",
                    body.contiguous_seq, cursor, our_tip
                );
            }
            cursor = body.contiguous_seq;
        } else if status == reqwest::StatusCode::CONFLICT {
            let body: ShipResponse = resp.json()?;
            if body.contiguous_seq <= cursor || body.contiguous_seq > our_tip {
                bail!(
                    "receiver refused at seq {:?}: {} (its tip is seq {}, our cursor was {}, our tip is {}) - shipping cannot heal a fork or a hole; run the health check on both stores",
                    body.at_seq, body.reason.unwrap_or_default(), body.contiguous_seq, cursor, our_tip
                );
            }
            cursor = body.contiguous_seq;
        } else if status == reqwest::StatusCode::UNAUTHORIZED {
            bail!("receiver rejected the shared secret (401) mid-push - it must match on both sides");
        } else {
            let text = resp.text().unwrap_or_default();
            bail!("receiver returned {status}: {text}");
        }
    }

    Ok(PushSummary { applied: total_applied, skipped: total_skipped, batches, final_cursor: cursor })
}

/// Local + (optionally) remote sync status, as data - `ops::install`'s CLI
/// and the health check both render this into their own wording, so the two
/// never have to agree on phrasing, only on the numbers.
pub struct SyncStatus {
    pub local_contiguous_seq: i64,
    pub local_tip_hash: String,
    pub remote: Option<Result<CursorResponse, String>>,
}

/// Live probe of sync status: this store's contiguous tip and, when a
/// receiver base URL is given, the replica's tip too. Never persists
/// anything - a fresh read every time, like `thor_core::event_store`'s own
/// `contiguous_tip`. Reports on a store without ever creating one
/// (`EventStore::open_existing`).
pub fn sync_status(db: &Path, remote: Option<(&str, &str)>) -> anyhow::Result<SyncStatus> {
    let store = EventStore::open_existing(db)?;
    let (local_contiguous_seq, local_tip_hash) = store.contiguous_tip()?;
    let remote = remote.map(|(base, token)| remote_cursor(base, token).map_err(|e| e.to_string()));
    Ok(SyncStatus { local_contiguous_seq, local_tip_hash, remote })
}

#[cfg(test)]
mod tests {
    use super::*;
    use thor_core::event_store::EventKind;

    #[test]
    fn test_ct_eq() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn test_authorized() {
        let mut h = HeaderMap::new();
        assert!(!authorized(&h, "tok"), "missing header is unauthorized");
        h.insert(header::AUTHORIZATION, "Bearer tok".parse().unwrap());
        assert!(authorized(&h, "tok"));
        assert!(!authorized(&h, "other"));
        assert!(!authorized(&h, ""), "an empty configured token never authorizes");

        let mut hl = HeaderMap::new();
        hl.insert(header::AUTHORIZATION, "bearer tok".parse().unwrap());
        assert!(authorized(&hl, "tok"), "bearer scheme is matched case-insensitively");
    }

    #[test]
    fn run_receiver_refuses_an_empty_token() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("r.db");
        let err = run_receiver(&db, "127.0.0.1:0", String::new(), None);
        assert!(err.is_err(), "an empty shared secret must never open an endpoint");
    }

    fn seed_authority() -> (EventStore, Vec<String>) {
        let mut a = EventStore::in_memory().unwrap();
        let e1 = a.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "first").unwrap();
        a.append_event("s", "l", "act", EventKind::FactRevised, "e1", Some(&e1.this_hash), "second").unwrap();
        a.append_event("s", "l", "act", EventKind::FactCreated, "e2", None, "third").unwrap();
        let hashes = a.get_all_events().unwrap().iter().map(|e| e.this_hash.clone()).collect();
        (a, hashes)
    }

    async fn start_test_receiver(replica: Arc<Mutex<EventStore>>, token: &str) -> (String, tokio::task::JoinHandle<()>) {
        let app = receiver_router(replica, token.to_string(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn test_push_makes_replica_hash_identical() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_test_receiver(replica.clone(), "secret").await;

        let (applied, authority_hashes) = tokio::task::spawn_blocking(move || {
            let (authority, hashes) = seed_authority();
            let summary = push_once(&authority, &base, "secret", 2).unwrap();
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

    /// The defect this guards against: an hourly scheduled ship writing
    /// "0 applied" into a log for weeks while the two stores are not related
    /// at all. The loop can only ever see "nothing to send" here, so the
    /// mismatch has to be caught before it, or it never gets reported.
    #[tokio::test]
    async fn a_receiver_ahead_of_us_is_refused_not_reported_as_a_clean_no_op() {
        // Replica holds a full chain; the "authority" is a fresh empty store.
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        {
            let (seeded, _) = seed_authority();
            let mut r = replica.lock().unwrap();
            for e in seeded.get_all_events().unwrap() {
                r.ingest_batch(&[e.to_shipped()]).unwrap();
            }
        }
        let (base, server) = start_test_receiver(replica.clone(), "secret").await;

        let err = tokio::task::spawn_blocking(move || {
            let empty = EventStore::in_memory().unwrap();
            push_once(&empty, &base, "secret", 10).map(|s| s.applied)
        })
        .await
        .unwrap();

        let msg = err.expect_err("shipping to a receiver that is ahead must fail loudly").to_string();
        assert!(
            msg.contains("AHEAD"),
            "the message must say the receiver is ahead, not just that nothing moved: {msg}"
        );
        server.abort();
    }

    /// The sibling defect: same height, different history. Nothing is left to
    /// ship, so without this check it reads as a permanently healthy sync.
    #[tokio::test]
    async fn a_fork_at_the_same_height_is_refused_not_a_silent_success() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        {
            let (seeded, _) = seed_authority();
            let mut r = replica.lock().unwrap();
            for e in seeded.get_all_events().unwrap() {
                r.ingest_batch(&[e.to_shipped()]).unwrap();
            }
        }
        let (base, server) = start_test_receiver(replica.clone(), "secret").await;

        let err = tokio::task::spawn_blocking(move || {
            // Same number of events, different bodies, so the same height with
            // a different tip hash.
            let mut other = EventStore::in_memory().unwrap();
            let e1 = other
                .append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "DIFFERENT")
                .unwrap();
            other
                .append_event("s", "l", "act", EventKind::FactRevised, "e1", Some(&e1.this_hash), "also different")
                .unwrap();
            other
                .append_event("s", "l", "act", EventKind::FactCreated, "e2", None, "third but not the same")
                .unwrap();
            push_once(&other, &base, "secret", 10).map(|s| s.applied)
        })
        .await
        .unwrap();

        let msg = err.expect_err("a fork at equal height must fail loudly").to_string();
        assert!(
            msg.contains("DIFFERENT tip"),
            "the message must name the fork rather than report a clean no-op: {msg}"
        );
        server.abort();
    }

    /// A re-ship applies nothing new - the exact defect a non-idempotent
    /// shipper would show up as (a duplicated event, or a divergent replica).
    #[tokio::test]
    async fn test_push_twice_applies_nothing_the_second_time() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_test_receiver(replica.clone(), "secret").await;

        let second_applied = tokio::task::spawn_blocking(move || {
            let (authority, _) = seed_authority();
            let first = push_once(&authority, &base, "secret", 10).unwrap();
            assert_eq!(first.applied, 3);
            push_once(&authority, &base, "secret", 10).unwrap().applied
        })
        .await
        .unwrap();

        assert_eq!(second_applied, 0, "a re-ship must apply nothing");
        assert_eq!(replica.lock().unwrap().get_all_events().unwrap().len(), 3);
        server.abort();
    }

    /// The load-bearing refusal test: a shipment that does NOT connect to
    /// what the replica already has (a gap - here, the replica never saw
    /// event 1 at all) must be rejected outright, and it must leave the
    /// replica exactly as it was - never a silently-created branch that
    /// starts mid-chain.
    #[tokio::test]
    async fn test_non_contiguous_shipment_is_rejected_and_makes_no_branch() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_test_receiver(replica.clone(), "secret").await;

        let (authority, _hashes) = seed_authority();
        // Ship only events_since(1) - i.e. skip event 1 entirely - straight at
        // an empty replica, bypassing push_once's own cursor read so the gap
        // reaches the receiver exactly as a corrupt/lagging shipper would.
        let pending: Vec<ShippedEvent> = authority.events_since(1).unwrap().iter().map(|e| e.to_shipped()).collect();
        assert_eq!(pending.len(), 2, "events 2 and 3 - deliberately skipping event 1");

        let b = base.clone();
        let status = tokio::task::spawn_blocking(move || {
            // Built here, on the blocking pool: reqwest's blocking client
            // must never be constructed (or dropped) from inside an active
            // tokio async context - in debug builds it self-checks for that
            // exact misuse by building and dropping a throwaway runtime,
            // and dropping a runtime from an async context is what panics.
            let client = reqwest::blocking::Client::new();
            client
                .post(format!("{b}/sync/append"))
                .header(header::AUTHORIZATION, "Bearer secret")
                .json(&ShipRequest { events: pending })
                .send()
                .unwrap()
                .status()
        })
        .await
        .unwrap();

        assert_eq!(status, reqwest::StatusCode::CONFLICT, "a gap must be refused (409), not silently accepted");
        assert!(
            replica.lock().unwrap().get_all_events().unwrap().is_empty(),
            "nothing may be written when the batch does not connect - no branch, ever"
        );
        server.abort();
    }

    /// The other load-bearing refusal test: a wrong shared secret must be
    /// rejected outright, and nothing may be ingested under it.
    #[tokio::test]
    async fn test_wrong_secret_is_rejected_and_ingests_nothing() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_test_receiver(replica.clone(), "right-secret").await;

        let result = tokio::task::spawn_blocking(move || {
            let (authority, _) = seed_authority();
            push_once(&authority, &base, "WRONG-secret", 10)
        })
        .await
        .unwrap();

        assert!(result.is_err(), "a wrong shared secret must fail the push");
        assert!(
            replica.lock().unwrap().get_all_events().unwrap().is_empty(),
            "nothing may be ingested under a wrong secret"
        );
        server.abort();
    }

    #[tokio::test]
    async fn test_sync_status_reports_lag_when_replica_is_behind() {
        let replica_store = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_test_receiver(replica_store.clone(), "secret").await;

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("authority.db");
        {
            let mut a = EventStore::new(&db).unwrap();
            let e1 = a.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "first").unwrap();
            a.append_event("s", "l", "act", EventKind::FactRevised, "e1", Some(&e1.this_hash), "second").unwrap();
        }

        let b = base.clone();
        let status = tokio::task::spawn_blocking(move || sync_status(&db, Some((&b, "secret"))).unwrap())
            .await
            .unwrap();
        assert_eq!(status.local_contiguous_seq, 2);
        let remote = status.remote.expect("remote was requested").expect("receiver reachable");
        assert_eq!(remote.contiguous_seq, 0, "an empty replica reports seq 0 - fully behind");
        server.abort();
    }

    #[test]
    fn sync_status_refuses_to_create_a_missing_store() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        assert!(sync_status(&missing, None).is_err(), "status must refuse a path with no store");
        assert!(!missing.exists(), "status must not create the store file");
    }

    async fn start_capture_receiver(inbox: PathBuf, token: &str) -> (String, tokio::task::JoinHandle<()>) {
        let store = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let app = receiver_router(store, token.to_string(), Some(inbox));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    /// THE DEFECT THIS PREVENTS: an hourly drain against a receiver that was
    /// never switched into capture mode reporting "0 captured" forever, which
    /// is indistinguishable from a quiet week. The answer has to say which of
    /// the two it is.
    #[tokio::test]
    async fn a_receiver_without_an_inbox_says_so_instead_of_answering_an_empty_batch() {
        let replica = Arc::new(Mutex::new(EventStore::in_memory().unwrap()));
        let (base, server) = start_test_receiver(replica, "secret").await;

        let resp = tokio::task::spawn_blocking(move || pull_inbox(&base, "secret").unwrap())
            .await
            .unwrap();

        assert!(!resp.capture_enabled, "a mirror must report that it captures nothing");
        assert!(resp.ops.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn a_captured_call_survives_the_hop_and_the_ack_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        inbox::append(
            &path,
            &InboxOp::new("remember", serde_json::json!({ "id": "from-the-phone", "kind": "rule" })),
        )
        .unwrap();
        let (base, server) = start_capture_receiver(path.clone(), "secret").await;

        let b = base.clone();
        let first = tokio::task::spawn_blocking(move || pull_inbox(&b, "secret").unwrap()).await.unwrap();
        assert!(first.capture_enabled);
        assert_eq!(first.ops.len(), 1);
        assert_eq!(first.ops[0].tool, "remember");
        assert_eq!(first.ops[0].args["id"], "from-the-phone");
        assert!(
            inbox::draining_path(&path).exists(),
            "the batch stays on the receiver until the authority acks it"
        );

        let b = base.clone();
        let after_ack = tokio::task::spawn_blocking(move || {
            ack_inbox(&b, "secret").unwrap();
            pull_inbox(&b, "secret").unwrap()
        })
        .await
        .unwrap();
        assert!(after_ack.ops.is_empty(), "an acked batch is not served twice");
        assert!(!inbox::draining_path(&path).exists());
        server.abort();
    }

    /// THE DEFECT THIS PREVENTS: an unauthenticated request rotating the
    /// inbox. The rotation is a side effect on the READ, so a 401 that still
    /// ran it would let anyone move captures into a draining slot that only
    /// the real authority can drain - a silent denial of service on the
    /// owner's own writes.
    #[tokio::test]
    async fn a_wrong_secret_neither_reads_nor_rotates_the_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        inbox::append(&path, &InboxOp::new("remember", serde_json::json!({ "id": "mine" }))).unwrap();
        let (base, server) = start_capture_receiver(path.clone(), "right").await;

        let b = base.clone();
        let err = tokio::task::spawn_blocking(move || pull_inbox(&b, "WRONG")).await.unwrap();
        assert!(err.is_err(), "a wrong secret must not be served the inbox");
        assert!(path.exists(), "the live inbox must not have been rotated by an unauthorized pull");
        assert!(!inbox::draining_path(&path).exists());

        // And the real authority still gets its batch.
        let b = base.clone();
        let ok = tokio::task::spawn_blocking(move || pull_inbox(&b, "right").unwrap()).await.unwrap();
        assert_eq!(ok.ops.len(), 1);
        server.abort();
    }

    /// The at-least-once guarantee over the wire: no ack means the same batch
    /// comes back, rather than being lost with the process that pulled it.
    #[tokio::test]
    async fn a_batch_pulled_but_never_acked_comes_back_on_the_next_pull() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        inbox::append(&path, &InboxOp::new("retract", serde_json::json!({ "id": "x" }))).unwrap();
        let (base, server) = start_capture_receiver(path, "secret").await;

        let both = tokio::task::spawn_blocking(move || {
            let first = pull_inbox(&base, "secret").unwrap();
            // The authority dies here, before the ack.
            let second = pull_inbox(&base, "secret").unwrap();
            (first.ops.len(), second.ops.len(), second.ops[0].tool.clone())
        })
        .await
        .unwrap();

        assert_eq!(both, (1, 1, "retract".to_string()), "the un-acked batch must be re-served");
        server.abort();
    }
}
