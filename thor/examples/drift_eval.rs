//! Reproducible drift eval (roadmap phase 0): at the start of a fresh session,
//! does THOR's AUTOMATIC injection surface the stored fact that would prevent
//! the agent's imminent mistake?
//!
//! Committed mode (default): `thor/eval/drift_scenarios.jsonl`, a synthetic
//! public corpus (fictional projects, mixed NL/EN like the real store). Every
//! scenario seeds its own throwaway temp store and then drives the REAL
//! production paths - `courier::injection_for_hook_json` for the prompt channel
//! and the guard's file-memory advisory for the file-touch channel - so a number
//! here measures the code that runs in the hooks, never a reimplementation.
//! Scores, per channel:
//! - preventer-surfaced: the preventer's entity id appears in the injected block;
//! - full-catch: the preventer's own line also carries EVERY match_term
//!   (case-insensitive), i.e. the actionable half survived snippet truncation.
//! This is a measurement instrument, not a target: low numbers are information.
//! Several scenarios are deliberately near-zero-overlap (the hardest drift
//! class) and are expected to miss on the courier channel.
//!
//! Live mode (`--live <path>`): the PRIVATE corpus (`{seq, drift_prompt, gold,
//! category}`) against the real store at the default db path, to reproduce the
//! published numbers. Read-only by construction: no `session_id` is passed (the
//! courier ledger only writes under a session identity) and nothing is ever
//! appended; opening the store runs the same idempotent pragmas as every hook
//! invocation. Each prompt runs scoped to the gold fact's own project via a
//! temp `.thor`-marker dir, matching the published "THOR scoped to the project"
//! setup; `--cwd <dir>` overrides that with a real project directory. The
//! published metric was LLM-judged, so live mode reports the mechanical
//! entity-id hit (primary) plus gold key-term coverage (proxy) - expect the
//! judged 54.8% / 39.7% to be bracketed, not hit exactly.
//!
//! Run:  cargo run --example drift_eval              (human table)
//!       cargo run --example drift_eval -- --json    (machine-readable)
//!       cargo run --example drift_eval -- --live "%LOCALAPPDATA%/thor/eval/drift_scenarios.json"

use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thor::event_store::{EventKind, EventStore};

// ---- committed corpus ----------------------------------------------------------

