//! The health check: one plain-language line per component, no jargon, no
//! silent gaps. Every reader here degrades to an honest "not configured" or
//! "unreachable" line instead of an error that stops the other lines from
//! printing - a health check that dies on the first broken thing is worse
//! than no health check.

use std::path::Path;
use thor_core::auditor::{verify_chain_integrity, DifferentialAuditor};
use thor_core::event_store::{Event, EventStore};

/// Open `db` and read every event, or explain in one line (no "memory
/// store: " prefix - the caller adds whatever prefix fits it) why that
/// could not be done. The one place that decides what "the store is not
/// available" means: shared by `store_line` (which turns a failure here
/// into the first health line) and `gate_verdict` (which fails OPEN on
/// exactly the same two cases - missing, or present but unreadable).
fn open_and_read(db: &Path) -> Result<Vec<Event>, String> {
    if !db.exists() {
        return Err(format!("not found at {}", db.display()));
    }
    let store = EventStore::open_existing(db)
        .map_err(|e| format!("found at {}, but could not be opened ({e})", db.display()))?;
    store
        .get_all_events()
        .map_err(|e| format!("found at {} but could not be read ({e})", db.display()))
}

/// Whether the event log's hash chain, and the independent differential
/// fold, both hold. The one place that decides "is the chain broken",
/// shared by `store_line`'s prose and `gate_verdict`'s pass/fail check, so
/// the two can never disagree about what counts as intact.
fn chain_intact(events: &[Event]) -> bool {
    verify_chain_integrity(events).is_ok() && DifferentialAuditor::verify_consistency(events).is_ok()
}

/// Component 1+2: does the store exist, how many events, is the chain
/// (hash continuity + the independent differential fold) intact.
pub fn store_line(db: &Path) -> String {
    let events = match open_and_read(db) {
        Ok(e) => e,
        Err(msg) => return format!("memory store: {msg}"),
    };
    if chain_intact(&events) {
        format!("memory store: {} events, chain intact", events.len())
    } else {
        let reason = verify_chain_integrity(&events)
            .err()
            .or_else(|| DifferentialAuditor::verify_consistency(&events).err())
            .unwrap_or_else(|| "unknown".to_string());
        format!("memory store: {} events, CHAIN BROKEN ({reason})", events.len())
    }
}

/// Component 3: is a code index configured, and if so, at which commit
/// (plus how far the working copy has drifted from it).
pub fn code_index_line(index_db: Option<&Path>, repo: Option<&Path>) -> String {
    match (index_db, repo) {
        (Some(index_db), Some(repo)) => {
            if !index_db.exists() {
                return format!("code index: not found at {}", index_db.display());
            }
            match serve::lookup::code_index_status(index_db, repo) {
                Ok(p) => match p.current_commit {
                    // The same COMMIT is not the same FILES. This line said
                    // "matches the working copy" while four files sat
                    // uncommitted - a rosier answer than the truth, and a
                    // line number out of the index is wrong for exactly those
                    // four files. Checked now, and said out loud.
                    Ok(current)
                        if current == p.indexed_commit
                            && p.uncommitted_changed.unwrap_or(0) == 0 =>
                    {
                        format!("code index: at commit {}, matches the working copy", short(&p.indexed_commit))
                    }
                    Ok(current) if current == p.indexed_commit => format!(
                        "code index: at commit {}, but {} file(s) in the working copy are \
                         uncommitted - their line numbers are stale",
                        short(&p.indexed_commit),
                        p.uncommitted_changed
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "an unknown number of".to_string())
                    ),
                    Ok(current) => format!(
                        "code index: at commit {}, working copy is at {} ({} file(s) differ)",
                        short(&p.indexed_commit),
                        short(&current),
                        p.files_differ.map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".to_string())
                    ),
                    Err(e) => format!(
                        "code index: at commit {}, but the repository could not be read ({e})",
                        short(&p.indexed_commit)
                    ),
                },
                Err(e) => format!("code index: could not be checked ({e})"),
            }
        }
        (None, None) => "code index: not configured".to_string(),
        _ => "code index: not configured (pass both the index and the repository to check one)".to_string(),
    }
}

/// Component 4: is a replica configured, and does it run level with us.
pub fn replica_line(db: &Path, replica: Option<(&str, &str)>) -> String {
    let Some((base, token)) = replica else {
        return "replica: not configured".to_string();
    };
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(e) => return format!("replica: configured at {base}, but the local store could not be opened ({e})"),
    };
    let (local_seq, _) = match store.contiguous_tip() {
        Ok(t) => t,
        Err(e) => return format!("replica: configured at {base}, but the local tip could not be read ({e})"),
    };
    match crate::transport::remote_cursor(base, token) {
        Ok(remote) => {
            let lag = local_seq - remote.contiguous_seq;
            if lag == 0 {
                format!("replica: {base}, in sync ({local_seq} events on both sides)")
            } else if lag > 0 {
                format!("replica: {base}, BEHIND by {lag} event(s) ({} of {local_seq})", remote.contiguous_seq)
            } else {
                format!("replica: {base}, AHEAD of us by {} event(s) - not a plain replica of this store", -lag)
            }
        }
        Err(e) => format!("replica: {base}, UNREACHABLE ({e})"),
    }
}

/// Component 5: how many fireable items (Rule/Orientation) still have no
/// falsifier - the worklist number, in plain language.
pub fn falsifier_line(db: &Path) -> String {
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(_) => return "falsifiers: memory store not available - nothing to count".to_string(),
    };
    let status = serve::status::store_status(&store);
    if status.fireable_total == 0 {
        return "falsifiers: no rules or orientations declared yet".to_string();
    }
    if status.missing_falsifier == 0 {
        format!("falsifiers: all {} rule(s)/orientation(s) have one", status.fireable_total)
    } else {
        format!(
            "falsifiers: {} of {} rule(s)/orientation(s) still have no falsifier",
            status.missing_falsifier, status.fireable_total
        )
    }
}

