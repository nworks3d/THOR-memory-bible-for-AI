//! Client for the warm injection daemon (`thor daemon` / `thor mcp --http`),
//! modeled 1:1 on embed_daemon's client conventions: a published sidecar
//! (THOR-DAEMON.flag next to the store) discovers the bind, a raw TcpStream
//! with a tight client-side timeout budget replaces a per-prompt process cost,
//! and ANY failure self-heals the stale flag and returns None so the caller
//! falls back to the in-process cold path. Fail-open like everything on the
//! hook path: nothing here may block or slow a prompt beyond the budget below.
//! Idea credit: the warm /inject daemon concept comes from mimir
//! (MakerViking/mimir); this is an independent reimplementation.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Total client-side connect+read budget. A warm /inject still runs a REAL
/// fused recall (measured 170-230ms on a 12k-event store; silence decisions
/// are faster) - the daemon saves the process/store/model startup, not the
/// recall itself. The budget must sit safely above a healthy answer and only
/// protect against a wedged daemon: worst case is "this budget + the cold
/// path" once, after which the self-healed flag makes later prompts cold-fast.
pub const CLIENT_TIMEOUT: Duration = Duration::from_millis(450);
/// Sub-budget for the TCP connect itself (loopback is near-instant or refused).
const CONNECT_TIMEOUT: Duration = Duration::from_millis(40);
/// Debounce for ensure_daemon's detached spawn (mirrors embed_daemon).
const SPAWN_DEBOUNCE: Duration = Duration::from_secs(15);

fn flagfile(db: &Path) -> PathBuf {
    db.with_file_name("THOR-DAEMON.flag")
}
fn startfile(db: &Path) -> PathBuf {
    db.with_file_name("THOR-DAEMON.starting")
}

/// Written by the daemon (mcp::serve_http) right after its listener binds; the
/// content is `{bind, pid, db}` JSON so doctor can report a mismatched store.
pub fn publish_daemon_flag(db: &Path, bind: &str) -> std::io::Result<()> {
    publish_daemon_flag_as(db, bind, std::process::id() as u64)
}

/// Flag write with an explicit pid: the bind-in-use adoption path republishes
/// the flag of an ALREADY-RUNNING daemon (whose pid comes from /health), e.g.
/// after a timeout self-heal deleted the flag of a live process.
pub fn publish_daemon_flag_as(db: &Path, bind: &str, pid: u64) -> std::io::Result<()> {
    let _ = std::fs::remove_file(startfile(db)); // start completed: clear the debounce marker
    std::fs::write(
        flagfile(db),
        serde_json::json!({
            "bind": bind,
            "pid": pid,
            "db": db.display().to_string(),
        })
        .to_string(),
    )
}

pub enum DaemonReply {
    /// The daemon answered and decided SILENCE (a real decision, not a failure).
    Silent,
    /// The daemon answered with an injection block.
    Inject(String),
}

/// Ask the warm daemon to inject for this raw hook JSON. `None` on ANY failure
/// (no flag file, connect refused/timeout, malformed or non-2xx response) -
/// the caller MUST fall back to the cold path. A failure also deletes the flag
/// file (self-heal after an ungraceful daemon death), exactly like
/// embed_daemon's client.
pub fn try_inject(db: &Path, raw_hook_json: &str) -> Option<DaemonReply> {
    if !flagfile(db).exists() {
        return None; // cheap fast path: no daemon was ever published
    }
    match request(db, "POST", "/inject", Some(raw_hook_json)) {
        Some((status, body)) if status == 200 => Some(DaemonReply::Inject(body)),
        Some((status, _)) if status == 204 => Some(DaemonReply::Silent),
        _ => {
            let _ = std::fs::remove_file(flagfile(db));
            None
        }
    }
}

/// Cheap liveness probe (GET /health) for doctor and ensure_daemon: never
/// touches the store or ledger. None = cold/unreachable.
pub fn health(db: &Path) -> Option<serde_json::Value> {
    health_at(&published_bind(db)?)
}

/// Direct-bind health probe (no flag lookup): the bind-in-use adoption path
/// asks "who holds this port" when the discovery flag may be gone.
pub fn health_at(bind: &str) -> Option<serde_json::Value> {
    let (status, body) = request_at(bind, "GET", "/health", None)?;
    (status == 200).then(|| serde_json::from_str(&body).ok())?
}

fn request(db: &Path, method: &str, path: &str, body: Option<&str>) -> Option<(u16, String)> {
    request_at(&published_bind(db)?, method, path, body)
}

