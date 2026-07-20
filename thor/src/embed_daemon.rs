//! Warm resident embedder for the per-prompt recall courier (feature `semantic`).
//!
//! Cold-loading the ONNX model costs ~1.25s (onnxruntime session init), which is
//! far too slow to pay inside the stateless per-prompt courier. So the model
//! lives in ONE long-running process that loads it once and answers query-embed
//! requests over a localhost socket in ~10ms. The courier asks this daemon for a
//! query vector and, if it is not (yet) reachable, degrades to bm25 for that
//! prompt and spawns the daemon detached so the NEXT prompt is warm. bm25 is
//! always the floor: nothing here can block or slow a prompt.
//!
//! Protocol: one JSON line per request `{"embed":"<text>"}` -> one JSON line
//! `{"vec":[..DIM..]}` (or `{"error":"..."}`). Localhost only; the port is chosen
//! by the OS and published in a portfile next to the store.

use crate::embed::{self, Embedder, DIM, MODEL_ID};
use anyhow::Result;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The daemon exits after this long with no request, so it never lingers holding
/// the model in RAM after a coding session ends.
const IDLE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// A client connect must be near-instant (the daemon is on localhost); if not, we
/// treat the daemon as down and fall back to bm25.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
/// Server-side read/write timeout once connected (embedding one short query is
/// ~10ms; the looser budget only guards a slow/partial client write).
const IO_TIMEOUT: Duration = Duration::from_secs(3);
/// Client-side (courier hook path) read/write budget. Deliberately tight: a warm
/// embed is ~10ms, so if a connect to the published port succeeds but no reply
/// arrives quickly the port is stale/recycled - we must fall back to bm25 fast
/// rather than stall prompt submission on a dead port.
const CLIENT_IO_TIMEOUT: Duration = Duration::from_millis(500);
/// Don't respawn a warming daemon on every prompt in a burst.
const SPAWN_DEBOUNCE: Duration = Duration::from_secs(15);

/// Where the running daemon publishes its port (next to the store).
fn portfile(db: &Path) -> PathBuf {
    db.with_file_name("thor-embedd.json")
}
/// A debounce marker written just before a detached spawn.
fn startfile(db: &Path) -> PathBuf {
    db.with_file_name("thor-embedd.starting")
}

/// Run the resident embedder. Loads the model, binds a localhost port, publishes
/// it, then serves query-embed requests until idle for `IDLE_TIMEOUT`.
pub fn run_embed_daemon(db: &Path) -> Result<()> {
    let model_dir = embed::default_model_dir().ok_or_else(|| {
        anyhow::anyhow!(
            "no per-user data directory for the model: LOCALAPPDATA, XDG_DATA_HOME and HOME are \
             all unset, so there is nowhere to load it from"
        )
    })?;
    let mut embedder = Embedder::load(&model_dir)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    std::fs::write(
        portfile(db),
        serde_json::json!({ "port": port, "pid": std::process::id(), "model_id": MODEL_ID })
            .to_string(),
    )?;
    let _ = std::fs::remove_file(startfile(db)); // we're up; clear the debounce marker

    // Idle watchdog: exit (and clean the portfile) after a quiet spell so a dead
    // session doesn't leave the model resident forever.
    let last = Arc::new(Mutex::new(Instant::now()));
    {
        let last = Arc::clone(&last);
        let pf = portfile(db);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(60));
            let idle = last.lock().map(|t| t.elapsed()).unwrap_or_default();
            if idle > IDLE_TIMEOUT {
                let _ = std::fs::remove_file(&pf);
                std::process::exit(0);
            }
        });
    }

    // Serial accept loop: courier calls are one-at-a-time and each embed is ~10ms,
    // so a single thread is plenty and keeps exactly one model in memory.
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(mut t) = last.lock() {
            *t = Instant::now();
        }
        let _ = handle_conn(&mut stream, &mut embedder); // per-conn errors never kill the loop
    }
    Ok(())
}

fn handle_conn(stream: &mut TcpStream, embedder: &mut Embedder) -> Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let resp = match serde_json::from_str::<serde_json::Value>(&line) {
        Ok(v) => {
            let text = v.get("embed").and_then(|x| x.as_str()).unwrap_or("");
            match embedder.embed_one(text) {
                Ok(vec) => serde_json::json!({ "vec": vec }),
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            }
        }
        Err(e) => serde_json::json!({ "error": format!("bad request: {e}") }),
    };
    writeln!(stream, "{}", resp)?;
    Ok(())
}

/// Ask the warm daemon to embed `text`, returning a unit-norm `DIM` vector.
/// `None` on ANY failure so the caller can fall back to bm25. On failure the
/// portfile is REMOVED so a stale one (left by a crashed daemon, or pointing at a
/// recycled port) self-heals: the next prompt spawns a fresh daemon instead of
/// paying the read budget on a dead port again.
pub fn client_embed(db: &Path, text: &str) -> Option<Vec<f32>> {
    match try_client_embed(db, text) {
        Some(v) => Some(v),
        None => {
            let _ = std::fs::remove_file(portfile(db));
            None
        }
    }
}

