//! The latency harness for the capture guard's judge transport
//! (`serve::judge`). Measures the REAL wall-clock round trip of a
//! CONFIGURED judge command - at least 20 calls, reporting min/median/p95/max
//! milliseconds, plus the measured prompt and response sizes in characters.
//!
//! This does NOT estimate, simulate, or otherwise fabricate a latency number.
//! If no judge config exists on this machine, or the configured command
//! cannot even be spawned, this prints that plainly and reports latency as
//! UNMEASURED - see `JUDGE-TRANSPORT.md`'s own latency section for exactly
//! that outcome, run on this machine, on 2026-08-05: there is no
//! `ANTHROPIC_API_KEY` and no `claude` CLI here, so nothing was measured.
//! This harness exists so the number can be produced the moment a real
//! transport is configured, without writing a new tool at that time.
//!
//! Run (from the `thor2` workspace root):
//!   cargo run -p serve --example judge_latency
//!
//! Overridable via env vars (no config file need be edited to try a
//! different one):
//!   JUDGE_LATENCY_CONFIG - path to a judge config JSON (default: the
//!     repo-root `guard-judge-config.example.json`, DOCUMENTED-BUT-UNVERIFIED
//!     - see that file's own comment).
//!   JUDGE_LATENCY_PROMPT - the one prompt text sent on every call (default:
//!     a generic, synthetic durable-decision sentence, never a real one).
//!   JUDGE_LATENCY_CALLS - how many calls to make (default 20; the SPEC's own
//!     floor - this harness refuses to report a percentile off fewer than
//!     that, see `MIN_CALLS` below).

use serve::judge;
use std::path::PathBuf;
use std::time::Instant;

const MIN_CALLS: usize = 20;
const DEFAULT_PROMPT: &str =
    "from now on always run the linter before every commit, no exceptions, this is now the standing rule";

fn config_path() -> PathBuf {
    std::env::var("JUDGE_LATENCY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../guard-judge-config.example.json")))
}

fn prompt_text() -> String {
    std::env::var("JUDGE_LATENCY_PROMPT").unwrap_or_else(|_| DEFAULT_PROMPT.to_string())
}

fn call_count() -> usize {
    std::env::var("JUDGE_LATENCY_CALLS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n.max(MIN_CALLS))
        .unwrap_or(MIN_CALLS)
}

/// p-th percentile (0.0-1.0) of an already-sorted slice, nearest-rank -
/// simple and adequate for a small, human-read report; not a statistics
/// library, and this harness does not need one.
fn percentile(sorted_ms: &[u128], p: f64) -> u128 {
    if sorted_ms.is_empty() {
        return 0;
    }
    let rank = ((p * sorted_ms.len() as f64).ceil() as usize).clamp(1, sorted_ms.len());
    sorted_ms[rank - 1]
}

fn main() {
    let config_path = config_path();
    let prompt = prompt_text();
    let n = call_count();

    println!("Judge transport latency harness (JUDGE-TRANSPORT.md)");
    println!("config   : {}", config_path.display());
    println!("prompt   : {} char(s) (measured)", prompt.chars().count());
    println!("calls    : {n} (minimum {MIN_CALLS} per the design brief)");
    println!();

    let Ok(config_text) = std::fs::read_to_string(&config_path) else {
        println!("No judge config file found at that path.");
        println!();
        println!("LATENCY: UNMEASURED. Nothing was called - there is no configured judge transport");
        println!("on this machine to measure. This harness is ready to produce a real min/median/p95/max");
        println!("the moment a working `guard-judge-config.json` (or a path passed via");
        println!("JUDGE_LATENCY_CONFIG) points at a real, spawnable command.");
        return;
    };
    let Some(config) = judge::parse_judge_config(&config_text) else {
        println!("The config file at that path is not a usable judge config (missing/empty 'command',");
        println!("or not valid JSON).");
        println!();
        println!("LATENCY: UNMEASURED. Same reason as a missing config: nothing to call.");
        return;
    };
    println!("command  : {} {:?} (timeout_ms {})", config.command, config.args, config.timeout_ms);
    println!();

    let mut wall_clock_ms: Vec<u128> = Vec::new();
    let mut response_chars: Vec<usize> = Vec::new();
    let mut failures = 0usize;

    for i in 0..n {
        let start = Instant::now();
        let raw = judge::run_judge_command(&config, &prompt);
        let elapsed = start.elapsed();
        match raw {
            Some(text) => {
                wall_clock_ms.push(elapsed.as_millis());
                response_chars.push(text.chars().count());
                eprintln!("  call {:>2}/{n}: {} ms", i + 1, elapsed.as_millis());
            }
            None => {
                failures += 1;
                eprintln!("  call {:>2}/{n}: no response (failed to spawn, timed out, or non-UTF8 output)", i + 1);
            }
        }
    }

    println!();
    if wall_clock_ms.is_empty() {
        println!("Every one of the {n} call(s) failed to produce a response (could not spawn the");
        println!("command, or it never answered before its own timeout - {failures}/{n} failure(s)).");
        println!();
        println!("LATENCY: UNMEASURED. A config file exists and parses, but the configured command");
        println!("itself is not reachable/working on this machine right now, so there is no real");
        println!("round trip to report a number for.");
        return;
    }

    let mut sorted = wall_clock_ms.clone();
    sorted.sort_unstable();
    let min = sorted.first().copied().unwrap_or(0);
    let max = sorted.last().copied().unwrap_or(0);
    let median = percentile(&sorted, 0.5);
    let p95 = percentile(&sorted, 0.95);

    let min_chars = response_chars.iter().min().copied().unwrap_or(0);
    let max_chars = response_chars.iter().max().copied().unwrap_or(0);
    let avg_chars = response_chars.iter().sum::<usize>() as f64 / response_chars.len() as f64;

    println!("successful calls : {}/{n} ({failures} failed/timed out)", wall_clock_ms.len());
    println!("wall-clock ms    : min {min}  median {median}  p95 {p95}  max {max}");
    println!(
        "response size    : min {min_chars} char(s)  avg {avg_chars:.1} char(s)  max {max_chars} char(s) (measured)"
    );
    println!();
    println!(
        "LATENCY: MEASURED on this machine, this run, against the command configured above. Not an \
         estimate, not a simulation."
    );
}