#[derive(Deserialize)]
struct SeedFact {
    entity_id: String,
    body: String,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
struct SeedChunk {
    entity_id: String,
    body: String,
}

#[derive(Deserialize)]
struct Scenario {
    id: String,
    task_prompt: String,
    #[serde(default)]
    seed_facts: Vec<SeedFact>,
    #[serde(default)]
    seed_chunks: Vec<SeedChunk>,
    preventer_id: String,
    match_terms: Vec<String>,
    channel_hint: String,
    #[serde(default)]
    guard_file: Option<String>,
}

#[derive(Clone, Copy)]
struct ChannelScore {
    surfaced: bool,
    slot: Option<usize>,
    full: bool,
}

impl ChannelScore {
    fn miss() -> Self {
        ChannelScore { surfaced: false, slot: None, full: false }
    }
    fn hit(slot: usize, full: bool) -> Self {
        ChannelScore { surfaced: true, slot: Some(slot), full }
    }
    fn json(&self) -> serde_json::Value {
        json!({ "surfaced": self.surfaced, "slot": self.slot, "full": self.full })
    }
}

struct ScenarioResult {
    id: String,
    hint: String,
    courier: ChannelScore,
    guard: Option<ChannelScore>,
}

/// Case-insensitive: every match_term must appear in the text the agent sees.
fn contains_all(text: &str, terms: &[String]) -> bool {
    let lower = text.to_lowercase();
    !terms.is_empty() && terms.iter().all(|t| lower.contains(&t.to_lowercase()))
}

/// A courier hit line is `- [scope][type] <entity_id> (<rev>...): <snippet>`;
/// keying on `" <id> ("` means an id can never match as a substring of a longer
/// id, and the suppressed-top stub / contested-head sublines never count as a
/// slot. Slot = 1-based position among the hit lines.
fn score_courier(block: Option<&str>, preventer: &str, terms: &[String]) -> ChannelScore {
    let Some(block) = block else { return ChannelScore::miss() };
    let needle = format!(" {} (", preventer);
    for (i, line) in block.lines().filter(|l| l.starts_with("- ")).enumerate() {
        if line.contains(&needle) {
            return ChannelScore::hit(i + 1, contains_all(line, terms));
        }
    }
    ChannelScore::miss()
}

/// A guard advisory is `...: [type] <entity_id>: <snippet>  ||  ...`; entries
/// are `"  ||  "`-separated, the id is `": "`-terminated.
fn score_guard(advisory: Option<&str>, preventer: &str, terms: &[String]) -> ChannelScore {
    let Some(adv) = advisory else { return ChannelScore::miss() };
    let needle = format!("{}: ", preventer);
    for (i, entry) in adv.split("  ||  ").enumerate() {
        if entry.contains(&needle) {
            return ChannelScore::hit(i + 1, contains_all(entry, terms));
        }
    }
    ChannelScore::miss()
}

/// A project key that is not the global tier, read off an entity id's prefix.
fn non_global_owner(entity_id: &str) -> Option<String> {
    thor::repo::owner_project(entity_id)
        .filter(|p| !thor::repo::is_global(Some(p)))
        .map(str::to_string)
}

/// The project the scenario's session "is in" (decides the hook cwd, and with
/// it the recall scope): the preventer's own project when it has one, else the
/// first project-scoped seed - so a global preventer still competes against the
/// current project's chunks and memories, exactly like a real session.
fn scenario_project(s: &Scenario) -> Option<String> {
    non_global_owner(&s.preventer_id)
        .or_else(|| s.seed_facts.iter().find_map(|f| non_global_owner(&f.entity_id)))
        .or_else(|| s.seed_chunks.iter().find_map(|c| non_global_owner(&c.entity_id)))
}

/// Corpus sanity: a silently mis-scoped seed would make a scenario unwinnable
/// and read as a recall regression, so a broken line fails loudly instead.
fn validate(s: &Scenario) -> anyhow::Result<()> {
    let fail = |msg: String| anyhow::bail!("scenario {}: {}", s.id, msg);
    if s.match_terms.is_empty() {
        return fail("match_terms must not be empty".into());
    }
    match s.channel_hint.as_str() {
        "courier" => {}
        "guard" if s.guard_file.is_some() => {}
        "guard" => return fail("channel_hint guard requires guard_file".into()),
        other => return fail(format!("unknown channel_hint '{}'", other)),
    }
    let mut seen = std::collections::HashSet::new();
    for f in &s.seed_facts {
        if f.project.as_deref() != thor::repo::owner_project(&f.entity_id) {
            return fail(format!(
                "fact {} declares project {:?} but its id prefix says {:?}",
                f.entity_id,
                f.project,
                thor::repo::owner_project(&f.entity_id)
            ));
        }
        if !seen.insert(f.entity_id.as_str()) {
            return fail(format!("duplicate seed id {}", f.entity_id));
        }
    }
    for c in &s.seed_chunks {
        if !thor::repo::is_chunk_id(&c.entity_id) {
            return fail(format!("{} is not a chunk id (<project>:<rel>#<n>)", c.entity_id));
        }
        if !seen.insert(c.entity_id.as_str()) {
            return fail(format!("duplicate seed id {}", c.entity_id));
        }
    }
    if !seen.contains(s.preventer_id.as_str()) {
        return fail(format!("preventer {} is not among the seeds", s.preventer_id));
    }
    // 5-15 distractors: enough that ranking is actually exercised, bounded so
    // one scenario cannot dominate runtime.
    let distractors = seen.len() - 1;
    if !(5..=15).contains(&distractors) {
        return fail(format!("{} distractors (must be 5-15)", distractors));
    }
    Ok(())
}

fn run_scenario(s: &Scenario) -> anyhow::Result<ScenarioResult> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("thor.db");
    {
        let mut store = EventStore::new(&db)?;
        for f in &s.seed_facts {
            store.append_event("eval", "eval", "drift-eval", EventKind::FactCreated, &f.entity_id, None, &f.body)?;
        }
        for c in &s.seed_chunks {
            store.append_event("eval", "eval", "drift-eval", EventKind::FactCreated, &c.entity_id, None, &c.body)?;
        }
    } // dropped: the courier/guard open their own handle, like the real hooks

