//! CLI-level proof for the defect closed in `ops::transport`
//! (`CONNECT_TIMEOUT`/`REQUEST_TIMEOUT`) and `ops::ship_state`: the real
//! `sync ship` binary, run against a real (fixture) receiver over loopback -
//! never the live store, never the NAS.
//!
//! `ops::transport`'s own unit tests already prove `push_once` itself
//! returns the right `Err`/`Ok` for each shape (wrong token, fork, ahead,
//! never-answers). What those cannot prove is that `sync.rs`'s compiled
//! `main` actually turns that `Err` into a non-zero exit and a one-line
//! stderr message, and that a successful `Ok` actually reaches
//! `ops::ship_state::record_success` - that seam is only real once both
//! binaries are built and run, which is what this file does.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use thor_core::event_store::{EventKind, EventStore};

const TOKEN: &str = "test-shared-secret";

/// A `sync recv` child process, killed on drop so a failing assertion never
/// leaves a listener running past this test.
struct Receiver(std::process::Child);

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// An OS-assigned free port: bind a throwaway listener, read the port back,
/// drop it. `sync recv --bind` cannot report back which port it actually
/// bound (it prints the literal `--bind` string, not the socket's real
/// address - see `transport::run_receiver`), so the caller has to already
/// know the port before starting it.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Start a real `sync recv` fixture receiver on `port`, backed by a fresh
/// store at `db`, and block until it actually accepts connections - spawning
/// a process is not instantaneous, and shipping against a receiver that has
/// not bound yet would fail for a reason this file is not testing.
fn start_receiver(db: &Path, port: u16) -> Receiver {
    let child = Command::new(env!("CARGO_BIN_EXE_sync"))
        .args(["recv", "--db"])
        .arg(db)
        .args(["--bind", &format!("127.0.0.1:{port}")])
        .env("THOR_SYNC_TOKEN", TOKEN)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start the fixture `sync recv` process");

    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the fixture receiver never started listening on {addr}");
        std::thread::sleep(Duration::from_millis(50));
    }
    Receiver(child)
}

/// A local store with a couple of events in it - something for `sync ship`
/// to actually have to send.
fn seed_shipper(db: &Path) {
    let mut store = EventStore::new(db).unwrap();
    let e1 = store.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "first").unwrap();
    store.append_event("s", "l", "act", EventKind::FactRevised, "e1", Some(&e1.this_hash), "second").unwrap();
}

fn run_ship(db: &Path, to: &str, token: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sync"))
        .args(["ship", "--db"])
        .arg(db)
        .args(["--to", to])
        .env("THOR_SYNC_TOKEN", token)
        .output()
        .expect("failed to run the `sync ship` process")
}

/// THE REFUSAL HALF OF THE DEFECT: a receiver that refuses the shared secret
/// must fail the CLI loudly - a non-zero exit and one line on stderr naming
/// what happened - never hang and never exit 0. This is `sync.rs`'s `main`
/// exercised for real, not just `push_once` in isolation.
#[test]
fn a_refused_ship_exits_non_zero_with_one_clear_message_and_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let receiver_db = dir.path().join("receiver.db");
    let shipper_db = dir.path().join("shipper.db");
    seed_shipper(&shipper_db);
    let port = free_port();
    let _receiver = start_receiver(&receiver_db, port);
    let to = format!("http://127.0.0.1:{port}");

    let out = run_ship(&shipper_db, &to, "the-WRONG-token");

    assert!(!out.status.success(), "a refused ship must exit non-zero, got {:?}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("401") || stderr.to_lowercase().contains("reject"),
        "stderr must name what the receiver said, not just fail silently: {stderr:?}"
    );
    assert_eq!(stderr.lines().count(), 1, "the CLI promises ONE clear line on stderr, got: {stderr:?}");
    assert!(
        ops::ship_state::read(&shipper_db).is_none(),
        "a refused ship must never be recorded as a success"
    );
}

/// THE SUCCESS HALF: a ship that actually lands records the timestamp (and
/// the receiver's own sequence) via `ops::ship_state`, so `ops::health::
/// ship_line`/`doctor` have something real to read afterwards.
#[test]
fn a_successful_ship_records_the_last_success() {
    let dir = tempfile::tempdir().unwrap();
    let receiver_db = dir.path().join("receiver.db");
    let shipper_db = dir.path().join("shipper.db");
    seed_shipper(&shipper_db);
    let port = free_port();
    let _receiver = start_receiver(&receiver_db, port);
    let to = format!("http://127.0.0.1:{port}");

    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let out = run_ship(&shipper_db, &to, TOKEN);
    let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 applied"), "both seeded events must ship: {stdout}");

    let state = ops::ship_state::read(&shipper_db).expect("a successful ship must record its state");
    assert_eq!(state.receiver_seq, 2, "the receiver's own tip after the ship, not a guess");
    assert!(
        state.completed_unix >= before && state.completed_unix <= after,
        "recorded timestamp {} must fall within [{before}, {after}]",
        state.completed_unix
    );
}

/// THE "NOTHING TO DO IS NOT A FAILURE" CASE, spelled out in the task this
/// closes: a second ship, with nothing new to send, is still a plain
/// success - never confused with a refusal.
#[test]
fn shipping_nothing_new_is_still_a_success_and_still_updates_the_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let receiver_db = dir.path().join("receiver.db");
    let shipper_db = dir.path().join("shipper.db");
    seed_shipper(&shipper_db);
    let port = free_port();
    let _receiver = start_receiver(&receiver_db, port);
    let to = format!("http://127.0.0.1:{port}");

    let first = run_ship(&shipper_db, &to, TOKEN);
    assert!(first.status.success(), "stderr: {}", String::from_utf8_lossy(&first.stderr));
    let first_state = ops::ship_state::read(&shipper_db).unwrap();

    // A tiny sleep so a second recording (if it happens) is provably later,
    // not just coincidentally equal.
    std::thread::sleep(Duration::from_millis(1100));

    let second = run_ship(&shipper_db, &to, TOKEN);
    assert!(second.status.success(), "nothing-to-ship must still exit 0: stderr {}", String::from_utf8_lossy(&second.stderr));
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(stdout.contains("0 applied"), "the second ship has nothing new to send: {stdout}");

    let second_state = ops::ship_state::read(&shipper_db).unwrap();
    assert!(
        second_state.completed_unix > first_state.completed_unix,
        "a successful no-op ship must still refresh the timestamp: first {}, second {} (slept 1.1s in between)",
        first_state.completed_unix,
        second_state.completed_unix
    );
}