/// How many rules and orientations carry a machine-runnable proof, not just
/// prose - the number that decides how much of this memory can ever stop a
/// wrong write.
///
/// WHY THIS LINE EXISTS. The doctrine is that only a rule whose check runs
/// and holds may block, and that prose can inform but never forbid. That
/// was built, tested, and proven blind. What nobody could see was how much
/// of the store it actually applied to, because nothing counted it: this
/// health check reported falsifier coverage at 100% and read as healthy
/// while proof coverage sat at 2 items out of 2999 (measured 2026-08-06).
/// A capability nothing is wired into looks exactly like one that works.
pub fn proof_line(db: &Path) -> String {
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(_) => return "provable rules: memory store not available - nothing to count".to_string(),
    };
    let items = serve::live::live_items(&store);
    let fireable: Vec<_> = items
        .iter()
        .filter(|i| matches!(i.item.kind, model::item::Kind::Rule | model::item::Kind::Orientation))
        .collect();
    if fireable.is_empty() {
        return "provable rules: no rules or orientations declared yet".to_string();
    }
    let with_check = fireable.iter().filter(|i| i.item.check.is_some()).count();

    // WHY THIS LINE BREAKS THE NUMBER DOWN, since 2026-08-08. "Carries a
    // check" was reported as if it meant "can block", and two independent
    // reviews caught that it does not. What a check can actually do depends
    // entirely on its FORM, and the forms differ more than the single
    // percentage suggested:
    //   Absent / AbsentAll - refuse a write that INTRODUCES the literal into
    //     the file the check names. The strongest form.
    //   Forbidden - the same, with no file at all - but ONLY on an item bound
    //     Always. `serve::absent_guard::find_forbidden_violation` is fed the
    //     Always pool and nothing else, so a Forbidden check on a moment- or
    //     target-bound item passes the write gate, looks like the strongest
    //     form, and can never block. It is counted as blocking nothing.
    //   Contains - refuses only a write to that same file that REMOVES the
    //     literal (`absent_guard::find_missing_required`). Real, but narrow:
    //     it protects a line, it never tests whether the rule's claim is true.
    //   PathExists - blocks nothing ON ITS OWN, but it is exactly what the
    //     LOCATION arm blocks with: Irreversible severity plus a Path/Dir
    //     binding equal to the check's own path makes the whole location out
    //     of bounds (`absent_guard::location_anchor`). Counted separately,
    //     because it refuses a write for WHERE it lands, not what it carries.
    //
    // Reporting one figure let a store full of Contains checks read as if it
    // were enforced. Counting by FORM rather than by capability then let a
    // Forbidden-without-Always read as the strongest thing in the store. Both
    // corrections came from a review reading the arms rather than the checks.
    let (mut forbidding, mut protecting, mut location, mut blocks_nothing) = (0usize, 0usize, 0usize, 0usize);
    for i in &fireable {
        // One definition, shared with `teeth_line` via `can_refuse`, so the
        // two lines can never disagree about what "can refuse" means.
        let refuses = can_refuse(&i.item);
        // A `Forbidden` check reaches wherever its BINDING says, and there are
        // exactly two bindings that carry a reach it can honour: Always (every
        // file write, via `absent_guard::find_forbidden_violation`) and Command
        // (that one command, via `find_command_violation`). Anything else -
        // a moment, a path, a directory - passes the write gate, looks like the
        // strongest form, and can never fire.
        match &i.item.check {
            Some(model::item::Check::Absent { .. })
            | Some(model::item::Check::AbsentAll { .. })
            | Some(model::item::Check::Forbidden { .. })
                if refuses =>
            {
                forbidding += 1
            }
            Some(model::item::Check::Absent { .. })
            | Some(model::item::Check::AbsentAll { .. })
            | Some(model::item::Check::Forbidden { .. }) => blocks_nothing += 1,
            Some(model::item::Check::Contains { .. }) => protecting += 1,
            Some(model::item::Check::PathExists { path }) => {
                let bar = i.item.severity == Some(model::item::Severity::Irreversible)
                    && i.item.bindings.iter().any(|b| {
                        matches!(
                            b,
                            model::item::Binding::Target { kind: model::item::TargetKind::Path | model::item::TargetKind::Dir, value }
                                if value == path
                        )
                    });
                if bar {
                    location += 1;
                } else {
                    blocks_nothing += 1;
                }
            }
            None => {}
        }
    }
    // The other population, and until 2026-08-14 nothing counted it: the rules
    // that were ASKED whether they could refuse and answered no. That answer is
    // the one exit from the gate nothing can verify, so the only honest thing
    // to do with it is show how much of the store rests on it - and how much of
    // that was taken on a bare word, before the reason was required. Bare ones
    // are grandfathered by `gate::revise`, never re-asked by the backlog burn,
    // and would otherwise be invisible forever.
    let (mut answered_no, mut without_reason) = (0usize, 0usize);
    for i in &fireable {
        match i.item.tags.iter().find_map(|t| model::store::teeth_answer(t)) {
            Some(model::store::TeethAnswer::Bare) => {
                answered_no += 1;
                without_reason += 1;
            }
            Some(model::store::TeethAnswer::Reasoned(_)) => answered_no += 1,
            None => {}
        }
    }
    format!(
        "provable rules: {} of {} rule(s)/orientation(s) carry a runnable check ({:.1}%) - of those, \
         {forbidding} can refuse a write that introduces something forbidden, {location} can refuse a \
         write for landing in a place that is out of bounds, {protecting} can only refuse one that \
         removes a required line from their own file, and {blocks_nothing} block nothing at all; \
         {answered_no} other rule(s) were asked and answered that nothing can catch them, {without_reason} \
         of those without saying why (a bare answer nothing will ask about again)",
        with_check,
        fireable.len(),
        100.0 * with_check as f64 / fireable.len() as f64
    )
}

/// What has quietly rotted since the last time anyone looked: anchors that
/// resolve to nothing, and proofs that now come out false.
///
/// WHY THIS LINE EXISTS. A memory decays between writes, not during them.
/// The write gate can refuse a bad declaration and the neighbourhood toll
/// can refuse a write next to rot, but neither ever fires while a file is
/// simply moved or deleted by ordinary work. On 2026-08-06 that had left
/// 128 anchors in four projects pointing at nothing: those facts fired
/// NOWHERE, and no surface said so. Repairing them took a full sweep. The
/// point of counting it here is that the next hundred announce themselves
/// instead of waiting for someone to go looking.
///
/// Needs `--checkouts`, for the same reason `orphan_projects_line` does: an
/// anchor is relative to a project's own working copy, and this refuses to
/// guess where that is.
/// One rotted item, NAMED.
///
/// THE DEFECT THIS CLOSES, reported by an agent on 2026-08-14 after it went
/// looking. This check knew exactly which item was broken - it holds the id
/// in its own loop - and returned a number. The line then said "run doctor to
/// see them", and doctor said "8". Nothing anywhere could turn that 8 into a
/// list: not `audit` (counts), not `anchorprobe` (wants the file you are
/// trying to find), not the MCP tools (they work per id, and the id is what
/// you do not have). The only route left was reading the SQLite store by
/// hand, around the whole tool. A number that names a problem and hides the
/// subject is not a report, it is a rumour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rot {
    pub id: String,
    pub project: String,
    /// The anchor that points at nothing, or the proof that came out false.
    pub what: String,
}

/// The result of trying to check decay: either it could not be checked at
/// all (and why), or it was, and every rotted item is named. The one place
/// that computes decay - shared by `decay_line` (prose, every project under
/// `checkouts`) and `gate_verdict` (pass/fail, optionally narrowed to one
/// project).
enum DecayCheck {
    StoreUnreadable,
    NoCheckouts,
    Counted { dead: Vec<Rot>, failing: Vec<Rot>, judged: usize },
}

/// How many rotted items the decay line names before it stops. Enough that a
/// normal repair session needs no second surface, small enough that a sweep
/// of 128 does not bury every other line in the report - and whatever it
/// holds back, it says so out loud rather than reading as the whole list.
const DECAY_NAMES_AT_MOST: usize = 20;

/// The rotted items as text, one per line, indented under their own line.
fn name_rot(label: &str, rot: &[Rot]) -> Vec<String> {
    let mut out = Vec::new();
    for r in rot.iter().take(DECAY_NAMES_AT_MOST) {
        out.push(format!("  {label}: {} [{}] {}", r.id, r.project, r.what));
    }
    if rot.len() > DECAY_NAMES_AT_MOST {
        out.push(format!(
            "  {label}: and {} more, not named here",
            rot.len() - DECAY_NAMES_AT_MOST
        ));
    }
    out
}

