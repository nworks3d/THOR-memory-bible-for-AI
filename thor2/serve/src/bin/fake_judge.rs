//! TEST FIXTURE ONLY - not a real judge, never point a live session at this.
//!
//! A tiny external command used by `serve/tests/judge_transport.rs` and
//! `serve/tests/capture_guard_hook.rs` to drive `serve::judge::run_judge_command`
//! (and the whole capture-guard Stop wiring) against a REAL child process -
//! spawn, stdin, stdout, exit code, a slow or hung command - the one thing a
//! pure unit test cannot exercise, and the one thing this workspace's "no new
//! third-party dependency" constraint rules out mocking with an HTTP test
//! server.
//!
//! Usage: `fake_judge <mode> [args...]`
//!
//!   block <citation words...>       -> "VERDICT: BLOCK\nJUSTIFICATION: <citation>"
//!   warn <citation words...>        -> "VERDICT: WARN\nJUSTIFICATION: <citation>"
//!   allow                           -> "VERDICT: ALLOW\nJUSTIFICATION: nothing stood out"
//!   garbage                         -> output that does not parse as a verdict at all
//!   sleep <ms> <mode...>            -> sleeps `ms` milliseconds, THEN behaves like <mode...>
//!   sentinel <path> <mode...>       -> writes a marker file at `<path>` (proof this
//!                                      command was actually invoked), then behaves
//!                                      like <mode...>
//!   respawn <serve_exe> <db> <session_id> <counter_file> <inner_stdout_file>
//!           <inner_exit_file> <mode...>
//!                                   -> THE REENTRANCY-PROOF FIXTURE. Behaves like a
//!                                      real headless `claude` CLI whose child session
//!                                      fires THOR's own hooks: appends one line to
//!                                      `counter_file` (proof this judge command was
//!                                      actually invoked - once per real invocation,
//!                                      never once per recursion level if the guard
//!                                      holds), then re-invokes `<serve_exe> --db <db>
//!                                      hook`, feeding it a synthetic Stop payload for
//!                                      `session_id` on its OWN (inherited) environment -
//!                                      exactly the shape a nested Claude Code session's
//!                                      own Stop hook would take. That inner call's raw
//!                                      stdout and exit code are written to
//!                                      `inner_stdout_file`/`inner_exit_file` for the
//!                                      test to inspect, then this process finally
//!                                      answers like <mode...> so the REAL, outer judge
//!                                      call it is standing in for still completes
//!                                      normally. See
//!                                      `serve/tests/judge_reentrancy.rs`.
//!
//! Reads (and discards) stdin fully before doing anything else, exactly like
//! a real judge command would, so the caller's stdin write can never block on
//! an unread pipe.

use std::io::{Read, Write};

fn main() {
    let mut discard = String::new();
    let _ = std::io::stdin().read_to_string(&mut discard);

    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args);
}

fn run(args: &[String]) {
    let Some(mode) = args.first().map(String::as_str) else {
        std::process::exit(1);
    };
    match mode {
        "sentinel" => {
            let path = args.get(1).expect("sentinel mode needs a path");
            let _ = std::fs::write(path, b"invoked");
            run(&args[2..]);
        }
        "sleep" => {
            let ms: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(ms));
            run(&args[2..]);
        }
        "respawn" => {
            let serve_exe = args.get(1).expect("respawn needs a serve exe path");
            let db = args.get(2).expect("respawn needs a db path");
            let session_id = args.get(3).expect("respawn needs a session id");
            let counter_file = args.get(4).expect("respawn needs a counter-file path");
            let inner_stdout_file = args.get(5).expect("respawn needs an inner-stdout file path");
            let inner_exit_file = args.get(6).expect("respawn needs an inner-exit file path");

            // Proof this judge command was actually invoked - one line per
            // real call. If the reentrancy guard ever failed, the inner
            // `serve hook` call below would itself reach `classify_capture`
            // and spawn ANOTHER `fake_judge respawn`, appending a second line
            // here; with the guard in place, this file must end up with
            // exactly one line no matter how deep the (attempted) recursion.
            {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(counter_file)
                    .expect("open counter file");
                writeln!(f, "invoked").expect("write counter file");
            }

            // Re-invoke `serve --db <db> hook`, fed a synthetic Stop payload
            // for the SAME session - exactly the shape a real child Claude
            // Code session's own Stop hook would take if the reentrancy
            // guard did not exist. This process' own environment already
            // carries `THOR_JUDGE_DEPTH` (set on it by the real judge
            // transport that spawned THIS fake_judge process); the child
            // spawned below inherits it automatically, the same way a real
            // `claude` CLI's own child processes would.
            let stop_payload = serde_json::json!({
                "hook_event_name": "Stop",
                "session_id": session_id,
                "stop_hook_active": false,
                "last_assistant_message": "",
            })
            .to_string();

            let mut child = std::process::Command::new(serve_exe)
                .arg("--db")
                .arg(db)
                .arg("hook")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn the inner serve hook");
            if let Some(mut stdin) = child.stdin.take() {
                // Tolerant, deliberately: if the reentrancy guard is working,
                // the inner `serve hook` process exits before it ever reads
                // stdin at all, which can race this write into a broken-pipe
                // error - exactly the same tolerance `judge::run_judge_command`
                // itself uses for the SAME reason on the real transport.
                let _ = stdin.write_all(stop_payload.as_bytes());
            }
            let output = child.wait_with_output().expect("wait for the inner serve hook");

            std::fs::write(inner_stdout_file, &output.stdout).expect("write inner-stdout file");
            std::fs::write(
                inner_exit_file,
                output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string()),
            )
            .expect("write inner-exit file");

            // Finally, answer like whatever ordinary mode the caller asked
            // for, so the real, outer judge call this process stands in for
            // still completes normally.
            run(&args[7..]);
        }
        "block" => {
            let citation = args[1..].join(" ");
            println!("VERDICT: BLOCK");
            println!("JUSTIFICATION: {citation}");
        }
        "warn" => {
            let citation = args[1..].join(" ");
            println!("VERDICT: WARN");
            println!("JUSTIFICATION: {citation}");
        }
        "allow" => {
            println!("VERDICT: ALLOW");
            println!("JUSTIFICATION: nothing stood out");
        }
        "garbage" => {
            println!("this is not a verdict line at all");
        }
        other => {
            eprintln!("fake_judge: unknown mode '{other}'");
            std::process::exit(1);
        }
    }
}
