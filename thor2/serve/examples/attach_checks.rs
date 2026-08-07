//! ONE-OFF working tool (2026-08-06): attach a machine-runnable check to
//! items that were declared with prose alone.
//!
//! WHY A TOOL AND NOT A HUNDRED TOOL CALLS. Proof coverage on the owner's
//! store stood at 3 of 3004: the enforcement layer worked perfectly and
//! applied to almost nothing. Raising that is per-fact work by design (the
//! plan forbids bridging the whole store at once, because that is how the
//! noise comes back), but "per fact" means the DECISION is per fact, not
//! the typing. The decision is made outside this program - a scanner
//! proposes a literal and verifies it is really in the file right now - and
//! this only applies what was already verified.
//!
//! EVERY WRITE GOES THROUGH THE NORMAL GATE. `model::store::revise` is the
//! same call the agent's own `revise` tool makes, so a refusal here is the
//! gate doing its job and is reported, never worked around. Nothing in this
//! file writes to the log directly.
//!
//! Dry run by default. Set ATTACH_APPLY=1 to write.
//!
//!   ATTACH_PLAN=plan.json ATTACH_DB=<copy of thor.db> \
//!     cargo run --release -p serve --example attach_checks

use model::item::{Binding, Check, TargetKind};
use model::store;
use serde::Deserialize;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// One anchor to repoint: the file moved, so the binding follows it. The
/// fact itself is untouched - only the path it aims at changes, and every
/// other binding it carries is preserved by rewriting exactly the one that
/// matches `old`.
#[derive(Debug, Deserialize)]
struct Moved {
    id: String,
    project: String,
    old: String,
    new: String,
}

/// One binding to correct in place: a target that was recorded as a `path`
/// but names a DIRECTORY or a route, or an item filed under the wrong
/// project. Both defects have the same symptom and it is the quiet one -
/// the fact fires nowhere at all, and nothing says so.
#[derive(Debug, Deserialize)]
struct Repair {
    id: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    to_kind: Option<String>,
    #[serde(default)]
    to_project: Option<String>,
    /// Replacement text. Used for one defect only: a line number in a
    /// fact's own body, which rots the moment the file grows a line. The
    /// gate refuses those on the way in, so an old fact carrying one can
    /// never be corrected in place until the number is gone.
    #[serde(default)]
    to_text: Option<String>,
    /// Bindings to ADD, never replacing what is there. Used to merge a pair
    /// that says the same thing twice - one bound to a target, one to a
    /// moment - into a single item carrying both. Two items with identical
    /// text fill two of the four slots a block has with one sentence.
    #[serde(default)]
    add_moments: Vec<String>,
}

/// One item to remove, with the reason kept in the log.
#[derive(Debug, Deserialize)]
struct Retracted {
    id: String,
    reason: String,
}

/// One planned check: which item, and the proof to give it. Only `contains`
/// is expressed here - the other kinds need a judgement this plan does not
/// carry (an `absent` says a file must NEVER regain something, which no
/// scanner can infer from a fact's text).
#[derive(Debug, Deserialize)]
struct Planned {
    id: String,
    project: String,
    kind: String,
    path: String,
    literal: String,
}