fn request_at(bind: &str, method: &str, path: &str, body: Option<&str>) -> Option<(u16, String)> {
    let addr: std::net::SocketAddr = bind.parse().ok()?;
    let deadline = Instant::now() + CLIENT_TIMEOUT;
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return None;
    }
    stream.set_read_timeout(Some(remaining)).ok()?;
    stream.set_write_timeout(Some(remaining)).ok()?;
    let payload = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len(),
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?; // Connection: close -> EOF ends the response
    parse_response(&buf)
}

/// Minimal HTTP/1.1 response parse: status code + body after the blank line.
/// Anything unparseable is a failure (the caller treats it as cold fallback).
fn parse_response(buf: &[u8]) -> Option<(u16, String)> {
    let text = String::from_utf8_lossy(buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status_line = head.lines().next()?;
    let code: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    Some((code, body.to_string()))
}

fn published_bind(db: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(flagfile(db)).ok()?;
    let meta: serde_json::Value = serde_json::from_str(&raw).ok()?;
    meta.get("bind").and_then(|v| v.as_str()).map(str::to_string)
}

/// SessionStart-safe warm start: when /health does not answer, spawn
/// `thor daemon` detached (debounced via the .starting marker). NEVER called
/// from the per-prompt failure path - a full HTTP+MCP server is a bigger side
/// effect than the embedder, so it only starts deliberately (manual `thor
/// daemon` or this opt-in SessionStart hook).
pub fn ensure_daemon(db: &Path) {
    if health(db).is_some() {
        return;
    }
    let sf = startfile(db);
    if let Ok(meta) = std::fs::metadata(&sf) {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().map(|e| e < SPAWN_DEBOUNCE).unwrap_or(false) {
                return; // a start is already in flight
            }
        }
    }
    let _ = std::fs::write(&sf, "");
    if let Ok(exe) = std::env::current_exe() {
        let _ = spawn_detached(&exe, db);
    }
}

// CRITICAL: the daemon MUST NOT inherit the caller's std handles. SessionStart
// hooks' stdout is read to EOF by Claude Code; a long-lived daemon holding
// that pipe's write end would block session start until it exits. Redirecting
// all three streams to null severs that. (Same rationale as embed_daemon.)
#[cfg(windows)]
fn spawn_detached(exe: &Path, db: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(exe)
        .arg("--db")
        .arg(db)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}
#[cfg(not(windows))]
fn spawn_detached(exe: &Path, db: &Path) -> std::io::Result<()> {
    std::process::Command::new(exe)
        .arg("--db")
        .arg(db)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_inject_none_without_flagfile() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("d.db");
        let t0 = Instant::now();
        assert!(try_inject(&db, "{}").is_none());
        assert!(t0.elapsed() < Duration::from_millis(50), "no-flag path is near-instant");
    }

    #[test]
    fn try_inject_self_heals_stale_flag_and_stays_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("d.db");
        // A flag pointing at a port nobody listens on (bind, then drop).
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().to_string()
        };
        publish_daemon_flag(&db, &dead).unwrap();
        let t0 = Instant::now();
        assert!(try_inject(&db, "{}").is_none());
        assert!(t0.elapsed() < Duration::from_millis(300), "bounded on connect-refused");
        assert!(!flagfile(&db).exists(), "stale flag self-healed");
    }

    #[test]
    fn try_inject_bounded_when_daemon_is_wedged() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("d.db");
        // A listener that accepts but never answers (wedged daemon).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let _keep = std::thread::spawn(move || {
            let _conn = listener.accept();
            std::thread::sleep(Duration::from_secs(5));
        });
        publish_daemon_flag(&db, &addr).unwrap();
        let t0 = Instant::now();
        assert!(try_inject(&db, "{}").is_none());
        assert!(
            t0.elapsed() < CLIENT_TIMEOUT + Duration::from_millis(250),
            "bounded well under the wedge: {:?}",
            t0.elapsed()
        );
        assert!(!flagfile(&db).exists(), "wedged daemon's flag self-healed");
    }

    #[test]
    fn health_at_parses_a_direct_bind_without_flag() {
        // A fake daemon: accepts one connection, answers a canned health JSON.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let _serve = std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let body = r#"{"status":"ok","pid":42,"db":"x"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(resp.as_bytes());
            }
        });
        let h = health_at(&addr).expect("direct-bind probe answers without any flag file");
        assert_eq!(h.get("pid").and_then(|v| v.as_u64()), Some(42));
    }
}