    // The hook cwd decides the recall scope: a .thor-marker dir for the
    // scenario's project, or a bare scratch dir (global-only) when everything
    // seeded is global - the two situations a real session can be in.
    let cwd = match scenario_project(s) {
        Some(key) => {
            let p = dir.path().join("proj");
            std::fs::create_dir_all(&p)?;
            std::fs::write(p.join(".thor"), format!("{}\n", key))?;
            p
        }
        None => {
            let p = dir.path().join("scratch");
            std::fs::create_dir_all(&p)?;
            p
        }
    };
    let cwd_str = cwd.to_string_lossy();

    // Courier channel: the exact JSON the UserPromptSubmit hook receives.
    let raw = json!({ "prompt": s.task_prompt, "session_id": "eval", "cwd": cwd_str }).to_string();
    let block = thor::courier::injection_for_hook_json(&db, &raw);
    let courier = score_courier(block.as_deref(), &s.preventer_id, &s.match_terms);

    // Guard channel: the first touch of the constrained file this session
    // (PreToolUse hook JSON). The file need not exist - the advisory keys on
    // the path, which is all a real Edit-before-create carries too.
    let guard = s.guard_file.as_deref().map(|gf| {
        let hook = json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": cwd.join(gf).to_string_lossy() },
            "session_id": format!("eval-{}", s.id),
            "cwd": cwd_str,
        });
        let adv = thor::guard::file_memory_advisory_for_eval(&db, &hook);
        score_guard(adv.as_deref(), &s.preventer_id, &s.match_terms)
    });

    Ok(ScenarioResult { id: s.id.clone(), hint: s.channel_hint.clone(), courier, guard })
}

#[derive(Default)]
struct Tally {
    n: usize,
    surfaced: usize,
    full: usize,
}

impl Tally {
    fn add(&mut self, surfaced: bool, full: bool) {
        self.n += 1;
        self.surfaced += surfaced as usize;
        self.full += full as usize;
    }
    fn json(&self) -> serde_json::Value {
        json!({
            "n": self.n,
            "surfaced": self.surfaced,
            "surfaced_pct": pct(self.surfaced, self.n),
            "full": self.full,
            "full_pct": pct(self.full, self.n),
        })
    }
}

fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 { 0.0 } else { (part as f64 * 1000.0 / whole as f64).round() / 10.0 }
}

fn fmt_score(c: &ChannelScore) -> String {
    match (c.surfaced, c.slot, c.full) {
        (true, Some(slot), true) => format!("slot {} FULL", slot),
        (true, Some(slot), false) => format!("slot {} partial", slot),
        _ => "miss".to_string(),
    }
}