/// For every live, fireable item whose project resolves to a checkout under
/// `checkouts`, does its anchor exist and does its proof still hold.
///
/// `project_filter`, when given, skips every item whose project does not
/// match it - this is what lets the gate narrow decay to one repository's
/// own items, so that repository cannot be failed by another checkout's rot
/// under the same `--checkouts` directory. `decay_line` always passes
/// `None`, so its own output is unchanged by this function existing:
/// every project's rot, exactly as before this was split out.
fn decay_check(db: &Path, checkouts: Option<&Path>, project_filter: Option<&str>) -> DecayCheck {
    let Ok(store) = EventStore::open_existing(db) else {
        return DecayCheck::StoreUnreadable;
    };
    let Some(root) = checkouts else {
        return DecayCheck::NoCheckouts;
    };

    let mut roots: std::collections::BTreeMap<String, std::path::PathBuf> = Default::default();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    roots.insert(key, path);
                }
            }
        }
    }

    let items = serve::live::live_items(&store);
    let (mut dead, mut failing, mut judged) = (Vec::new(), Vec::new(), 0usize);
    for li in items.iter().filter(|li| li.item.kind.can_fire()) {
        let Some(project) = li.item.project.as_deref() else { continue };
        if let Some(only) = project_filter {
            if only != project {
                continue;
            }
        }
        let Some(base) = roots.get(project) else { continue };
        // An anchor on something DELIBERATELY absent is not decay. A rule
        // guarding a gitignored secrets file is anchored there exactly so it
        // fires the moment that file appears; counting it as rot is what got
        // six of them swept into the archive on 2026-08-07.
        if li.item.tags.iter().any(|t| t == model::store::DELIBERATE_ANCHOR_TAG) {
            continue;
        }
        judged += 1;
        for binding in &li.item.bindings {
            if let model::item::Binding::Target { kind: model::item::TargetKind::Path, value } = binding {
                // An absolute path is somewhere else on this machine, not in
                // the checkout, so it is not this line's business.
                if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                    continue;
                }
                if !base.join(value.replace('\\', "/")).exists() {
                    dead.push(Rot {
                        id: li.id.clone(),
                        project: project.to_string(),
                        what: value.clone(),
                    });
                }
            }
        }
        if let Some(check) = li.item.check.as_ref() {
            if model::check::run(check, base) == model::check::Outcome::Fails {
                failing.push(Rot {
                    id: li.id.clone(),
                    project: project.to_string(),
                    // The check's own Debug form. Deliberately not a hand-written
                    // sentence per kind: a description that has to be kept in
                    // step with the enum is a description that goes stale, and
                    // this one only has to say enough to find the thing.
                    what: format!("{check:?}"),
                });
            }
        }
    }
    DecayCheck::Counted { dead, failing, judged }
}

pub fn decay_line(db: &Path, checkouts: Option<&Path>) -> String {
    match decay_check(db, checkouts, None) {
        DecayCheck::StoreUnreadable => "decay: store unreadable, not checked".to_string(),
        DecayCheck::NoCheckouts => {
            "decay: pass --checkouts <dir> to check anchors and proofs against real working copies".to_string()
        }
        DecayCheck::Counted { judged: 0, .. } => {
            "decay: no project under --checkouts answers to any item's key, nothing checked".to_string()
        }
        // Both live arms say WHOSE rot this is. The session-start notice
        // counts the repository you are standing in and this counts every
        // project under --checkouts, so the two numbers legitimately differ -
        // and until 2026-08-14 neither line said which it was, which read as
        // the tool contradicting itself.
        DecayCheck::Counted { dead, failing, judged } if dead.is_empty() && failing.is_empty() => {
            format!("decay: none - across every project under --checkouts, every anchor of {judged} project-scoped item(s) resolves and every proof holds")
        }
        DecayCheck::Counted { dead, failing, judged } => {
            let mut out = vec![format!(
                "decay: {} anchor(s) point at nothing (those facts fire NOWHERE) and {} proof(s) now come out FALSE, across {judged} project-scoped item(s) in every project under --checkouts - each one named below; repair with revise (the file moved) or retract (the fact went with it)",
                dead.len(),
                failing.len()
            )];
            out.extend(name_rot("dead anchor", &dead));
            out.extend(name_rot("false proof", &failing));
            out.join("\n")
        }
    }
}

/// How many items have been put in front of a reader over and over without
/// ever being judged either way.
///
/// WHY THIS IS A COUNT AND NOT A REFUSAL. Judging a served fact costs the
/// session that does it and pays only the next one, so it does not happen -
/// on this store, in its whole life, exactly four judgements had ever been
/// recorded, all on one day. The obvious fix is a toll, the way the write
/// gate tolls a write next to rot. It does not fit here: a toll needs
/// something local to hold hostage, and the worst offenders are the pinned
/// rules that fire at every session start with no target at all. The other
/// candidate, refusing at the end of a turn, is the capture guard's shape -
/// measured on a blind hold-out at 55% catch and 11.4% false blocks, and
/// recorded as not deployable.
///
/// So this does the one thing that demonstrably moved the other number:
/// count it where someone will see it. Proof coverage sat at 3 of 3004
/// until a line here said so out loud, and it is 246 a day later.
pub fn unjudged_line(db: &Path) -> String {
    const HEAVY: usize = 40;
    let Ok(store) = EventStore::open_existing(db) else {
        return "unjudged: store unreadable, not checked".to_string();
    };
    let Ok(events) = store.event_kinds() else {
        return "unjudged: the log could not be read, not checked".to_string();
    };
    use thor_core::event_store::EventKind;
    let mut served: std::collections::HashMap<String, usize> = Default::default();
    let mut judged: std::collections::HashSet<String> = Default::default();
    for (kind, id) in events {
        match kind {
            EventKind::ItemServed => *served.entry(id).or_default() += 1,
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise => {
                judged.insert(id);
            }
            _ => {}
        }
    }
    // Pinned items are excluded here for the same reason the Stop-time ask
    // excludes them: "did it belong where it fired" is already answered by
    // the owner having pinned it. Counting them would make this line
    // disagree with the mechanism it exists to report on.
    let pinned: std::collections::HashSet<String> = serve::live::live_items(&store)
        .into_iter()
        .filter(|li| li.item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always)))
        .map(|li| li.id)
        .collect();
    let heavy = served
        .iter()
        .filter(|(id, n)| **n >= HEAVY && !judged.contains(*id) && !pinned.contains(*id))
        .count();
    let total_unjudged = served.keys().filter(|id| !judged.contains(*id) && !pinned.contains(*id)).count();
    if total_unjudged == 0 {
        return "unjudged: every item that has ever fired has been judged at least once".to_string();
    }
    format!(
        "unjudged: {heavy} trigger-bound item(s) fired {HEAVY}+ times and were never judged either way ({total_unjudged} unjudged in total, pinned items excluded - the owner answered that question by pinning them) - `mark` is the only thing that ever retires noise, and silence decides nothing"
    )
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(8)]
}

/// Component 6: which mode surface 4's meaning search (feature `semantic`)
/// runs in right now - the exact same line `serve status` prints (see
/// `serve::semantic_paths::mode_line`), so the two can never disagree about
/// what is active. Never opens the store: whether the mode is active or
/// falling back depends only on the binary's own compile flags and the
/// model directory, not on any particular store's content.
pub fn semantic_line(model_dir: Option<&Path>) -> String {
    serve::semantic_paths::mode_line(model_dir)
}

/// Component 6: project keys held by live items that NO checkout under
/// `checkouts` resolves to - items that can never reach a pushed surface
/// because nothing answers to their name.
///
/// WHY THIS LINE EXISTS. Three separate defects on 2026-08-03 had exactly
/// one shape: an item named a project that resolution could not produce.
/// The global tier stored as a project called "global"; 1.0's marker file
/// name that 2.0 never read; a linked worktree resolving to its own slug.
/// All three were invisible from every surface - a rule scoped out looks
/// exactly like a rule that never matched. This turns that class from
/// silent into a line anyone can read.
pub fn orphan_projects_line(db: &Path, checkouts: Option<&Path>) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "project keys: store unreadable, not checked".to_string();
    };
    let items = serve::live::live_items(&store);
    let held: Vec<String> = items
        .iter()
        .filter(|li| li.item.kind.can_fire())
        .filter_map(|li| li.item.project.clone())
        .collect();
    if held.is_empty() {
        return "project keys: every fireable item is global, nothing to check".to_string();
    }

    let Some(root) = checkouts else {
        let distinct: std::collections::BTreeSet<&str> = held.iter().map(String::as_str).collect();
        return format!(
            "project keys: {} distinct key(s) in use; pass --checkouts <dir> to check that a real checkout answers to each one",
            distinct.len()
        );
    };

    let mut resolvable: std::collections::BTreeSet<String> = Default::default();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    resolvable.insert(key);
                }
            }
        }
    }
    let orphans = serve::project::orphaned_project_keys(held.iter().map(String::as_str), &resolvable);
    if orphans.is_empty() {
        return format!(
            "project keys: every key in use resolves to a checkout under {}",
            root.display()
        );
    }
    let detail: Vec<String> = orphans.iter().map(|(k, n)| format!("{k} ({n} item(s))")).collect();
    format!(
        "project keys: {} key(s) that no checkout under {} answers to, so their items only ever answer a lookup and never fire at a moment: {}",
        orphans.len(),
        root.display(),
        detail.join(", ")
    )
}