fn main() -> anyhow::Result<()> {
    let plan_path = std::env::var("ATTACH_PLAN").expect("set ATTACH_PLAN to the plan JSON");
    let db = PathBuf::from(std::env::var("ATTACH_DB").expect("set ATTACH_DB - point it at a COPY first"));
    let apply = std::env::var("ATTACH_APPLY").ok().as_deref() == Some("1");

    let plan: Vec<Planned> = serde_json::from_str(&std::fs::read_to_string(&plan_path)?)?;
    println!("{} planned check(s) from {plan_path}", plan.len());
    println!("store: {}", db.display());
    println!("mode: {}\n", if apply { "APPLY - this writes" } else { "dry run - nothing is written" });

    // Never creates a store: a typo'd path must not produce a healthy-looking
    // empty one that every check then "applies" to.
    let mut store = EventStore::open_existing(&db)?;

    // Anchor repairs first: a fact aimed at a file that is not there fires
    // NOTHING, and nothing says so - the owner's own rule about anchoring to
    // the file a fact is really about. Attaching a check to such a fact
    // would just give a silent item a proof nobody ever runs.
    if let Ok(moves_path) = std::env::var("ATTACH_MOVES") {
        let moves: Vec<Moved> = serde_json::from_str(&std::fs::read_to_string(&moves_path)?)?;
        println!("-- {} anchor(s) to repoint --", moves.len());
        for m in &moves {
            let Ok(existing) = store::show(&store, &m.id) else {
                println!("GONE  {}", m.id);
                continue;
            };
            let mut updated = existing.clone();
            let mut touched = false;
            for b in updated.bindings.iter_mut() {
                if let Binding::Target { kind: TargetKind::Path, value } = b {
                    if value.replace('\\', "/") == m.old {
                        *value = m.new.clone();
                        touched = true;
                    }
                }
            }
            if !touched {
                println!("SKIP  {} - no path binding matching {}", m.id, m.old);
                continue;
            }
            if !apply {
                println!("WOULD {} [{}] {} -> {}", m.id, m.project, m.old, m.new);
                continue;
            }
            match store::revise(&mut store, "attach-checks", "attach-checks", "cli", &existing, &updated) {
                Ok(ev) => println!("MOVED {} [{}] {} -> {} (seq {})", m.id, m.project, m.old, m.new, ev.seq),
                Err(e) => println!("REFUSED {} - {e}", m.id),
            }
        }
        println!();
    }

    if let Ok(rep_path) = std::env::var("ATTACH_REPAIR") {
        let reps: Vec<Repair> = serde_json::from_str(&std::fs::read_to_string(&rep_path)?)?;
        println!("-- {} binding(s) to correct --", reps.len());
        for r in &reps {
            let Ok(existing) = store::show(&store, &r.id) else {
                println!("GONE  {}", r.id);
                continue;
            };
            let mut updated = existing.clone();
            if let Some(proj) = &r.to_project {
                updated.project = Some(proj.clone());
            }
            if let Some(t) = &r.to_text {
                updated.text = t.clone();
            }
            for m in &r.add_moments {
                match model::gate::parse_moment(m) {
                    Ok(b) => {
                        if !updated.bindings.contains(&b) {
                            updated.bindings.push(b);
                        }
                    }
                    Err(e) => println!("SKIP  {} - {e}", r.id),
                }
            }
            if let (Some(v), Some(k)) = (&r.value, &r.to_kind) {
                let want = match k.as_str() {
                    "dir" => TargetKind::Dir,
                    "route" => TargetKind::Route,
                    "command" => TargetKind::Command,
                    "symbol" => TargetKind::Symbol,
                    other => {
                        println!("SKIP  {} - unknown target kind '{other}'", r.id);
                        continue;
                    }
                };
                for b in updated.bindings.iter_mut() {
                    if let Binding::Target { kind, value } = b {
                        if value.replace('\\', "/") == *v {
                            *kind = want;
                        }
                    }
                }
            }
            if !apply {
                println!("WOULD {} project={:?} retarget={:?}", r.id, r.to_project, r.to_kind);
                continue;
            }
            match store::revise(&mut store, "attach-checks", "attach-checks", "cli", &existing, &updated) {
                Ok(ev) => println!("FIXED {} (seq {})", r.id, ev.seq),
                Err(e) => println!("REFUSED {} - {e}", r.id),
            }
        }
        println!();
    }

    if let Ok(rt) = std::env::var("ATTACH_RETRACT") {
        let outs: Vec<Retracted> = serde_json::from_str(&std::fs::read_to_string(&rt)?)?;
        println!("-- {} item(s) to retract --", outs.len());
        for o in &outs {
            if !apply {
                println!("WOULD retract {} - {}", o.id, o.reason);
                continue;
            }
            match store::retract(&mut store, "attach-checks", "attach-checks", "cli", &o.id, &o.reason) {
                Ok(_) => println!("OUT   {}", o.id),
                Err(e) => println!("REFUSED {} - {e}", o.id),
            }
        }
        println!();
    }

    let (mut applied, mut refused, mut missing, mut already) = (0, 0, 0, 0);
    for p in &plan {
        if p.kind != "contains" {
            println!("SKIP  {} - unsupported check kind '{}'", p.id, p.kind);
            continue;
        }
        let existing = match store::show(&store, &p.id) {
            Ok(item) => item,
            Err(e) => {
                missing += 1;
                println!("GONE  {} - {e}", p.id);
                continue;
            }
        };
        if existing.check.is_some() {
            already += 1;
            println!("KEEP  {} - already carries a check, left untouched", p.id);
            continue;
        }
        let mut updated = existing.clone();
        updated.check = Some(Check::Contains { path: p.path.clone(), literal: p.literal.clone() });

        if !apply {
            println!("WOULD {} [{}] {} contains {:?}", p.id, p.project, p.path, p.literal);
            applied += 1;
            continue;
        }
        match store::revise(&mut store, "attach-checks", "attach-checks", "cli", &existing, &updated) {
            Ok(ev) => {
                applied += 1;
                println!("OK    {} [{}] seq {}", p.id, p.project, ev.seq);
            }
            Err(e) => {
                refused += 1;
                println!("REFUSED {} - {e}", p.id);
            }
        }
    }

    println!(
        "\n{}: {applied}, refused by the gate: {refused}, already had one: {already}, id gone: {missing}",
        if apply { "attached" } else { "would attach" }
    );
    Ok(())
}
