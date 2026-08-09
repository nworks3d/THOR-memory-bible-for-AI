//! Does this anchor actually fire? Read-only, against the real store.
//!
//! THE GAP THIS CLOSES, found the hard way on 2026-08-09. A command anchor was
//! easy to test - pipe a PreToolUse payload into `serve hook` and read the
//! decision. A PATH anchor had no such route, and the two workarounds both
//! lie: `Test-Path` proves the file exists and nothing about whether the fact
//! reaches anybody, and piping a payload into `serve hook` needs a throwaway
//! copy of the store, which is a second thing to get wrong (a copy taken while
//! the live WAL was mid-write reports "not served" for anchors that do fire,
//! which is the most expensive possible wrong answer: it says the guard is
//! dead when it is alive).
//!
//! So this asks `serve::serve` directly, the same function the hook calls,
//! against the real store opened read-only. No payload, no copy, and - unlike
//! the hook - no delivery event and no gate event, so probing never inflates
//! the serving counts or the gate counter it is being used to check.
//!
//!   anchorprobe <db> --file <path> [--project <key>]
//!   anchorprobe <db> --command "<command line>" [--project <key>]
//!
//! It prints every item that reaches that place, capped exactly as a real
//! block would be, and marks which ones are shown versus crowded out.

use serve::input::ServeInput;
use thor_core::event_store::EventStore;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let db = args.next().unwrap_or_default();
    if db.is_empty() {
        eprintln!("usage: anchorprobe <db> [--file <path>] [--command <line>] [--project <key>]");
        std::process::exit(2);
    }

    // "How do I know the system actually put an important fact behind the
    // gate?" - asked by the owner on 2026-08-09. Until now the only answer was
    // a count in the health report, which says how many and never which. This
    // prints the list itself, with the one distinction that matters: a rule
    // that has actually refused something is proven, and a rule that has not
    // may be perfect and untested or may be a typo nobody will ever notice.
    let mut argv: Vec<String> = args.collect();
    if argv.first().map(String::as_str) == Some("--armed") {
        return list_armed(std::path::Path::new(&db));
    }
    let mut args = argv.drain(..);

    let (mut file, mut command, mut project, mut moment) = (None, None, None, None);
    let (mut route, mut host) = (None, None);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--file" => file = Some(value),
            "--command" => command = Some(value),
            "--project" => project = Some(value),
            // A route or a host is bound with add_target, not add_file, so
            // neither was reachable here. An anchor you cannot probe is the
            // exact shape this tool exists to catch: it fires nowhere and
            // nobody notices.
            "--route" => route = Some(value),
            "--host" => host = Some(value),
            // A moment is where the worst crowds live - a broad one like
            // claim_done or deploy collects everything anybody ever thought
            // worth saying at that point. Without this flag the only way to
            // see such a pool was to guess a file whose name happens to
            // trigger it, which mixes the moment's crowd with the file's own.
            "--moment" => moment = Some(value),
            other => {
                eprintln!("unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    if file.is_none() && command.is_none() && moment.is_none() && route.is_none() && host.is_none() {
        eprintln!("give at least --file, --command or --moment: an empty place serves nothing and that says nothing");
        std::process::exit(2);
    }

    let store = EventStore::open_existing(std::path::Path::new(&db))?;
    let mut input = ServeInput { project: project.clone(), ..Default::default() };
    if let Some(f) = &file {
        input.add_file(f);
    }
    if let Some(c) = &command {
        input.add_command(c.as_str());
    }
    if let Some(r) = &route {
        input.add_target(model::item::TargetKind::Route, r);
    }
    if let Some(h) = &host {
        input.add_target(model::item::TargetKind::Host, h);
    }
    if let Some(m) = &moment {
        match m.parse::<intent::Action>() {
            Ok(action) => input.add_moment(action),
            Err(_) => {
                eprintln!("'{m}' is not a moment this memory knows - the vocabulary is closed, see intent::Action");
                std::process::exit(2);
            }
        }
    }

    let served = serve::serve(&store, &input);
    let shown: Vec<&str> = served.selection.shown.iter().map(|r| r.id.as_str()).collect();

    println!("place: file={:?} command={:?} route={:?} host={:?} project={:?}", file, command, route, host, project);
    if shown.is_empty() {
        println!("NOTHING is served here - an anchor pointing at this place is silent");
    }
    for id in &shown {
        println!("  SHOWN   {id}");
    }
    // Everything that reached the place and lost its seat, in rank order and
    // with its text. The difference between "the anchor is dead" and "the
    // anchor works and the place is full" is why this is printed at all: 0
    // shown with 0 queued is silence, 0 shown with 7 queued is a queue. The
    // TEXT is here because a queue is where near-duplicates hide, and reading
    // them side by side is the only way to see that two of them say one thing.
    let queued: Vec<_> = served.all.iter().filter(|r| !shown.contains(&r.id.as_str())).collect();
    println!("  queued behind them: {} (selection says {})", queued.len(), served.selection.withheld);
    for r in queued {
        println!("  QUEUED  {}  {}", r.id, r.item.text);
    }
    Ok(())
}