fn try_client_embed(db: &Path, text: &str) -> Option<Vec<f32>> {
    let raw = std::fs::read_to_string(portfile(db)).ok()?;
    let meta: serde_json::Value = serde_json::from_str(&raw).ok()?;
    // A daemon left over from a different model build must not be trusted.
    if meta.get("model_id").and_then(|v| v.as_str()) != Some(MODEL_ID) {
        return None;
    }
    let port = meta.get("port").and_then(|v| v.as_u64())? as u16;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    // Tight client budget: a wedged or recycled port must fall through to bm25 in
    // ~0.5s, not stall the prompt for the server's 3s.
    stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT)).ok()?;
    writeln!(stream, "{}", serde_json::json!({ "embed": text })).ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let resp: serde_json::Value = serde_json::from_str(&line).ok()?;
    let arr = resp.get("vec")?.as_array()?;
    let vec: Vec<f32> = arr.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
    (vec.len() == DIM).then_some(vec)
}

/// Claim the right to spawn the daemon. Exactly ONE caller in a burst wins.
///
/// The claim is the ATOMIC creation of the marker file, not a check followed by
/// a write: a burst of couriers all miss the marker within the same millisecond
/// and a plain `write` succeeds for every one of them, so each spawns its own
/// daemon (measured live: three, all started in the same second, each holding
/// the ~650 MB model). `create_new` fails with `AlreadyExists` for every loser,
/// which is the whole point.
///
/// A marker older than the debounce is stale (a previous start died before the
/// daemon published its port) and must not block starts forever, so it is
/// cleared first. Any error claiming the marker fails OPEN: a spare daemon is a
/// wart, no daemon at all is a silent drop to bm25 on every prompt.
fn claim_spawn(sf: &Path) -> bool {
    if let Ok(meta) = std::fs::metadata(sf) {
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|e| e < SPAWN_DEBOUNCE);
        if fresh {
            return false; // a start is already in flight
        }
        let _ = std::fs::remove_file(sf); // stale marker: let the claim below decide
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(sf) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false, // lost the race
        Err(_) => true, // cannot claim at all - fail open rather than never start
    }
}

/// Ensure a daemon is (being) started, detached so it outlives this courier
/// process. Debounced via a start-marker file so a burst of prompts during the
/// ~1.25s warm-up does not spawn a swarm of daemons.
pub fn ensure_daemon(db: &Path) {
    if !claim_spawn(&startfile(db)) {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = spawn_detached(&exe, db);
    }
}

// CRITICAL: the daemon MUST NOT inherit the courier's std handles. The courier is
// the UserPromptSubmit hook, whose stdout Claude Code reads to EOF; a long-lived
// daemon holding that pipe's write end would block prompt submission until it
// exits (up to IDLE_TIMEOUT). Redirecting all three streams to null severs that.
#[cfg(windows)]
fn spawn_detached(exe: &Path, db: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(exe)
        .arg("--db")
        .arg(db)
        .arg("embed-daemon")
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
        .arg("embed-daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this guards: a burst of couriers hitting a cold daemon all miss
    /// the marker in the same millisecond. Check-then-write let every one of
    /// them through (measured live: three daemons, same second, ~650 MB each).
    /// Exactly one claim may win.
    #[test]
    fn only_one_of_a_burst_may_claim_the_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let sf = dir.path().join("thor-embedd.starting");
        let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (sf, winners, barrier) = (sf.clone(), winners.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait(); // release them all at once - this is the race
                    if claim_spawn(&sf) {
                        winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            winners.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a burst must spawn exactly one daemon, not a swarm"
        );
    }

    #[test]
    fn a_fresh_marker_blocks_and_a_stale_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let sf = dir.path().join("thor-embedd.starting");

        assert!(claim_spawn(&sf), "no marker: the first caller starts the daemon");
        assert!(!claim_spawn(&sf), "marker is fresh: a start is already in flight");

        // A start that died leaves the marker behind; it must not block forever.
        let stale = std::time::SystemTime::now() - SPAWN_DEBOUNCE - Duration::from_secs(5);
        let f = std::fs::File::options().write(true).open(&sf).unwrap();
        f.set_modified(stale).unwrap();
        drop(f);
        assert!(claim_spawn(&sf), "a stale marker must not block a restart forever");
    }

    #[test]
    fn test_portfile_and_startfile_next_to_db() {
        let db = Path::new(r"C:\x\thor.db");
        assert_eq!(portfile(db).file_name().unwrap(), "thor-embedd.json");
        assert_eq!(startfile(db).file_name().unwrap(), "thor-embedd.starting");
    }

    #[test]
    fn test_client_embed_none_without_portfile() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(client_embed(&db, "anything").is_none(), "no portfile -> None (bm25 fallback)");
    }

    #[test]
    fn test_client_embed_ignores_stale_model_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        std::fs::write(
            portfile(&db),
            serde_json::json!({ "port": 1, "pid": 1, "model_id": "some-old-model" }).to_string(),
        )
        .unwrap();
        assert!(
            client_embed(&db, "x").is_none(),
            "a portfile from a different model build must be ignored"
        );
    }
}