/// The whole report: one line per component, in a fixed order, so a person
/// (or a script) can read it top to bottom without guessing what is missing.
/// How many items are eligible at one of their own triggers and never shown
/// there, because the block has fewer places than the pool has claimants.
///
/// WHY THIS LINE EXISTS, and why it did not until 2026-08-08. CONTRACT R2 used
/// to declare crowding reports unnecessary, on the reasoning that nothing is
/// truncated at delivery because nothing delivered is long. That is true of
/// truncation INSIDE an item and says nothing about how many items are dropped
/// FROM the list. The contract therefore argued this project out of the one
/// measurement that would have shown the damage, and nobody looked for months.
/// First measurement, the day R2 was corrected: 239 items eligible somewhere
/// and shown nowhere, out of 698; the worst single pool held 38 for 4 places.
///
/// It asks the REAL selector (`serve::serve`), never a second copy of the
/// selection rule, for the same reason `why` does: a report that disagrees
/// with the thing it reports on is worse than no report.
///
/// Deliberately NOT part of `gate_verdict`. This is whole-store debt, the same
/// class as proof coverage and the unjudged count, and a commit gate that
/// fails on those teaches people to bypass it.
/// Whether the rules the owner marked most costly are the ones that can
/// actually refuse - because measured on the real store, they are the exact
/// opposite.
///
/// WHY THIS LINE EXISTS. Severity is the FIRST ranking key, so a heavy item
/// takes a place from every lighter one at the same trigger. It is meant to
/// say what it costs when this goes wrong. Measured 2026-08-08: of 228 items
/// marked irreversible or costly, ZERO could refuse anything, while all 17
/// that could refuse were marked house style or nothing at all. The store's
/// own sense of danger and its actual teeth point in opposite directions, and
/// no line said so.
///
/// This is a worklist, not a fault: these are exactly the rules where a check
/// is worth writing, because being wrong about them is what the owner already
/// said is expensive.
pub fn teeth_line(db: &Path) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "teeth: store unreadable, not checked".to_string();
    };
    let live = serve::live::live_items(&store);
    let (mut heavy, mut heavy_with_teeth) = (0usize, 0usize);
    for li in live.iter().filter(|li| li.item.kind.can_fire()) {
        if !matches!(
            li.item.severity,
            Some(model::item::Severity::Irreversible) | Some(model::item::Severity::Costly)
        ) {
            continue;
        }
        heavy += 1;
        if can_refuse(&li.item) {
            heavy_with_teeth += 1;
        }
    }
    if heavy == 0 {
        return "teeth: no rule is marked irreversible or costly yet".to_string();
    }
    // Armed is a claim about shape. Having refused something is a fact about
    // the world, and it is the only one of the two that cannot be wrong.
    //
    // WHY BOTH NUMBERS, and the owner put it exactly right on 2026-08-09: the
    // agent writes these rules, not him, so "a human will spot a bad literal"
    // is not a safeguard - the human never sees it. Nothing here can check
    // that a forbidden fragment is spelled the way the real command spells it
    // (measured the same day: requiring the fragment to appear in the rule's
    // own text would have refused 17 of 29 literals, including every one of
    // the typography rule's, which cannot quote what it forbids). What CAN be
    // known is whether a rule has ever actually stopped anything. A rule that
    // has is proven in the field; one that has not may be perfect and merely
    // untested, or may be a typo nobody will ever notice, and this line
    // refuses to blur those two.
    let proven = store
        .get_all_events()
        .map(|events| {
            events
                .iter()
                .filter(|e| e.kind == thor_core::event_store::EventKind::GateRefused)
                .map(|e| e.entity_id.clone())
                .collect::<std::collections::HashSet<_>>()
                .len()
        })
        .unwrap_or(0);
    format!(
        "teeth: {heavy_with_teeth} of {heavy} rule(s) marked irreversible or costly can actually refuse something - the other {} can only inform, and they are the ones where being wrong was already called expensive; {proven} rule(s) of any weight have ever actually refused a write, which is the only count here that is proven rather than claimed",
        heavy - heavy_with_teeth
    )
}

/// What the only maintenance loop in this system never looks at.
///
/// WHY THIS LINE EXISTS. A pinned item is excluded from the judgement debt
/// (pinning is itself a verdict, so "did it belong where it fired" has no
/// honest answer) and from decay (same reason). It is also served IN FULL at
/// every session start, uncapped. Both choices are defensible on their own and
/// together they produce a blind spot nobody had counted: measured
/// 2026-08-08, 31 pinned items were 48.9% of every serving this store had ever
/// made, and 23 of them had never been examined by anything.
///
/// No mechanism is proposed here. The number is the point: half of what this
/// memory says comes from the part of it nothing checks.
pub fn pinned_line(db: &Path) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "pinned: store unreadable, not checked".to_string();
    };
    let Ok(events) = store.event_kinds() else {
        return "pinned: log unreadable, not checked".to_string();
    };
    let mut served: std::collections::HashMap<String, usize> = Default::default();
    let mut judged: std::collections::HashSet<String> = Default::default();
    for (kind, id) in events {
        match kind {
            thor_core::event_store::EventKind::ItemServed => *served.entry(id).or_default() += 1,
            thor_core::event_store::EventKind::ItemMarkedUseful
            | thor_core::event_store::EventKind::ItemMarkedNoise => {
                judged.insert(id);
            }
            _ => {}
        }
    }
    let live = serve::live::live_items(&store);
    let pinned: Vec<_> = live
        .iter()
        .filter(|li| li.item.kind.can_fire())
        .filter(|li| li.item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always)))
        .collect();
    if pinned.is_empty() {
        return "pinned: nothing is pinned".to_string();
    }
    let never_judged = pinned.iter().filter(|li| !judged.contains(&li.id)).count();
    // A share of nothing is not a share. On a fresh store this line said "0% of
    // every serving" beside "4 never examined", which reads as an alarm about a
    // memory that has simply not been used yet - the first thing a new user
    // sees, and wrong.
    if served.values().sum::<usize>() == 0 {
        return format!(
            "pinned: {} item(s), served at every session start - nothing has been served yet, so there is nothing to weigh them against",
            pinned.len()
        );
    }
    let their_servings: usize = pinned.iter().map(|li| served.get(&li.id).copied().unwrap_or(0)).sum();
    let all: usize = served.values().sum();
    format!(
        "pinned: {} item(s), {never_judged} never examined by anything, together {:.0}% of every serving this store has made - the judgement debt and decay both skip them on purpose, so this share is the part of your memory nothing ever re-reads",
        pinned.len(),
        100.0 * their_servings as f64 / all.max(1) as f64
    )
}

/// The one definition of "this check can refuse something", shared by
/// `proof_line` and `teeth_line` so the two can never disagree about it.
fn can_refuse(item: &model::item::Item) -> bool {
    let always = item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always));
    let command = item
        .bindings
        .iter()
        .any(|b| matches!(b, model::item::Binding::Target { kind: model::item::TargetKind::Command, .. }));
    match &item.check {
        Some(model::item::Check::Absent { .. }) | Some(model::item::Check::AbsentAll { .. }) => true,
        Some(model::item::Check::Forbidden { .. }) => always || command,
        _ => false,
    }
}