/// Every rule that can refuse a write, in plain reading order, with what it
/// refuses and whether it has ever actually done it.
fn list_armed(db: &std::path::Path) -> anyhow::Result<()> {
    use model::item::Check;
    use thor_core::event_store::EventKind;

    let store = EventStore::open_existing(db)?;
    let fired: std::collections::HashSet<String> = store
        .get_all_events()
        .map(|events| {
            events.iter().filter(|e| e.kind == EventKind::GateRefused).map(|e| e.entity_id.clone()).collect()
        })
        .unwrap_or_default();

    let mut rows: Vec<(bool, String, String, String)> = Vec::new();
    for li in serve::live::live_items(&store) {
        let blocks = match &li.item.check {
            Some(Check::Forbidden { literals }) => literals.join(", "),
            Some(Check::Absent { path, literal }) => format!("{literal:?} appearing in {path}"),
            Some(Check::AbsentAll { path, literals }) => format!("{} in {path}", literals.join(", ")),
            _ => continue,
        };
        let where_ = li
            .item
            .bindings
            .iter()
            .map(|b| match b {
                model::item::Binding::Always => "any file you write".to_string(),
                model::item::Binding::Moment(a) => format!("moment {}", a.as_str()),
                model::item::Binding::Target { value, .. } => value.clone(),
            })
            .collect::<Vec<_>>()
            .join(" + ");
        rows.push((fired.contains(&li.id), li.item.text.clone(), blocks, where_));
    }
    // Proven ones first: those are the ones an owner can trust without asking
    // anybody, and burying them under untested claims would defeat the point.
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    // Rules enforced by the ANSWER guard rather than by the write gate.
    //
    // A rule about what gets SAID can never carry a check: a check reaches
    // file writes and commands, and an answer is neither. Its enforcement
    // lives in the response rulebook, a file the store knows nothing about -
    // so until 2026-08-09 the two were separate copies of one rule with no
    // link, and editing either left the other silently stale. A tag naming
    // the rulebook entry is that link, and this block is where it becomes
    // visible: including, loudly, when the entry it names is gone.
    let rulebook_ids: Vec<String> = std::fs::read_to_string(serve::respond::default_rulebook_path(db))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_array().cloned())
        .map(|entries| {
            entries.iter().filter_map(|e| e.get("id").and_then(|i| i.as_str()).map(String::from)).collect()
        })
        .unwrap_or_default();
    for li in serve::live::live_items(&store) {
        for tag in li.item.tags.iter().filter_map(|t| t.strip_prefix("answer-guard:")) {
            let alive = rulebook_ids.iter().any(|id| id == tag);
            rows.push((
                alive,
                li.item.text.clone(),
                if alive {
                    format!("your own answers, via the response guard entry '{tag}'")
                } else {
                    format!("NOTHING - it names response guard entry '{tag}', which no longer exists")
                },
                "every reply".to_string(),
            ));
        }
    }

    let proven = rows.iter().filter(|r| r.0).count();
    println!("{} rule(s) can refuse a write. {proven} of them have actually done so.\n", rows.len());
    for (has_fired, text, blocks, where_) in rows {
        println!("{} {}", if has_fired { "[PROVEN ]" } else { "[untried]" }, text);
        println!("           blocks: {blocks}");
        println!("           at:     {where_}\n");
    }
    Ok(())
}