fn run_committed(json_out: bool) -> anyhow::Result<()> {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("eval").join("drift_scenarios.jsonl");
    let raw = std::fs::read_to_string(&corpus)?;
    let scenarios: Vec<Scenario> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(scenarios.len() >= 30, "corpus shrank below 30 scenarios");
    for s in &scenarios {
        validate(s)?;
    }

    let mut results = Vec::with_capacity(scenarios.len());
    for s in &scenarios {
        results.push(run_scenario(s)?);
    }

    // Courier runs on every scenario; guard on those naming a file; "either"
    // is the union an actual session gets (both hooks are installed at once).
    let (mut courier, mut guard, mut either) = (Tally::default(), Tally::default(), Tally::default());
    for r in &results {
        courier.add(r.courier.surfaced, r.courier.full);
        if let Some(g) = &r.guard {
            guard.add(g.surfaced, g.full);
        }
        let g = r.guard.as_ref();
        either.add(
            r.courier.surfaced || g.map_or(false, |g| g.surfaced),
            r.courier.full || g.map_or(false, |g| g.full),
        );
    }

    if json_out {
        let out = json!({
            "mode": "committed",
            "n": results.len(),
            "channels": { "courier": courier.json(), "guard": guard.json(), "either": either.json() },
            "scenarios": results.iter().map(|r| json!({
                "id": r.id,
                "hint": r.hint,
                "courier": r.courier.json(),
                "guard": r.guard.as_ref().map(|g| g.json()),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("THOR drift eval - committed corpus ({} scenarios)", results.len());
    println!("surfaced = preventer id in the injected block; FULL = its line carries every match_term\n");
    println!("{:26} {:8} {:16} {:16}", "id", "hint", "courier", "guard");
    for r in &results {
        println!(
            "{:26} {:8} {:16} {:16}",
            r.id,
            r.hint,
            fmt_score(&r.courier),
            r.guard.as_ref().map(|g| fmt_score(g)).unwrap_or_else(|| "-".to_string()),
        );
    }
    println!();
    println!("{:10} {:>10} {:>22} {:>16}", "channel", "scenarios", "preventer-surfaced", "full-catch");
    for (name, t) in [("courier", &courier), ("guard", &guard), ("either", &either)] {
        println!(
            "{:10} {:>10} {:>15} {:4.1}% {:>10} {:4.1}%",
            name, t.n, t.surfaced, pct(t.surfaced, t.n), t.full, pct(t.full, t.n)
        );
    }
    Ok(())
}

// ---- live mode (private corpus, real store) ------------------------------------

#[derive(Deserialize)]
struct LiveScenario {
    seq: i64,
    drift_prompt: String,
    gold: String,
    #[serde(default)]
    category: String,
}

/// Key terms of a gold description: lowercase alphanumeric tokens of >= 4
/// chars, deduped. Crude but stable; short function words drop out on length
/// alone, so no stopword list can drift out of sync with recall's.
fn key_terms(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 4)
        .map(|t| t.to_lowercase())
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

fn run_live(corpus: &Path, cwd_override: Option<&Path>, json_out: bool) -> anyhow::Result<()> {
    let db = thor::ledger::data_dir()
        .ok_or_else(|| anyhow::anyhow!("no per-user data dir resolvable"))?
        .join("thor.db");
    // Never create a store at the default path from an eval run.
    anyhow::ensure!(db.exists(), "no live store at {} - live mode never creates one", db.display());
    let scenarios: Vec<LiveScenario> = serde_json::from_reader(std::fs::File::open(corpus)?)?;

    // One pass over the log: seq -> entity (the gold pointer; entity-level
    // matching, so a later revision of the gold still counts) and the
    // effective project per entity (to scope each prompt like the published run).
    let store = EventStore::new(&db)?;
    let events = store.get_all_events()?;
    let seq_to_entity: HashMap<i64, String> =
        events.iter().map(|e| (e.seq, e.entity_id.clone())).collect();
    let projects = thor::cas::compute_projects(&events);
    drop(store);

    // Synthetic .thor-marker dirs, one per gold project, so the courier scopes
    // exactly as a session in that project would.
    let dirs = tempfile::tempdir()?;
    let scratch = dirs.path().join("scratch");
    std::fs::create_dir_all(&scratch)?;
    let mut proj_dirs: HashMap<String, PathBuf> = HashMap::new();

    struct Row {
        seq: i64,
        category: String,
        surfaced: bool,
        coverage: f64,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut skipped = 0usize;

    for s in &scenarios {
        let Some(entity) = seq_to_entity.get(&s.seq) else {
            skipped += 1; // gold seq not in this store: count it, never guess
            continue;
        };
        let project = projects.get(entity).cloned().flatten();
        let cwd: PathBuf = if let Some(c) = cwd_override {
            c.to_path_buf()
        } else if let Some(key) = &project {
            if !proj_dirs.contains_key(key) {
                let safe: String =
                    key.chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
                let p = dirs.path().join(format!("proj-{}", safe));
                std::fs::create_dir_all(&p)?;
                std::fs::write(p.join(".thor"), format!("{}\n", key))?;
                proj_dirs.insert(key.clone(), p);
            }
            proj_dirs[key].clone()
        } else {
            scratch.clone()
        };

        // NO session_id: the courier's ledger only writes under a session
        // identity, so the live sidecars stay untouched (read-only contract).
        let raw = json!({ "prompt": s.drift_prompt, "cwd": cwd.to_string_lossy() }).to_string();
        let block = thor::courier::injection_for_hook_json(&db, &raw).unwrap_or_default();
        let surfaced = block.contains(&format!(" {} (", entity));
        let terms = key_terms(&s.gold);
        let lower = block.to_lowercase();
        let hit = terms.iter().filter(|t| lower.contains(t.as_str())).count();
        let coverage = if terms.is_empty() { 0.0 } else { hit as f64 / terms.len() as f64 };
        rows.push(Row { seq: s.seq, category: s.category.clone(), surfaced, coverage });
    }

    let mut cats: Vec<String> = rows.iter().map(|r| r.category.clone()).collect();
    cats.sort();
    cats.dedup();
    let summarize = |rows: &[&Row]| -> serde_json::Value {
        let n = rows.len();
        let surfaced = rows.iter().filter(|r| r.surfaced).count();
        let half = rows.iter().filter(|r| r.coverage >= 0.5).count();
        let mean = if n == 0 { 0.0 } else { rows.iter().map(|r| r.coverage).sum::<f64>() / n as f64 };
        json!({
            "n": n,
            "entity_surfaced": surfaced,
            "entity_surfaced_pct": pct(surfaced, n),
            "gold_terms_half_pct": pct(half, n),
            "gold_terms_mean_coverage": (mean * 1000.0).round() / 1000.0,
        })
    };

    let all: Vec<&Row> = rows.iter().collect();
    let overall = summarize(&all);
    let per_cat: serde_json::Map<String, serde_json::Value> = cats
        .iter()
        .map(|c| {
            let sel: Vec<&Row> = rows.iter().filter(|r| &r.category == c).collect();
            (c.clone(), summarize(&sel))
        })
        .collect();

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "mode": "live",
                "db": db.to_string_lossy(),
                "skipped_missing_seq": skipped,
                "overall": overall,
                "per_category": per_cat,
                "scenarios": rows.iter().map(|r| json!({
                    "seq": r.seq, "category": r.category,
                    "surfaced": r.surfaced,
                    "coverage": (r.coverage * 1000.0).round() / 1000.0,
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }

    println!("THOR drift eval - LIVE store ({} scenarios, {} skipped: seq not in store)", rows.len(), skipped);
    println!("entity-surfaced = the gold fact's entity id is in the injection (mechanical);");
    println!("gold-term metrics proxy the judged score. Published (judged): surfaced 54.8%, full-catch 39.7%.\n");
    println!("{:10} {:>4} {:>18} {:>16} {:>14}", "category", "n", "entity-surfaced", "terms>=50%", "mean coverage");
    let print_row = |name: &str, v: &serde_json::Value| {
        println!(
            "{:10} {:>4} {:>12} {:4.1}% {:>10.1}% {:>13.3}",
            name,
            v["n"],
            v["entity_surfaced"],
            v["entity_surfaced_pct"].as_f64().unwrap_or(0.0),
            v["gold_terms_half_pct"].as_f64().unwrap_or(0.0),
            v["gold_terms_mean_coverage"].as_f64().unwrap_or(0.0),
        );
    };
    for c in &cats {
        print_row(c, &per_cat[c]);
    }
    print_row("overall", &overall);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut json_out = false;
    let mut live: Option<PathBuf> = None;
    let mut cwd_override: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => json_out = true,
            "--live" => {
                let p = args.next().ok_or_else(|| anyhow::anyhow!("--live needs a corpus path"))?;
                live = Some(PathBuf::from(p));
            }
            "--cwd" => {
                let p = args.next().ok_or_else(|| anyhow::anyhow!("--cwd needs a directory"))?;
                cwd_override = Some(PathBuf::from(p));
            }
            other => anyhow::bail!("unknown argument '{}' (expected --json, --live <path>, --cwd <dir>)", other),
        }
    }
    match live {
        Some(corpus) => run_live(&corpus, cwd_override.as_deref(), json_out),
        None => run_committed(json_out),
    }
}
