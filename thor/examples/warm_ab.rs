//! The adoption gate for the resident cache, measured on WHAT COUNTS.
//!
//! Adoption rule: ZERO regression on what weighs heavy (drift-catch,
//! correctness), minimal on what weighs light. So this does not measure recall
//! internals - it measures the INJECTION BLOCK the agent actually receives,
//! through the real production entry points:
//!
//!   cold = courier::injection_for_hook_json_warm      (what ships today)
//!   warm = courier::injection_for_hook_json_resident  (the daemon's path)
//!
//! If the injected text is byte-identical, then drift-catch, recall coverage
//! and every judged metric derived from it are identical BY CONSTRUCTION - no
//! jury needed to prove a no-op. Any difference is a regression and is printed.
//!
//! Also drives the case a cache is most likely to break: a WRITE mid-run, after
//! which the warm path must still match cold.
//!
//! Run: cargo run --release --features semantic --example warm_ab

use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;
use thor::event_store::{EventKind, EventStore};
use thor::recall::WarmRecall;

fn local(sub: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"));
    p.push("thor");
    for s in sub {
        p.push(s);
    }
    p
}

#[derive(Deserialize)]
struct LiveScenario {
    drift_prompt: String,
}

#[derive(Deserialize)]
struct QueryItem {
    query: String,
}

fn pct(a: usize, b: usize) -> String {
    if b == 0 {
        return "n/a".into();
    }
    format!("{:.1}%", 100.0 * a as f64 / b as f64)
}