/// How often the gate actually did its job, read straight out of the log.
///
/// THE POINT OF THIS LINE. Everything else in this report counts what the
/// store CONTAINS. This is the only line that counts what it DID. Until
/// 2026-08-08 no such number existed anywhere, which is how a gate that
/// stood down after one refusal per file per session survived unnoticed from
/// the first day of 2.0: an allow left no trace, so the failure mode was
/// literally unobservable. Two counts, deliberately side by side - a refusal
/// is visible to whoever it refused, a stand-aside is visible to nobody.
///
/// Reads history, so the numbers are lifetime totals for this store, not for
/// this session. That is the useful frame: "this memory has refused 40
/// writes" is the sentence that says whether 2.0 works.
pub fn gate_line(db: &Path) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "gate: store unreadable, not checked".to_string();
    };
    let Ok(events) = store.get_all_events() else {
        return "gate: log unreadable, not checked".to_string();
    };
    let mut refused = 0usize;
    let mut stood_aside = 0usize;
    let mut rules: std::collections::BTreeSet<String> = Default::default();
    for event in &events {
        match event.kind {
            thor_core::event_store::EventKind::GateRefused => {
                refused += 1;
                rules.insert(event.entity_id.clone());
            }
            thor_core::event_store::EventKind::GateStoodAside => stood_aside += 1,
            _ => {}
        }
    }
    if refused == 0 && stood_aside == 0 {
        return "gate: has never refused a write, and has never stood aside from one".to_string();
    }
    format!(
        "gate: {refused} refusals by {} distinct rules, {stood_aside} stand-asides, lifetime (a stand-aside is the stale-rule nudge holding off for the rest of a session; no prohibition stands aside SINCE 2026-08-08, so older ones in this total are real prohibitions that did)",
        rules.len()
    )
}

pub fn crowding_line(db: &Path, checkouts: Option<&Path>) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "crowding: store unreadable, not checked".to_string();
    };
    let Some(root) = checkouts else {
        return "crowding: pass --checkouts <dir> to see how many facts never win a place".to_string();
    };

    let mut roots: std::collections::BTreeMap<String, std::path::PathBuf> = Default::default();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    roots.insert(key, path);
                }
            }
        }
    }

    // Every distinct anchor that resolves, and the pool it stands for.
    let mut anchors: std::collections::BTreeSet<(String, String)> = Default::default();
    for li in serve::live::live_items(&store).iter().filter(|li| li.item.kind.can_fire()) {
        let Some(project) = li.item.project.clone() else { continue };
        let Some(base) = roots.get(&project) else { continue };
        for binding in &li.item.bindings {
            let model::item::Binding::Target { kind: model::item::TargetKind::Path, value } = binding else {
                continue;
            };
            if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                continue;
            }
            if base.join(value.replace('\\', "/")).exists() {
                anchors.insert((project.clone(), value.clone()));
            }
        }
    }
    if anchors.is_empty() {
        return "crowding: no resolvable anchor to probe, nothing to say".to_string();
    }

    let (mut eligible, mut reachable) = (
        std::collections::HashSet::new(),
        std::collections::HashSet::new(),
    );
    let mut worst: Option<(usize, String, String)> = None;
    for (project, path) in &anchors {
        let mut input = serve::input::ServeInput::default();
        input.add_file(path);
        // The project is what `rank::select` filters on. Leaving it out serves
        // only global items and makes every pool look empty.
        input.project = Some(project.clone());
        let served = serve::serve(&store, &input);
        let shown: std::collections::HashSet<&str> =
            served.selection.shown.iter().map(|r| r.id.as_str()).collect();
        for r in &served.all {
            eligible.insert(r.id.clone());
            if shown.contains(r.id.as_str()) {
                reachable.insert(r.id.clone());
            }
        }
        if worst.as_ref().map(|(n, _, _)| served.all.len() > *n).unwrap_or(true) {
            worst = Some((served.all.len(), project.clone(), path.clone()));
        }
    }

    let invisible = eligible.len() - reachable.len();
    if invisible == 0 {
        return format!("crowding: none - every one of {} eligible item(s) wins a place somewhere", eligible.len());
    }
    match worst {
        Some((n, project, path)) if n > serve::render::MAX_ITEMS => format!(
            "crowding: {invisible} of {} item(s) are eligible at their own trigger and NEVER shown there, \
             because a block holds {} - they are current and correctly bound, just permanently outranked; \
             worst pool: {n} claimants at {path} in {project}",
            eligible.len(),
            serve::render::MAX_ITEMS
        ),
        _ => format!(
            "crowding: {invisible} of {} item(s) are eligible at their own trigger and never shown there",
            eligible.len()
        ),
    }
}

pub fn report(
    db: &Path,
    index_db: Option<&Path>,
    repo: Option<&Path>,
    replica: Option<(&str, &str)>,
    model_dir: Option<&Path>,
    checkouts: Option<&Path>,
) -> Vec<String> {
    vec![
        store_line(db),
        code_index_line(index_db, repo),
        replica_line(db, replica),
        falsifier_line(db),
        proof_line(db),
        gate_line(db),
        teeth_line(db),
        pinned_line(db),
        decay_line(db, checkouts),
        crowding_line(db, checkouts),
        unjudged_line(db),
        semantic_line(model_dir),
        orphan_projects_line(db, checkouts),
    ]
}

/// The three-way answer `--gate` needs. See `gate_verdict` for exactly what
/// separates `Clean` from `Failing`, and why a store that could not be
/// judged is neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    /// The store could not be judged at all: missing, or present but
    /// unreadable. The caller must treat this as a PASS, never a failure -
    /// see `gate_verdict`'s doc comment.
    NotAvailable,
    /// The store was judged and nothing gate-worthy was found.
    Clean,
    /// The store was judged and at least one real finding was found.
    Failing,
}