fn main() -> anyhow::Result<()> {
    let db = local(&["thor.db"]);
    let store = EventStore::new(&db)?;

    // Both channels the agent actually sees.
    let drift: Vec<String> =
        match std::fs::File::open(local(&["eval", "drift_scenarios.json"])) {
            Ok(f) => serde_json::from_reader::<_, Vec<LiveScenario>>(f)?
                .into_iter()
                .map(|s| s.drift_prompt)
                .collect(),
            Err(_) => vec![],
        };
    let mut recall_q: Vec<String> = vec![];
    for f in ["percategory_queries.json", "queries_full.json"] {
        if let Ok(file) = std::fs::File::open(local(&["eval", f])) {
            if let Ok(items) = serde_json::from_reader::<_, Vec<QueryItem>>(file) {
                recall_q.extend(items.into_iter().map(|q| q.query));
            }
        }
    }
    recall_q.truncate(120);

    // A real project cwd so scoping behaves like a live session.
    let cwd = std::env::current_dir()?;
    let cwd_s = cwd.to_string_lossy().to_string();

    let vpath = thor::vectors::default_vectors_path(&db);
    let vecs = thor::vectors::VectorStore::open(&vpath)?;
    let mut warm = WarmRecall::build(&store, vecs)?;

    // VALIDITY GATE. try_semantic_recall silently returns None when the embed
    // daemon is not up, and the courier then serves BM25 - on BOTH arms, with
    // the cache never touched. That run reports a perfect 0% difference and
    // means NOTHING. So: bring the daemon up, wait for it, and refuse to
    // measure until a real query vector comes back.
    //
    // NEVER call embed_daemon::ensure_daemon() from an example. It spawns
    // `std::env::current_exe() --db <db> embed-daemon`; in a binary that is
    // thor.exe (which handles that subcommand), but in an EXAMPLE it is this
    // harness, which ignores the args, re-runs main(), calls ensure_daemon
    // again... and fork-bombs the machine. Measured the hard way: 780 stray
    // processes. Launch the real binary explicitly instead.
    let thor_exe = std::env::current_exe()?
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("thor.exe"))
        .filter(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("thor.exe not found next to the examples dir"))?;
    if thor::embed_daemon::client_embed(&db, "probe").is_none() {
        std::process::Command::new(&thor_exe)
            .arg("--db")
            .arg(&db)
            .arg("embed-daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    }
    let mut ready = false;
    for _ in 0..60 {
        if thor::embed_daemon::client_embed(&db, "daemon readiness probe").is_some() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    anyhow::ensure!(
        ready,
        "embed daemon never came up: the courier would fall back to bm25 on BOTH arms, \
         the resident cache would never be exercised, and this harness would report a \
         meaningless 0% difference. Refusing to measure."
    );
    println!("embed daemon: UP (semantic path live - the cache is actually exercised)\n");

    let run = |warm: &mut WarmRecall, prompt: &str| -> (Option<String>, Option<String>, f64, f64) {
        let raw = json!({ "prompt": prompt, "cwd": cwd_s }).to_string();
        let t0 = Instant::now();
        let cold = thor::courier::injection_for_hook_json_warm(&store, &db, &raw);
        let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let t1 = Instant::now();
        let hot = thor::courier::injection_for_hook_json_resident(&store, &db, &raw, warm);
        let warm_ms = t1.elapsed().as_secs_f64() * 1000.0;
        (cold, hot, cold_ms, warm_ms)
    };

    let mut report: Vec<(String, usize, usize, usize, f64, f64)> = vec![];

    for (label, prompts) in [("DRIFT (heavy)", &drift), ("RECALL (medium)", &recall_q)] {
        if prompts.is_empty() {
            continue;
        }
        let (mut same, mut diff, mut nonempty) = (0usize, 0usize, 0usize);
        let (mut c_tot, mut w_tot) = (0.0f64, 0.0f64);
        let mut examples: Vec<String> = vec![];
        for p in prompts.iter() {
            let (cold, hot, cms, wms) = run(&mut warm, p);
            c_tot += cms;
            w_tot += wms;
            if cold.is_some() {
                nonempty += 1;
            }
            if cold == hot {
                same += 1;
            } else {
                diff += 1;
                if examples.len() < 3 {
                    examples.push(format!(
                        "    prompt: {:?}\n      cold: {:?}\n      warm: {:?}",
                        &p.chars().take(60).collect::<String>(),
                        cold.as_ref().map(|s| s.chars().take(80).collect::<String>()),
                        hot.as_ref().map(|s| s.chars().take(80).collect::<String>()),
                    ));
                }
            }
        }
        let n = prompts.len();
        println!("== {label} ==");
        println!("  prompts            : {n}");
        println!("  courier spoke      : {nonempty} ({})", pct(nonempty, n));
        println!("  injection IDENTICAL: {same} ({})", pct(same, n));
        println!("  injection DIFFERENT: {diff} ({})", pct(diff, n));
        println!(
            "  latency            : cold {:.1}ms -> warm {:.1}ms  ({:+.1}%)",
            c_tot / n as f64,
            w_tot / n as f64,
            100.0 * (w_tot - c_tot) / c_tot
        );
        for e in &examples {
            println!("  DIFF EXAMPLE:\n{e}");
        }
        println!();
        report.push((label.into(), n, same, diff, c_tot / n as f64, w_tot / n as f64));
    }

    // The cache-breaking case: write mid-run, then demand warm still matches.
    println!("== MID-RUN WRITE (the parked risk) ==");
    let mut store_w = EventStore::new(&db)?;
    let pid = "The-AI-memory-bible:mem-warmab-probe-DELETEME";
    let r1 = store_w.append_event(
        "warm-ab", "warm-ab", "warm-ab-harness", EventKind::FactCreated, pid, None,
        "WARM AB PROBE: kwiknip zorbex unique marker. Written mid-run to invalidate the resident cache.",
    )?;
    let probes = ["kwiknip zorbex unique marker", "wat is de kwiknip zorbex marker"];
    let (mut same, mut diff) = (0usize, 0usize);
    for p in probes.iter() {
        let (c, h, _, _) = run(&mut warm, p);
        if c == h { same += 1 } else { diff += 1 }
    }
    let r2 = store_w.append_event(
        "warm-ab", "warm-ab", "warm-ab-harness", EventKind::FactRevised, pid, Some(&r1.this_hash),
        "WARM AB PROBE v2: kwiknip zorbex unique marker REVISED. A stale cache would serve v1.",
    )?;
    for p in probes.iter() {
        let (c, h, _, _) = run(&mut warm, p);
        if c == h { same += 1 } else { diff += 1 }
    }
    store_w.append_event(
        "warm-ab", "warm-ab", "warm-ab-harness", EventKind::FactRetracted, pid, Some(&r2.this_hash),
        "[retracted: warm-ab probe cleanup]",
    )?;
    let after: Vec<String> =
        probes.iter().map(|s| s.to_string()).chain(recall_q.iter().take(10).cloned()).collect();
    for p in &after {
        let (c, h, _, _) = run(&mut warm, p);
        if c == h { same += 1 } else { diff += 1 }
    }
    println!("  after create/revise/retract: identical {same}, different {diff}\n");

    let (reuses, rebuilds) = warm.stats();
    println!("cache usage: {reuses} reuses, {rebuilds} rebuilds");
    assert!(
        reuses > rebuilds,
        "VACUOUS BENCHMARK: the cache rebuilt ({rebuilds}) at least as often as it was reused          ({reuses}) - the warm arm is just the cold arm plus overhead, so its timings mean nothing"
    );

    println!("{}", "=".repeat(64));
    let total_diff: usize = report.iter().map(|r| r.3).sum::<usize>() + diff;
    if total_diff == 0 {
        println!("VERDICT: 0 injection differences on every channel.");
        println!("         Drift-catch and recall quality are IDENTICAL by construction.");
    } else {
        println!("VERDICT: {total_diff} differences - REGRESSION, do not adopt.");
    }
    Ok(())
}