/// Decide `--gate`'s verdict, reusing exactly the computations the plain-
/// language lines above already do - never a second evaluation of what they
/// evaluate. This function is the one place that decides which finding is
/// gate-worthy; `doctor` only turns its answer into an exit code.
///
/// FAILS the gate (`Failing`):
///
/// - The event chain is broken (`chain_intact`, the same check `store_line`
///   reports as "CHAIN BROKEN"): the store's own audit trail no longer
///   holds together. Not a matter of degree or opinion, and worse than any
///   single item being wrong, so this applies regardless of `project`.
/// - A dead anchor, or a proof that now comes out FALSE (`decay_check`): a
///   fact that fires nowhere, or a rule whose own machine-checkable claim is
///   presently untrue. Both are current and concrete, not historical debt -
///   scoped to `project`'s own items when one is given, so one repository's
///   gate can never be failed by another checkout's rot under the same
///   `--checkouts` directory.
///
/// Does NOT fail the gate (`Clean`, even where the line above may read as
/// less than perfect) - and why each was left out:
///
/// - Code index / replica lines report a COMPANION system's staleness or
///   reachability, never the memory's own correctness. A replica that is
///   merely unreachable (no network, a cloud sandbox) would fail every
///   single commit for a reason that has nothing to do with what changed -
///   exactly the kind of noise that trains people to skip a gate.
/// - Falsifier coverage and provable-rule coverage are whole-store
///   PERCENTAGES of historical debt, not a defect introduced by this run.
///   Forcing either up in bulk was already tried and explicitly rejected
///   (raise per fact where being wrong is costly, never bulk-link the whole
///   store) because it reintroduces the noise this whole feature exists to
///   keep out - so the gate must not recreate that same pressure by another
///   route. (Also: `declare` already requires a falsifier for anything that
///   can fire, so a missing one is legacy debt from before that rule
///   existed, never something a normal write can create today.)
/// - Unjudged items are explicitly a nudge, not a defect: silence decides
///   nothing about whether an item belongs where it fired.
/// - Semantic search mode is a build/deploy fact (is the feature compiled
///   in, is a model installed on THIS machine) - never a statement about
///   any store's content, so it can never be a store's finding.
/// - Orphan project keys are real, but only as trustworthy as the
///   `--checkouts` directory is complete. Miss one sibling checkout, rename
///   a project, or archive one on purpose, and this reports an orphan that
///   has nothing to do with the commit being gated - a false positive with
///   no way for `project` to rule it out the way it can for decay. Left out
///   until it can be scoped as safely as decay already is, rather than risk
///   exactly the false positive this function exists to avoid.
///
/// FAILS OPEN (`NotAvailable`) when the store is missing or cannot be read.
/// A cloud session has no thor.db at all, and must never be blocked by its
/// absence - nor may a store that is present but corrupt/mid-sync turn into
/// a build failure, since neither case says anything about whether THIS
/// change is wrong. A gate that cries wolf gets bypassed, and this codebase
/// has a measured instance of exactly that, which is the reason every
/// "does not fail" case above errs toward `Clean` as well.
pub fn gate_verdict(db: &Path, checkouts: Option<&Path>, project: Option<&str>) -> GateVerdict {
    let events = match open_and_read(db) {
        Ok(e) => e,
        Err(_) => return GateVerdict::NotAvailable,
    };
    let chain_broken = !chain_intact(&events);
    let decay_finding = matches!(
        decay_check(db, checkouts, project),
        DecayCheck::Counted { dead, failing, .. } if !dead.is_empty() || !failing.is_empty()
    );
    if chain_broken || decay_finding {
        GateVerdict::Failing
    } else {
        GateVerdict::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Check, Item, Kind, TargetKind};
    use model::store;

    fn rule(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "a fixture rule".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this fixture rule turns out to be wrong".to_string()),
            check: None,
        }
    }

    #[test]
    fn store_line_reports_missing_store_without_creating_one() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        let line = store_line(&missing);
        assert!(line.contains("not found"), "{line}");
        assert!(!missing.exists(), "the health check must never create a store");
    }

    #[test]
    fn store_line_counts_events_and_reports_an_intact_chain() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("r1")).unwrap();
        }
        let line = store_line(&db);
        assert!(line.contains("intact"), "{line}");
        assert!(line.contains('1'), "{line}");
    }

    #[test]
    fn code_index_line_says_plainly_when_not_configured() {
        assert_eq!(code_index_line(None, None), "code index: not configured");
    }

    #[test]
    fn replica_line_says_plainly_when_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert_eq!(replica_line(&db, None), "replica: not configured");
    }

    /// THE DEFECT THIS PREVENTS: counting a check by its FORM instead of by
    /// what it can do. A `Forbidden` check reads as the strongest thing in the
    /// store, and it is - but only on an item bound Always, because
    /// `absent_guard::find_forbidden_violation` is fed the Always pool and
    /// nothing else. Bound to a moment instead, it passes the write gate,
    /// counted as blocking, and can never block. Found by a review reading the
    /// guard's arms rather than the check list.
    #[test]
    fn a_forbidden_check_without_an_always_binding_is_counted_as_blocking_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut reaches = rule("forbidden-and-pinned");
            reaches.check = Some(model::item::Check::Forbidden { literals: vec!["\u{2014}".to_string()] });
            store::declare(&mut s, "s", "l", "a", &reaches).unwrap();

            let mut cannot = rule("forbidden-but-not-pinned");
            cannot.text = "a webhook retry backs off before it gives up entirely".to_string();
            cannot.project = Some("fixture-project".to_string());
            cannot.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "server/lib/mail.js".to_string(),
            }];
            cannot.check = Some(model::item::Check::Forbidden { literals: vec!["\u{2014}".to_string()] });
            store::declare(&mut s, "s", "l", "a", &cannot).unwrap();
        }
        let line = proof_line(&db);
        assert!(line.contains("1 can refuse a write that introduces"), "{line}");
        assert!(line.contains("1 block nothing at all"), "{line}");
    }

    /// The mirror: a `PathExists` check is NOT merely a currency proof when it
    /// meets the location arm's bar (Irreversible, plus a binding equal to the
    /// check's own path). Then it refuses a write for WHERE it lands.
    #[test]
    fn a_path_exists_check_that_meets_the_location_bar_is_counted_as_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut blocks = rule("frozen-place");
            blocks.severity = Some(model::item::Severity::Irreversible);
            blocks.project = Some("fixture-project".to_string());
            blocks.bindings =
                vec![model::item::Binding::Target { kind: model::item::TargetKind::Dir, value: "frozen".to_string() }];
            blocks.check = Some(model::item::Check::PathExists { path: "frozen".to_string() });
            store::declare(&mut s, "s", "l", "a", &blocks).unwrap();

            let mut currency = rule("just-currency");
            currency.text = "the estimator rounds a quote up to whole cents".to_string();
            currency.project = Some("fixture-project".to_string());
            currency.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "server/lib/quote.js".to_string(),
            }];
            currency.check = Some(model::item::Check::PathExists { path: "server/lib/quote.js".to_string() });
            store::declare(&mut s, "s", "l", "a", &currency).unwrap();
        }
        let line = proof_line(&db);
        assert!(line.contains("1 can refuse a write for landing in a place"), "{line}");
        assert!(line.contains("1 block nothing at all"), "severity below Irreversible does not block: {line}");
    }

    /// THE DEFECT THIS PREVENTS: a health check that reads as fully healthy
    /// while almost nothing in the store can actually block anything. Every
    /// other line here was green on 2026-08-06 with proof coverage at 2 of
    /// 2999, and no line said so.
    #[test]
    fn the_proof_line_reports_coverage_and_never_stays_silent_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("prose-only")).unwrap();
        }
        let line = proof_line(&db);
        assert!(line.contains("0 of 1"), "a store with no provable rule must say so: {line}");
        assert!(line.contains("block"), "and say what the number decides: {line}");
    }

    /// THE INVERSION THIS REPORTS. Severity is the first ranking key, so a
    /// heavy item takes a place from every lighter one at the same trigger -
    /// and measured on the owner's real store, not one heavy rule could refuse
    /// anything, while every rule that could was marked house style or nothing.
    /// The store's sense of danger and its actual teeth pointed in opposite
    /// directions and no line said so.
    #[test]
    fn the_teeth_line_counts_heavy_rules_that_can_actually_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut heavy_prose = rule("heavy-but-toothless");
            heavy_prose.severity = Some(model::item::Severity::Irreversible);
            // Since gate ground 11 this is the only way a heavy rule carries no
            // check: asked, and the answer was that there is nothing to catch.
            heavy_prose.tags = vec![format!("{}a test fixture with nothing literal to catch", store::NO_LITERAL_REASON_PREFIX)];
            store::declare(&mut s, "s", "l", "a", &heavy_prose).unwrap();

            let mut heavy_armed = rule("heavy-and-armed");
            heavy_armed.text = "a webhook retry backs off before it gives up entirely".to_string();
            heavy_armed.severity = Some(model::item::Severity::Costly);
            heavy_armed.check = Some(model::item::Check::Forbidden { literals: vec!["\u{2014}".to_string()] });
            store::declare(&mut s, "s", "l", "a", &heavy_armed).unwrap();

            // Light and armed: counted by proof_line, never by this line.
            let mut light_armed = rule("light-and-armed");
            light_armed.text = "the estimator rounds a quote up to whole cents".to_string();
            light_armed.check = Some(model::item::Check::Forbidden { literals: vec!["\u{2026}".to_string()] });
            store::declare(&mut s, "s", "l", "a", &light_armed).unwrap();
        }
        let line = teeth_line(&db);
        assert!(line.contains("1 of 2"), "{line}");
        assert!(line.contains("the other 1 can only inform"), "{line}");
    }

    /// THE BLIND SPOT THIS REPORTS. A pinned item is skipped by the judgement
    /// debt and by decay, both on the reasoning that pinning is itself a
    /// verdict - and it is served in full at every session start. Measured on
    /// the real store, that made 31 items 48.9% of every serving ever made,
    /// with 23 of them never examined by anything. Both design choices are
    /// defensible; the number they produce together had never been counted.
    #[test]
    fn the_pinned_line_counts_what_no_maintenance_loop_ever_looks_at() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("pinned-one")).unwrap();

            let mut anchored = rule("anchored-one");
            anchored.text = "a webhook retry backs off before it gives up entirely".to_string();
            anchored.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "docs/NOTES.md".to_string(),
            }];
            anchored.project = Some("fixture-project".to_string());
            store::declare(&mut s, "s", "l", "a", &anchored).unwrap();

            for _ in 0..3 {
                serve::deliver::record_delivery(&mut s, "s", "s", "t", "2026-08-08T00:00:00Z", &["pinned-one".to_string()]);
            }
            serve::deliver::record_delivery(&mut s, "s", "s", "t", "2026-08-08T00:00:00Z", &["anchored-one".to_string()]);
        }
        let line = pinned_line(&db);
        assert!(line.contains("1 item(s)"), "{line}");
        assert!(line.contains("1 never examined"), "{line}");
        assert!(line.contains("75%"), "three of four servings came from the pin: {line}");
    }

    #[test]
    fn the_gate_line_says_plainly_when_the_gate_has_never_done_anything() {
        // A fresh store must not read as "fine". A gate with zero refusals is
        // either a very well behaved session or a gate that is not wired in,
        // and the line has to leave both readings open rather than print a
        // reassuring nothing.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let _ = EventStore::new(&db).unwrap();
        }
        let line = gate_line(&db);
        assert!(line.contains("never refused"), "{line}");
    }

    #[test]
    fn the_gate_line_counts_refusals_and_stand_asides_apart() {
        // Apart, and never summed: a refusal is visible to whoever it
        // refused, a stand-aside is visible to nobody, and it was exactly the
        // invisible half that hid a broken gate for the whole of 2.0 until
        // 2026-08-08.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            for (kind, id) in [
                (thor_core::event_store::EventKind::GateRefused, "rule-a"),
                (thor_core::event_store::EventKind::GateRefused, "rule-a"),
                (thor_core::event_store::EventKind::GateRefused, "rule-b"),
                (thor_core::event_store::EventKind::GateStoodAside, "rule-c"),
            ] {
                s.append_event("s", "l", "t", kind, id, None, "{}").unwrap();
            }
        }
        let line = gate_line(&db);
        assert!(line.contains("3 refusals"), "{line}");
        assert!(line.contains("2 distinct rules"), "the same rule twice is one rule: {line}");
        assert!(line.contains("1 stand-aside"), "{line}");
    }

    #[test]
    fn the_gate_line_is_part_of_the_report_not_an_optional_extra() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let _ = EventStore::new(&db).unwrap();
        }
        let lines = report(&db, None, None, None, None, None);
        assert!(lines.iter().any(|l| l.starts_with("gate:")), "{lines:#?}");
    }

    #[test]
    fn the_proof_line_is_part_of_the_report_not_an_optional_extra() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("prose-only")).unwrap();
        }
        let lines = report(&db, None, None, None, None, None);
        assert!(
            lines.iter().any(|l| l.starts_with("provable rules:")),
            "the health check must report proof coverage every run: {lines:?}"
        );
    }

    #[test]
    fn falsifier_line_counts_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("has-one")).unwrap();
            s.append_event(
                "s", "l", "a", thor_core::event_store::EventKind::FactCreated, "old-1", None,
                r#"{"id":"old-1","kind":"rule","text":"pre-migration","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null}"#,
            )
            .unwrap();
        }
        let store = EventStore::open_existing(&db).unwrap();
        let _ = store; // keep the connection alive for the duration of the call below
        let line = falsifier_line(&db);
        assert!(line.contains("1 of 2"), "{line}");
    }

    #[test]
    fn report_has_one_line_per_component_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let lines = report(&db, None, None, None, None, None);
        assert_eq!(lines.len(), 13);
        assert!(lines[0].starts_with("memory store:"));
        assert!(lines[1].starts_with("code index:"));
        assert!(lines[2].starts_with("replica:"));
        assert!(lines[3].starts_with("falsifiers:"));
        assert!(lines[4].starts_with("provable rules:"));
        // Straight after the coverage number, on purpose: one says how many
        // rules COULD refuse, the next says how often one DID. Read apart
        // they mislead in opposite directions.
        assert!(lines[5].starts_with("gate:"));
        // Next to the gate on purpose: one says how often it fired, the next
        // two say where it has no teeth and what nothing ever re-reads.
        assert!(lines[6].starts_with("teeth:"));
        assert!(lines[7].starts_with("pinned:"));
        assert!(lines[8].starts_with("decay:"));
        assert!(lines[9].starts_with("crowding:"));
        assert!(lines[10].starts_with("unjudged:"));
        assert!(lines[11].starts_with("semantic search:"));
        assert!(lines[12].starts_with("project keys:"));
    }

    /// THE DEFECT THIS PREVENTS: a health line that goes quiet exactly when it
    /// has nothing to read. Without a checkout there is no way to know which
    /// pool an item competes in, and guessing would be worse than saying so -
    /// the same stance `decay_line` takes one line above.
    #[test]
    fn crowding_says_what_it_needs_rather_than_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert!(crowding_line(&db, None).contains("--checkouts"), "{}", crowding_line(&db, None));
    }

    /// The empty case has to read as an answer, not as a failure: a store whose
    /// every item wins a place somewhere is healthy, and a line that stays
    /// silent about that is indistinguishable from one that broke.
    #[test]
    fn crowding_on_a_store_with_nothing_to_probe_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let checkouts = dir.path().join("checkouts");
        std::fs::create_dir_all(&checkouts).unwrap();
        let line = crowding_line(&db, Some(&checkouts));
        assert!(line.starts_with("crowding:"), "{line}");
        assert!(!line.contains("--checkouts"), "a checkout WAS given: {line}");
    }

    // ------------------------------------------------------- semantic_line

    #[test]
    fn semantic_line_is_never_blank_regardless_of_the_store() {
        // Named after the defect it prevents: doctor must always state the
        // active search mode, in every build - this is the one health line
        // that never even opens the store to answer.
        let line = semantic_line(None);
        assert!(line.starts_with("semantic search:"), "{line}");
    }

    #[test]
    fn semantic_line_agrees_with_serves_own_search_mode() {
        // Deliberately never relies on which --features flag built THIS
        // binary (that would make the test's own expectation a guess): it
        // reads `serve::semantic_paths::search_mode` for the same explicit,
        // deliberately-empty directory and checks doctor's line agrees with
        // it - meaningful in either build. Passes an explicit temp dir
        // rather than `None`, since a real model may already be installed
        // at this machine's own default model directory.
        let dir = tempfile::tempdir().unwrap(); // no model files at all
        let line = semantic_line(Some(dir.path()));
        match serve::semantic_paths::search_mode(Some(dir.path())) {
            serve::semantic_paths::SearchMode::CompiledOut => assert!(line.contains("not compiled"), "{line}"),
            serve::semantic_paths::SearchMode::ModelMissing => assert!(line.contains("no model found"), "{line}"),
            serve::semantic_paths::SearchMode::ModelPresent => panic!("an empty directory must never read as present"),
        }
    }

    /// THE DEFECT THIS LINE EXISTS FOR. Three times on 2026-08-03 an item
    /// named a project that resolution could never produce, and every time it
    /// was invisible: a scoped-out rule looks exactly like a rule that never
    /// matched. This asserts the report NAMES such a key and says how many
    /// items hold it, rather than counting them as healthy.
    #[test]
    fn a_project_key_no_checkout_answers_to_is_named_with_its_weight() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut owned = rule("orphan-1");
            owned.project = Some("Nobody-Answers-To-This".to_string());
            store::declare(&mut store, "s", "l", "a", &owned).unwrap();
        }
        // A checkouts root with one real checkout that resolves to something else.
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Some-Other-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let line = orphan_projects_line(&db, Some(&checkouts));
        assert!(line.contains("Nobody-Answers-To-This"), "the key must be named: {line}");
        assert!(line.contains("1 item"), "and weighted by how many items hold it: {line}");
    }

    #[test]
    fn a_store_whose_keys_all_resolve_says_so_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut owned = rule("ok-1");
            owned.project = Some("Real-Repo".to_string());
            store::declare(&mut store, "s", "l", "a", &owned).unwrap();
        }
        let line = orphan_projects_line(&db, Some(&checkouts));
        assert!(line.contains("every key in use resolves"), "{line}");
    }

    // --------------------------------------------------------- gate_verdict

    /// THE DEFECT THIS PREVENTS: a cloud session with no thor.db at all
    /// getting blocked by --gate for the absence of something it was never
    /// going to have. FAIL OPEN on a missing store is not a nicety here - it
    /// is the one behaviour --gate must never regress on.
    #[test]
    fn gate_verdict_fails_open_when_the_store_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        assert_eq!(gate_verdict(&missing, None, None), GateVerdict::NotAvailable);
        assert!(!missing.exists(), "judging the gate must never create a store");
    }

    /// THE DEFECT THIS PREVENTS: a store that is present but corrupt or
    /// mid-sync (a half-written replica, a file that is not a database at
    /// all) turning --gate into a build failure that says nothing about
    /// whether the change actually being gated is wrong.
    #[test]
    fn gate_verdict_fails_open_when_the_store_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("not-a-store.db");
        std::fs::write(&db, b"not a sqlite file").unwrap();
        assert_eq!(gate_verdict(&db, None, None), GateVerdict::NotAvailable);
    }

    /// THE DEFECT THIS PREVENTS: --gate failing every single commit just
    /// because decay could never be checked (no --checkouts given) or
    /// because most rules are prose-only - neither is a defect in what
    /// changed, both are whole-store debt, and failing on either is exactly
    /// the noise that trains people to bypass a gate.
    #[test]
    fn gate_verdict_is_clean_on_a_healthy_store_with_nothing_gate_worthy() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("prose-only")).unwrap();
        }
        assert_eq!(gate_verdict(&db, None, None), GateVerdict::Clean);
    }

    /// THE DEFECT THIS PREVENTS: an item whose anchor points at nothing - it
    /// fires nowhere - reading as a clean gate, the exact rot that sat
    /// undetected across 128 anchors in four projects until `decay_line` was
    /// built (see `decay_check`'s own doc comment).
    #[test]
    fn gate_verdict_fails_on_a_dead_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("dead-anchor-1");
            item.project = Some("Real-Repo".to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "missing.txt".to_string() }];
            store::declare(&mut s, "s", "l", "a", &item).unwrap();
        }
        assert_eq!(gate_verdict(&db, Some(&checkouts), None), GateVerdict::Failing);
    }

    /// The contrast case for the test above: an anchor that DOES resolve
    /// must never fail the gate, or every project would fail on day one.
    #[test]
    fn gate_verdict_is_clean_when_every_anchor_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("present.txt"), "content").unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("live-anchor-1");
            item.project = Some("Real-Repo".to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "present.txt".to_string() }];
            store::declare(&mut s, "s", "l", "a", &item).unwrap();
        }
        assert_eq!(gate_verdict(&db, Some(&checkouts), None), GateVerdict::Clean);
    }

    /// THE DEFECT THIS PREVENTS: a rule whose own machine-runnable claim is
    /// presently false reading as a clean gate because nothing besides the
    /// anchor was ever checked.
    #[test]
    fn gate_verdict_fails_on_a_proof_that_now_comes_out_false() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("false-proof-1");
            item.project = Some("Real-Repo".to_string());
            item.check = Some(Check::PathExists { path: "missing.txt".to_string() });
            store::declare(&mut s, "s", "l", "a", &item).unwrap();
        }
        assert_eq!(gate_verdict(&db, Some(&checkouts), None), GateVerdict::Failing);
    }

    /// THE DEFECT THIS PREVENTS: two checkouts sharing one --checkouts
    /// directory means one repository's rot could fail a DIFFERENT
    /// repository's gate - the exact reason --project exists.
    #[test]
    fn gate_verdict_with_a_project_filter_ignores_another_projects_rot() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        std::fs::create_dir_all(checkouts.join("Repo-A").join(".git")).unwrap();
        std::fs::create_dir_all(checkouts.join("Repo-B").join(".git")).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut rotten = rule("rotten-in-a");
            rotten.project = Some("Repo-A".to_string());
            rotten.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "missing.txt".to_string() }];
            store::declare(&mut s, "s", "l", "a", &rotten).unwrap();

            let mut clean = rule("clean-in-b");
            clean.project = Some("Repo-B".to_string());
            store::declare(&mut s, "s", "l", "a", &clean).unwrap();
        }

        assert_eq!(
            gate_verdict(&db, Some(&checkouts), None),
            GateVerdict::Failing,
            "unscoped, Repo-A's dead anchor must still fail the gate"
        );
        assert_eq!(
            gate_verdict(&db, Some(&checkouts), Some("Repo-B")),
            GateVerdict::Clean,
            "Repo-B's own items are fine, and Repo-A's rot must not reach it"
        );
        assert_eq!(
            gate_verdict(&db, Some(&checkouts), Some("Repo-A")),
            GateVerdict::Failing,
            "scoped to its own project, Repo-A's dead anchor must still fail"
        );
    }

    /// THE DEFECT THIS PREVENTS: a tampered event log reading as a clean
    /// gate because --gate only ever looked at decay. A broken chain is
    /// worse than any single item's rot, so it fails regardless of
    /// --project, exactly as `gate_verdict`'s own doc comment says it must.
    #[test]
    fn gate_verdict_fails_on_a_broken_chain_regardless_of_project() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("r1")).unwrap();
        }
        {
            // Tamper the row directly, the same way
            // `test_fsck_recomputes_hashes_on_tampered_fields` (core::auditor)
            // proves a stale body_ch does not hide a body flip: this_hash was
            // computed over the ORIGINAL body at write time, so recomputing it
            // over the tampered body can never match again.
            let s = EventStore::open_existing(&db).unwrap();
            s.conn().execute("UPDATE event SET body = 'tampered' WHERE seq = 1", []).unwrap();
        }
        assert_eq!(gate_verdict(&db, None, None), GateVerdict::Failing);
        assert_eq!(
            gate_verdict(&db, None, Some("some-project-that-does-not-exist")),
            GateVerdict::Failing,
            "a broken chain must not be scoped away by --project"
        );
    }

    /// THE DEFECT THIS PREVENTS: an incomplete --checkouts directory (one
    /// sibling checkout missing, a project renamed or archived on purpose)
    /// failing a commit for a reason that has nothing to do with the commit
    /// itself - exactly the false positive `gate_verdict`'s own doc comment
    /// names as the reason orphan project keys are left out of --gate.
    #[test]
    fn gate_verdict_does_not_fail_when_a_projects_own_checkout_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        std::fs::create_dir_all(&checkouts).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("ghost-1");
            item.project = Some("Ghost-Project".to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "missing.txt".to_string() }];
            store::declare(&mut s, "s", "l", "a", &item).unwrap();
        }
        assert_eq!(gate_verdict(&db, Some(&checkouts), None), GateVerdict::Clean);
    }
}
