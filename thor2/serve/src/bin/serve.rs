//! The four channels over the one serving path (`serve` crate's `lib.rs`):
//!
//!   hook   Claude Code PreToolUse: JSON on stdin, JSON on stdout, ALWAYS
//!          exit 0, silent on any failure (R5's "read and inject" policy).
//!   check  human-facing: what would fire for a given command/file/target.
//!   why    everything that applies, including what the block would withhold.
//!   audit  declared items vs. their ItemServed history - "declared, never
//!          delivered" as a query, not a thing read off a report by eye.
//!
//! check/why/audit are diagnostic tools for a person and are allowed to
//! report a real error (R5's "write and declare" policy does not apply here
//! either, but "never silent" is still the right instinct for a human-facing
//! tool - only `hook` is required to stay quiet).

use clap::{Parser, Subcommand};
use intent::Action;
use model::item::TargetKind;
use serde_json::Value;
use serve::decay::DecayContext;
use serve::input::ServeInput;
use serve::{absent_guard, capture, deliver, judge, lookup, mark, project, prompt, render, respond, session_start, stale_guard, status, time};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(name = "serve", about = "Select, rank, cap and render items - one path, four channels")]
struct Cli {
    /// Path to the core event-log database.
    #[arg(long)]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The real production delivery boundary for surfaces 1 (session start),
    /// 2 (moment of action) and 3 (per prompt): JSON on stdin, JSON on
    /// stdout, exit 0 always, silent on any failure. Branches on the JSON
    /// payload's `hook_event_name` ("SessionStart" / "UserPromptSubmit" /
    /// anything else, treated as a PreToolUse-shaped tool call).
    Hook,
    /// What would fire for a given command, file, target or moment (surface
    /// 2 preview - does not count as a delivery).
    Check(TargetArgs),
    /// Everything that applies, including what the block would withhold
    /// (surface 2 preview - does not count as a delivery).
    Why(TargetArgs),
    /// Declared items vs. their delivery history.
    Audit,
    /// Surface 1 preview: every Always item, global plus (optionally) one
    /// project - full, never capped. Does not count as a delivery; only
    /// `hook`'s SessionStart branch does that.
    SessionStart {
        /// The current project's key. Omitted = resolve it automatically
        /// from the current working directory (`serve::project::
        /// resolve_project` - the one place in the workspace that performs
        /// this resolution: a marker file wins, else the git root's
        /// basename, else global-only). Pass this to preview a different
        /// project without changing directory.
        #[arg(long)]
        project: Option<String>,
    },
    /// Surface 3 preview: what a raw prompt resolves to, or nothing. Does
    /// not count as a delivery; only `hook`'s UserPromptSubmit branch does
    /// that.
    Prompt {
        #[arg(long)]
        text: String,
    },
    /// Surface 4 (explicit lookup): free-text search over every live item,
    /// any project, archive kinds (Report/Chunk) included.
    Search {
        query: String,
    },
    /// Surface 4's other door: an explicit request for one Lookup's key.
    LookupKey {
        key: String,
    },
    /// Every address this memory can be asked for by name: the registers
    /// (`Lookup` items, returned whole and never ranked) and how many
    /// documents each scope holds, plus how many live items carry no scope
    /// at all. Read-only. The same answer the agent's `lookup` gives when it
    /// is called with neither a query nor a key - one definition, two doors.
    Catalog,
    /// Surface 4's code door: search the code index built by the code-index
    /// binary against a repository. Every hit carries the commit it came
    /// from; the index's own drift against the repository's current state
    /// is printed first. Never an injection surface - see this crate's own
    /// guard test proving so (`serve/tests`, code-index-only-via-lookup).
    SearchCode {
        /// Path to the code index's own sqlite file (built with the
        /// code-index binary's own `<db> <repo> build` subcommand).
        #[arg(long = "index-db")]
        index_db: PathBuf,
        /// Path to (or inside) the repository the index was built from.
        #[arg(long)]
        repo: PathBuf,
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Who defines and who uses one symbol name - the blast radius of a
    /// change, as file and line, with the index's own provenance first.
    /// Resolution is by bare name only: two unrelated things sharing a name
    /// come back together, so this points at places to open, never at a
    /// conclusion.
    WhereUsed {
        #[arg(long = "index-db")]
        index_db: PathBuf,
        #[arg(long)]
        repo: PathBuf,
        name: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// What one file defines, in line order. A path the index has never seen
    /// is reported as not indexed, which is a different answer from "this
    /// file defines nothing".
    Outline {
        #[arg(long = "index-db")]
        index_db: PathBuf,
        /// Repository-relative path, exactly as the index stores it.
        path: String,
    },
    /// What this store knows right now: how many live items per kind, how
    /// many are declared but never fired, how many were served repeatedly
    /// without ever being marked useful, and - when a code index is named -
    /// which commit it is at and how far the working copy has drifted.
    Status {
        #[arg(long = "index-db")]
        index_db: Option<PathBuf>,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Record that a served item actually helped. Recorded in the log and
    /// reported by `status`; it does NOT decide whether anything fires - see
    /// `serve::decay`'s own doc comment for why silence stopped removing
    /// things on 2026-08-03. Never a ranking nudge either.
    Mark {
        id: String,
        /// Record the OPPOSITE judgement: this item did not belong where it
        /// fired. Two of these, with no mark of usefulness, retire it from
        /// the injection surfaces - it stays fully findable via search.
        #[arg(long)]
        noise: bool,
    },
    /// Rebuild the meaning-search vector sidecar (feature `semantic`) from
    /// every live item `search` itself covers (any project, archive kinds
    /// included, never a Lookup). Overwrites whatever was stored before.
    #[cfg(feature = "semantic")]
    VectorsBuild {
        /// Override the model directory (default:
        /// `serve::semantic_paths::default_model_dir`).
        #[arg(long = "model-dir")]
        model_dir: Option<PathBuf>,
        /// Override the sidecar's own path (default:
        /// `serve::semantic_paths::default_vectors_path`).
        #[arg(long = "vectors-db")]
        vectors_db: Option<PathBuf>,
    },
    /// How many vectors are stored (feature `semantic`) and whether they
    /// still match the current content: missing (no vector yet), stale (the
    /// item's text changed since it was embedded), orphaned (the id is no
    /// longer live), plus whether the stored model_id matches this binary's.
    #[cfg(feature = "semantic")]
    VectorsStatus {
        #[arg(long = "vectors-db")]
        vectors_db: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
struct TargetArgs {
    /// A command about to run (derives its own moments; also a Command doel).
    #[arg(long)]
    command: Option<String>,
    /// A file about to be touched (derives its own moments; also a Path doel).
    #[arg(long)]
    file: Option<String>,
    /// An explicit target, `<kind>:<value>` (repeatable), kind one of
    /// path/dir/symbol/command/project/route/host.
    #[arg(long = "target", value_parser = parse_target_arg)]
    targets: Vec<(TargetKind, String)>,
    /// An explicit moment, named directly (repeatable).
    #[arg(long = "moment", value_parser = parse_moment_arg)]
    moments: Vec<Action>,
}

fn parse_target_arg(s: &str) -> Result<(TargetKind, String), String> {
    let (kind_str, value) = s
        .split_once(':')
        .ok_or_else(|| format!("expected <kind>:<value> (e.g. path:src/main.rs), got '{s}'"))?;
    let kind = TargetKind::from_str(kind_str).map_err(|e| e.to_string())?;
    Ok((kind, value.to_string()))
}

fn parse_moment_arg(s: &str) -> Result<Action, String> {
    match model::gate::parse_moment(s) {
        Ok(model::item::Binding::Moment(action)) => Ok(action),
        Ok(_) => unreachable!("parse_moment only ever returns Binding::Moment"),
        Err(e) => Err(e.to_string()),
    }
}

/// The preview commands (`check`, `why`) build the same input the hook does,
/// project included. Without that they would answer a question the hook
/// answers differently - the same two-doors defect that let the MCP `lookup`
/// tool and the `search` CLI drift apart earlier the same day. A preview whose
/// answer does not match the real surface is worse than no preview.
fn build_input(args: &TargetArgs) -> ServeInput {
    let mut input = ServeInput {
        project: std::env::current_dir().ok().and_then(|dir| project::resolve_project(&dir)),
        ..Default::default()
    };
    if let Some(command) = &args.command {
        input.add_command(command);
    }
    if let Some(file) = &args.file {
        input.add_file(file);
    }
    for (kind, value) in &args.targets {
        input.add_target(*kind, value);
    }
    for action in &args.moments {
        input.add_moment(*action);
    }
    input
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Hook => cmd_hook(&cli.db),
        Command::Check(args) => cmd_check(&cli.db, &build_input(&args)),
        Command::Why(args) => cmd_why(&cli.db, &build_input(&args)),
        Command::Audit => cmd_audit(&cli.db),
        Command::SessionStart { project } => {
            let resolved = project.or_else(current_project_from_cwd);
            cmd_session_start(&cli.db, resolved.as_deref())
        }
        Command::Prompt { text } => cmd_prompt(&cli.db, &text),
        Command::Search { query } => cmd_search(&cli.db, &query),
        Command::LookupKey { key } => cmd_lookup_key(&cli.db, &key),
        Command::Catalog => {
            let store = open_store_or_die(&cli.db);
            print!("{}", lookup::render_catalog(&lookup::catalog(&store)));
        }
        Command::SearchCode { index_db, repo, query, limit } => cmd_search_code(&index_db, &repo, &query, limit),
        Command::WhereUsed { index_db, repo, name, limit } => cmd_where_used(&index_db, &repo, &name, limit),
        Command::Outline { index_db, path } => cmd_outline(&index_db, &path),
        Command::Status { index_db, repo } => cmd_status(&cli.db, index_db.as_deref(), repo.as_deref()),
        Command::Mark { id, noise } => cmd_mark(&cli.db, &id, noise),
        #[cfg(feature = "semantic")]
        Command::VectorsBuild { model_dir, vectors_db } => cmd_vectors_build(&cli.db, model_dir.as_deref(), vectors_db.as_deref()),
        #[cfg(feature = "semantic")]
        Command::VectorsStatus { vectors_db } => cmd_vectors_status(&cli.db, vectors_db.as_deref()),
    }
}

/// Resolve "the current project" from the process' own working directory,
/// through the one place that does this (`project::resolve_project`). A
/// working directory that cannot even be read degrades to `None` (global
/// only) rather than erroring - this is a human-facing preview command, but
/// "what project am I in" is exactly the kind of read that should never
/// block on an unusual environment.
fn current_project_from_cwd() -> Option<String> {
    std::env::current_dir().ok().and_then(|dir| project::resolve_project(&dir))
}

// --------------------------------------------------------------------- hook

/// Everything the hook channel does that could possibly fail, collapsed to
/// "nothing to say" on any of it: a bad payload, a missing/corrupt store, or
/// nothing matching. Never panics by design, but see `cmd_hook` for the
/// belt-and-braces around a panic anyway.
///
/// This is the ONE real production delivery boundary for surfaces 1, 2 and
/// 3 (CONTRACT.md: "elke levering aan oppervlak 1, 2 of 3 legt vast dat het
/// item gevuurd heeft") - every branch below that returns a block also
/// records delivery for exactly the ids shown, via the same
/// `deliver::record_delivery` call. `check`/`why`/`Prompt`/`SessionStart`
/// (the human-facing preview commands) call the identical selection
/// functions but never this one, so a dry run never inflates "how often did
/// this actually fire".
/// What a hook run produces. THREE shapes, because the Response Guard's own
/// two verdicts each speak a different language from the injection surfaces,
/// and from each other: the surfaces hand the model `additionalContext`, a
/// BLOCK hands it a `decision`, and a WARN (added 2026-08-06) hands the
/// OWNER a top-level `systemMessage` and the model an `additionalContext`,
/// both carrying the same text, never a `decision` - see `Warn`'s own doc
/// comment below for what Claude Code's own hooks documentation confirms
/// about this shape, and the one question it still leaves open.
/// One line for the owner when this project's memory has quietly rotted,
/// or nothing at all.
///
/// Counts two things against the checkout this session started in: anchors
/// that resolve to no file (those facts fire NOWHERE) and checks that now
/// come out FALSE. Both accumulate while nobody is writing, which is
/// exactly when no gate is watching.
///
/// Debounced to once a day through a stamp file beside the store, for the
/// same reason the backup hook debounces: a line that appears every single
/// session stops being read by the third one.
///
/// SILENT ON EVERY FAILURE, per R5's read-and-inject policy: no checkout, no
/// git root, an unreadable stamp, a check that cannot run - all of it means
/// no notice, never a guess and never an error on this channel.
/// The binary named `name` sitting next to this one, with the platform's own
/// executable suffix. Mirrors `ops::install`'s own `sibling` helper: nothing
/// in this deployment is ever on PATH, so a message that names a bare
/// command someone could type is not actually runnable without this.
fn sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

fn decay_notice(store: &EventStore, db: &Path, cwd: Option<&Path>) -> Option<String> {
    let root = project::git_root(cwd?)?;
    let project = project::resolve_project(&root)?;

    let stamp = db.with_file_name("decay-notice.stamp");
    let today = time::now_iso8601().get(..10)?.to_string();
    if std::fs::read_to_string(&stamp).ok().map(|s| s.trim().to_string()) == Some(today.clone()) {
        return None;
    }

    let (mut dead, mut failing) = (0usize, 0usize);
    let mut found: Vec<absent_guard::StaleFinding> = Vec::new();
    // Every item whose check this pass actually RAN. That is the set whose
    // recovery this scan is entitled to record - see `record_stale_text` for
    // the day a healed proof went on being reported as broken.
    let mut scanned_ids: Vec<String> = Vec::new();
    for li in serve::live::live_items(store).iter().filter(|li| li.item.kind.can_fire()) {
        if li.item.project.as_deref() != Some(project.as_str()) {
            continue;
        }
        for binding in &li.item.bindings {
            if let model::item::Binding::Target { kind: model::item::TargetKind::Path, value } = binding {
                if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                    continue;
                }
                if !root.join(value.replace('\\', "/")).exists() {
                    dead += 1;
                }
            }
        }
        if let Some(check) = li.item.check.as_ref() {
            let outcome = model::check::run(check, &root);
            if outcome != model::check::Outcome::CannotRun {
                scanned_ids.push(li.id.clone());
            }
            if outcome == model::check::Outcome::Fails {
                failing += 1;
                // A proof that RAN and came out false is the strongest thing
                // this system can say about a stored fact: it is provably out
                // of date. Recording it here is what makes the difference
                // between telling somebody and asking them.
                found.push(absent_guard::StaleFinding {
                    id: li.id.clone(),
                    outcome: absent_guard::StaleOutcome::Failed,
                    check: format!("{check:?}"),
                    file: root.display().to_string(),
                });
            }
        }
    }
    // Clear first, and BEFORE the early return below: a scan that finds
    // everything healthy is exactly the scan that has something to retract.
    if let Ok(next_seq) = store.get_next_seq() {
        let stale_path = absent_guard::default_stale_path(db);
        let existing = std::fs::read_to_string(&stale_path).ok();
        if let Some(text) = absent_guard::record_stale_text(
            existing.as_deref(),
            &found,
            &scanned_ids,
            next_seq.saturating_sub(1),
        ) {
            let _ = std::fs::write(&stale_path, text);
        }
    }
    if dead == 0 && failing == 0 {
        return None;
    }

    // WHY THIS WRITES INTO THE SAME SIDECAR THE FILE GUARD USES. Until now
    // this scan was a dead end: it is the only thing that looks at EVERY
    // fact of a project, including the ones whose file nobody touched, and
    // all it did was print a count. The two Stop-time guards that actually
    // make somebody act only ever saw what was written during the turn. So
    // the mechanism that finds the rot could not ask, and the mechanisms
    // that ask could not see it - which is exactly why a session only ever
    // worked on staleness when the owner brought it up himself.
    //
    // Handing the findings to `absent_guard`'s staleness sidecar closes that
    // without inventing a second loop: `serve::stale_guard` already reads it
    // at Stop, already asks about one at a time, and already treats an item
    // as settled once it has been revised or retracted. Only FAILING proofs
    // go in - a dead anchor means a fact fires nowhere, which is bad but is
    // not evidence that it is wrong, and some of them are dead on purpose.

    let _ = std::fs::write(&stamp, &today);
    let doctor = sibling("doctor").unwrap_or_else(|| PathBuf::from("doctor"));
    let checkouts = root.parent().unwrap_or(&root);
    Some(format!(
        "[THOR] {project}: {dead} anchor(s) point at a file that is not there, so those facts fire nowhere, and {failing} proof(s) now come out false. Run `\"{}\" --db \"{}\" --checkouts \"{}\"` to see each one named on the decay line, then repair per id with revise (the file moved) or retract (the fact went with it). That report counts EVERY project under the checkouts directory, so its two numbers are larger than this line's, which counts only {project}.",
        doctor.display(), db.display(), checkouts.display()
    ))
}

enum HookOutput {
    /// An injection surface's block, wrapped by the caller as
    /// `hookSpecificOutput.additionalContext`.
    Context { event_name: String, block: String },
    /// An injection block PLUS a one-line notice for the OWNER.
    ///
    /// WHY A SECOND AUDIENCE AT SESSION START. A memory decays while nobody
    /// is writing: files move, and the anchors that pointed at them stop
    /// firing. The health check counts that, but a count in a command
    /// somebody has to run is exactly the suggestion this project keeps
    /// refusing to rely on - the owner never once saw it, because he never
    /// ran it. This carries the same block to the model and, only when
    /// something is actually wrong, one line to him.
    ContextWithNotice { event_name: String, block: String, notice: String },
    /// A verdict, printed verbatim - `{"decision":"block","reason":...}`.
    /// Asks the model to reconsider (Stop) or refuses a tool call
    /// (PreToolUse) rather than adding to its context. Three surfaces share
    /// this shape: the Response Guard (`respond`, Stop), and Lane C's
    /// capture guard C2 (Stop, `capture::decide_stop`) and C3 (PreToolUse,
    /// `capture::sink_verdict`'s `Block` case) - see `serve::capture`'s own
    /// doc comment.
    Decision(Value),
    /// A WARN-tier verdict at Stop (`respond::GuardVerdict::warn_reason`):
    /// visible text that must NEVER stop the turn - the third verdict
    /// SPEC-ENFORCEMENT.md 1.1 names, that before this variant existed had no
    /// way to reach the owner at all (a rulebook rule could already carry
    /// `"tier":"warn"` and `respond::guard_verdict` would already compute the
    /// text, but nothing ever read `GuardVerdict::warn_reason` - see
    /// `respond.rs`'s own "WARN TIER" section for the state this left off
    /// at, and `hook_once`'s `Stop` arm below for where this is now built).
    ///
    /// `cmd_hook` renders this as TWO independent fields carrying the same
    /// `text`, one per audience, instead of the one shape `Context` uses: a
    /// top-level `systemMessage` for the OWNER, and
    /// `hookSpecificOutput.additionalContext` (keyed to this same
    /// `event_name`, always `"Stop"` in practice - `hook_once`'s `Stop` arm
    /// is the only place this variant is ever built) for the MODEL. Neither
    /// field's presence implies the other, and setting them never sets a
    /// `"decision"` key - a WARN must never be readable as a block.
    ///
    /// Why two fields for one string: a Response Guard warning is about the
    /// reply the assistant just finished, and at `Stop` that turn is already
    /// over - the assistant cannot act on anything unless it is blocked, and
    /// a WARN by definition never blocks, so the model-facing half can only
    /// ever land on a LATER turn. The owner is the only audience who can
    /// still do something about it now, which is the reason this variant
    /// exists at all.
    ///
    /// CONFIRMED BY THE DOCS, not merely defensive any more: `Stop` accepts
    /// `additionalContext` for non-blocking feedback in the first place -
    /// "Stop and SubagentStop also accept
    /// `hookSpecificOutput.additionalContext` for non-error feedback that
    /// continues the conversation" - but that feedback is addressed to
    /// Claude, not the owner: "For non-blocking feedback that Claude should
    /// see and act on, use `hookSpecificOutput.additionalContext` instead",
    /// landing "at the end of the turn. The conversation continues so Claude
    /// can act on the feedback" - i.e. next turn, never this one. The field
    /// that actually reaches the owner is the different, top-level one this
    /// variant now also sets: "If you want to show a message to the user
    /// without blocking Claude, return `systemMessage` instead", which the
    /// universal field table every hook event shares (not `Stop`-specific)
    /// lists as "Warning message shown to the user". And the JSON never
    /// carrying a `"decision"` key is still proven the older way too, by
    /// this variant's own tests in `serve/tests/` against the real compiled
    /// binary - so no consumer that keys its blocking behaviour off
    /// `decision`/`reason` can ever read a WARN as a block.
    ///
    /// LEFT OPEN BY THE DOCS: whether `additionalContext` at `Stop` is
    /// actually surfaced to the model when it arrives WITHOUT a `decision`
    /// key - the only way this variant ever sends it, since a WARN by
    /// definition carries no `decision`. The docs say `Stop` takes the field
    /// for non-error feedback, but every worked example in that section
    /// pairs `additionalContext` with a block; none shows it standing alone.
    /// Left unresolved rather than guessed at again: this variant does not
    /// depend on the answer to do its job, because the `systemMessage` half
    /// - the reason it exists - is confirmed on its own and reaches the
    /// owner regardless. If `additionalContext` alone turns out to be
    /// silently ignored at `Stop`, that is the same "the model does not see
    /// it" outcome this whole shape was always chosen to fail into, not a
    /// new failure mode. If either assumption is ever found wrong, only this
    /// one variant's rendering in `cmd_hook` needs to change - nothing about
    /// `hook_once`'s own ordering logic depends on the JSON shape chosen
    /// here.
    Warn { event_name: String, text: String },
}

/// Whether this payload arrives from inside a Task-tool subagent rather than
/// the owner's own main session. `agent_id` is Claude Code's own documented
/// signal for exactly this question - the hooks reference describes it as
/// "present only when the hook fires inside a subagent call... use this to
/// distinguish subagent hook calls from main-thread calls." No other field is
/// used, and none is inferred: a check that misfires on the owner's OWN main
/// session would silently blind it, which is worse than the gap this closes
/// (see INJECTION-FRAMING.md's step 3 for what else was checked and ruled
/// out).
///
/// Confirmed 2026-08-05 against Claude Code's own hooks documentation:
/// `SessionStart` fires ONLY in the main session (it never fires for a
/// subagent at all, so a subagent gate on that arm would be dead code - see
/// `INJECTION-FRAMING.md`'s own addendum); `PreToolUse`, `UserPromptSubmit`
/// and `Stop` DO fire inside a subagent, and `agent_id`/`agent_type` are only
/// ever present in that case - which is exactly why this predicate now gates
/// Lane C's capture guard (C1/C2/C3, `capture_flag`/`capture_stop_check`/
/// `capture_sink_check` below): a subagent's own task prompt is written by an
/// orchestrating agent, not the owner, and is often full of the very words
/// ("always", "never", "from now on") this guard watches for - flagging it
/// would deadlock the subagent's own Stop on a "decision" the owner never
/// made.
fn payload_is_from_a_subagent(payload: &Value) -> bool {
    payload.get("agent_id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
}

fn hook_once(db_path: &Path) -> Option<HookOutput> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).ok()?;
    let payload: Value = serde_json::from_str(&raw).ok()?;

    let event_name = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("PreToolUse")
        .to_string();

    let session_id = payload.get("session_id").and_then(|v| v.as_str()).unwrap_or("hook").to_string();

    // Surface 5, the Response Guard - handled FIRST and without opening the
    // store, because it watches the assistant's reply, not the memory. See
    // `serve::respond` for the whole story of why this surface had to be
    // rebuilt. Reads the assistant's last message straight from the payload
    // (Claude Code puts it there - no transcript to parse), refuses to
    // re-fire when a guard already fired this turn (loop safety), and asks
    // `respond::guard_verdict` for BOTH tiers at once (SPEC-ENFORCEMENT.md
    // 1.1's three verdicts, `respond::GuardVerdict`) instead of the old
    // `respond::block_reason`, which could only ever see BLOCK. A BLOCK
    // reason returns immediately below, exactly as `block_reason` always did
    // - nothing after it in this function runs. A WARN reason (no block) is
    // instead held in `warn_text` rather than returned here: the ordering
    // guarantee this task exists to keep is that a WARN must never suppress
    // or delay Lane C's capture check or the stale-rule guard below, so it
    // only ever becomes visible (as `HookOutput::Warn`) at whichever point
    // below first finds nothing else to say - see `HookOutput::Warn`'s own
    // doc comment for the JSON shape this produces and exactly what is
    // proven versus assumed about it. Every error path (no rulebook,
    // unreadable rulebook, malformed JSON) yields neither tier - `warn_text`
    // stays `None` - and falls through the same way, either to Lane C's
    // capture check below (still on this Stop event) or, for every other
    // event name, past this whole block.
    if event_name == "Stop" {
        let already_fired = payload.get("stop_hook_active").and_then(|v| v.as_bool()).unwrap_or(false);
        let msg = payload.get("last_assistant_message").and_then(|v| v.as_str()).unwrap_or("");
        // THE DEFECT THIS CLOSES, reported by the owner on 2026-08-09: "my
        // TLDR rules worked an hour ago and now they are gone."
        //
        // `stop_hook_active` means a Stop hook already held this turn once.
        // Returning None on it was loop safety, and it was too wide: it
        // switched off the Response Guard as well as the debts. That went
        // unnoticed while blocks were rare. The moment one debt started
        // firing every turn, EVERY follow-up reply landed in an already-fired
        // turn, and the guard silently stopped watching any of them - exactly
        // the replies most likely to need it, since they come after a nudge.
        //
        // So the second pass still reads the reply and still says what it
        // found, as a WARN. A warning cannot hold the turn, so it cannot
        // loop; the loop safety that matters is keeping BLOCK to one pass,
        // not going blind.
        if already_fired {
            if msg.trim().is_empty() {
                return None;
            }
            let rulebook_text = std::fs::read_to_string(respond::default_rulebook_path(db_path)).ok();
            let verdict = respond::guard_verdict(rulebook_text.as_deref(), msg);
            let text = verdict.block_reason.or(verdict.warn_reason)?;
            return Some(HookOutput::Warn { event_name, text });
        }
        let warn_text: Option<String> = if msg.trim().is_empty() {
            None
        } else {
            let rulebook_text = std::fs::read_to_string(respond::default_rulebook_path(db_path)).ok();
            let verdict = respond::guard_verdict(rulebook_text.as_deref(), msg);
            if let Some(reason) = verdict.block_reason {
                return Some(HookOutput::Decision(serde_json::json!({
                    "decision": "block",
                    "reason": reason,
                })));
            }
            verdict.warn_reason
        };

        // Lane C, C2 (SPEC-ENFORCEMENT.md 2.2) - the Response Guard above had
        // no BLOCK to return (a WARN, if any, is only pending in `warn_text`
        // so far, not yet returned), so this is still the one Stop path that
        // may open the store, and only read-only. See `capture_stop_check`'s
        // own doc comment. NEVER for a subagent (`payload_is_from_a_subagent`'s
        // own doc comment): a subagent's Stop must never be blocked over a
        // "decision" that lives only in its own delegated task prompt - but
        // the Response Guard's own WARN, if pending, still applies regardless
        // of subagent status, unchanged from its existing BLOCK behaviour
        // (`JUDGE-TRANSPORT.md`'s "Subagent gating": "The Response Guard...
        // is UNCHANGED and still runs on every Stop regardless of subagent
        // status - this fix is scoped to Lane C only"), so it is returned
        // here rather than silently dropped.
        if payload_is_from_a_subagent(&payload) {
            return warn_text.map(|text| HookOutput::Warn { event_name, text });
        }
        if let Some(output) = capture_stop_check(db_path, &session_id) {
            return Some(output);
        }

        // THE MESS YOU MADE THIS SESSION, before the older debt below it.
        // Asked first because it is the only one whose answer decays: the
        // person who wrote the fact still knows what it was for, and tomorrow
        // nobody does.
        // The project THIS session is working in, from the payload's own cwd -
        // resolved here because the shared `session_project` below is computed
        // after this early Stop branch returns. It routes the crowding nag to
        // the session whose project the fact belongs to (see crowding_debt).
        let stop_project = payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .as_deref()
            .and_then(project::resolve_project);
        if let Some(reason) = EventStore::open_existing(db_path)
            .ok()
            .and_then(|s| crowding_debt(&s, db_path, &session_id, stop_project.as_deref()))
        {
            return Some(HookOutput::Decision(
                serde_json::json!({ "decision": "block", "reason": reason }),
            ));
        }

        // The one debt nobody ever pays voluntarily. Independent of the
        // capture guard above it - that one can be switched off and usually
        // is, and this must not go quiet with it.
        if let Some(reason) = EventStore::open_existing(db_path).ok().and_then(|s| judgement_debt(&s, &session_id)) {
            return Some(HookOutput::Decision(
                serde_json::json!({ "decision": "block", "reason": reason }),
            ));
        }

        // The backlog burn: one fact per turn that LOOKS armable and has
        // never been asked. Last of the debts on purpose - it is the only one
        // with no urgency, and it must never speak over a mess made this
        // session or a verdict that is owed.
        if let Some(reason) = EventStore::open_existing(db_path)
            .ok()
            .filter(|_| teeth_not_yet_asked_this_session(db_path, &session_id))
            .and_then(|s| teeth_debt(&s))
        {
            record_teeth_asked(db_path, &session_id);
            return Some(HookOutput::Decision(
                serde_json::json!({ "decision": "block", "reason": reason }),
            ));
        }

        // The stale-rule guard (`serve::stale_guard`) - AFTER both the
        // Response Guard above and Lane C's capture guard just above, on
        // purpose: it never overrides either, only fills the silence when
        // neither had anything to say. See `stale_guard_stop_check`'s own
        // doc comment for the full doctrine. Same subagent exemption as
        // Lane C, inherited for free from the `payload_is_from_a_subagent`
        // check above rather than repeated here. A pending WARN loses to a
        // maintenance BLOCK here exactly as it already lost to Lane C's own
        // BLOCK just above - only once NEITHER has anything to say does the
        // WARN, if any, finally reach the owner, on the very last line below.
        if let Some(output) = stale_guard_stop_check(db_path, &session_id) {
            return Some(output);
        }
        return warn_text.map(|text| HookOutput::Warn { event_name, text });
    }

    // The session's own working directory, and the project resolved from it -
    // both resolved ONCE for every surface below. Claude Code puts the
    // session's working directory in "cwd" on every hook payload; a payload
    // without one falls back to this process' own directory rather than
    // assuming. `session_cwd` is kept (not just consumed into a project key)
    // because the Absent-check guard (`absent_guard_block` below) also needs
    // a real filesystem root to prove a check's own currency against. An
    // unresolvable project means the global layer only (project::applies_to
    // says why that is the safe direction); an unresolvable cwd means that
    // guard can never prove currency for anything, so it never blocks either
    // (see `absent_guard::find_violation`'s own `root: Option<&Path>`).
    let session_cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let session_project = session_cwd.as_deref().and_then(project::resolve_project);

    let mut store = EventStore::new(db_path).ok()?;

    match event_name.as_str() {
        "SessionStart" => {
            // Surface 1. Claude Code's own SessionStart payload carries the
            // session's working directory under "cwd"; resolve "the current
            // project" from it through the one place that does this
            // (project::resolve_project - a marker file wins, else the git
            // root's basename, else global-only). A payload with no "cwd" at
            // all (never observed from Claude Code itself, but this channel
            // must degrade rather than assume) falls back to this process'
            // own working directory.
            //
            // No subagent gate here: confirmed 2026-08-05 against Claude
            // Code's own hooks documentation that `SessionStart` fires ONLY
            // in the main session and never inside a subagent at all, so a
            // subagent check on this arm can never execute - it was removed
            // as dead code that looked like protection (see
            // `INJECTION-FRAMING.md`'s own addendum and
            // `payload_is_from_a_subagent`'s doc comment, which now gates the
            // three surfaces that actually DO fire inside a subagent).
            // Where the log stands right now, so the crowding debt at Stop can
            // tell what THIS session wrote from what was already there. It
            // cannot use the session id for that: every write through the tool
            // server is stamped with a constant, never the caller's own
            // session - see `crowding_debt`.
            record_session_watermark(db_path, &session_id);

            let candidates = serve::live::always_candidates(&store);
            let decay = DecayContext::load(&store);
            let items = serve::decay::retain_live(
                session_start::select(&candidates, session_project.as_deref()),
                &decay,
            );
            let block = session_start::render(&items)?;
            let ids: Vec<String> = items.iter().map(|r| r.id.clone()).collect();
            deliver::record_delivery(&mut store, &session_id, &session_id, "hook", &time::now_iso8601(), &ids);
            match decay_notice(&store, db_path, session_cwd.as_deref()) {
                Some(notice) => Some(HookOutput::ContextWithNotice { event_name, block, notice }),
                None => Some(HookOutput::Context { event_name, block }),
            }
        }
        "UserPromptSubmit" => {
            // Surface 3. Claude Code's UserPromptSubmit payload carries the
            // raw text under a top-level "prompt" field (never under
            // tool_input, which is a PreToolUse-only shape).
            let prompt_text = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            if prompt_text.trim().is_empty() {
                return None;
            }

            // Lane C, C1 (SPEC-ENFORCEMENT.md 2.1): flag a durable decision
            // for capture at Stop, independent of whether anything below
            // resolves for injection - a decision can be stated even when no
            // existing item matches this prompt's own keywords. See
            // `capture_flag`'s own doc comment; every failure here is silent.
            // NEVER for a subagent (`payload_is_from_a_subagent`'s own doc
            // comment) - a subagent's own task prompt is not the owner
            // stating anything.
            if !payload_is_from_a_subagent(&payload) {
                capture_flag(&store, db_path, &session_id, prompt_text);
            }

            let candidates = serve::live::live_items(&store);
            let mut input = prompt::resolve(prompt_text, &candidates);
            input.project = session_project.clone();
            if input.is_empty() {
                return None;
            }
            let decay = DecayContext::load(&store);
            let all = serve::decay::retain_live(serve::rank::select(&candidates, &input), &decay);
            let selection = render::cap(all);
            let block = render::render_text(&selection, &input.moments)?;
            let ids: Vec<String> = selection.shown.iter().map(|r| r.id.clone()).collect();
            deliver::record_delivery(&mut store, &session_id, &session_id, "hook", &time::now_iso8601(), &ids);
            Some(HookOutput::Context { event_name, block })
        }
        _ => {
            // Surface 2 (PreToolUse, and the safe default for any event name
            // this hook does not otherwise recognise).
            let tool_input = payload.get("tool_input");
            let file_path = tool_input.and_then(|t| t.get("file_path")).and_then(|v| v.as_str());
            let tool_name = payload.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");

            // Lane C, C3 (SPEC-ENFORCEMENT.md 2.3): the wrong-sink check,
            // BEFORE anything else in this arm. Deliberately narrow - see
            // `capture_sink_check` / `capture::sink_verdict`'s own doc
            // comments. NEVER for a subagent (`payload_is_from_a_subagent`'s
            // own doc comment). A `Block` (an ACTUAL judge verdict, still
            // unpaid) stops the tool call outright; a `Warn` (C1's own
            // cheap, provisional rulebook match - possibly all that exists
            // yet, before this turn's Stop has run) never blocks, only rides
            // along as visible context below.
            let sink_verdict = if payload_is_from_a_subagent(&payload) {
                capture::SinkVerdict::Allow
            } else {
                capture_sink_check(&store, db_path, &session_id, file_path)
            };
            if let capture::SinkVerdict::Block(reason) = sink_verdict {
                return Some(HookOutput::Decision(serde_json::json!({
                    "decision": "block",
                    "reason": reason,
                })));
            }
            let sink_warning = match sink_verdict {
                capture::SinkVerdict::Warn(reason) => Some(reason),
                _ => None,
            };

            // The Absent-check guard (see `serve::absent_guard`'s own module
            // doc comment for the full doctrine) - independent of Lane C
            // above: it fires on the CONTENT about to be written, never on a
            // stated decision, so unlike C3 it is never gated on
            // `payload_is_from_a_subagent` - it has no notion of "the
            // owner's own debt" to gate on in the first place, and a
            // subagent's write can violate an anchored, still-current rule
            // exactly as easily as the owner's own.
            if let Some(reason) = absent_guard_block(
                &store,
                db_path,
                &session_id,
                tool_name,
                file_path,
                tool_input,
                session_project.as_deref(),
                session_cwd.as_deref(),
            ) {
                return Some(HookOutput::Decision(serde_json::json!({
                    "decision": "block",
                    "reason": reason,
                })));
            }

            // The command guard - the Absent-check guard's own THIRD anchor
            // shape (see `serve::absent_guard`'s own module doc comment,
            // "THE COMMAND guard"): a live Rule/Orientation bound to a
            // `Command` target blocks a Bash-style call whose own command
            // string carries a forbidden literal. Tried AFTER the file-based
            // guard above, never before it: in every real Claude Code
            // payload the two guards' own domains never overlap (a
            // Write/Edit carries a file_path and no command, a Bash call
            // carries a command and no file_path), so ordering has no
            // observable effect in the cases that actually occur; where it
            // theoretically could (a hand-built tool_input carrying both
            // fields at once), this keeps the pre-existing file-based
            // guard's own precedence completely unchanged rather than
            // letting a brand new prohibition preempt an already-shipped
            // one. Same subagent stance as `absent_guard_block` just above,
            // for the identical reason: this fires on the COMMAND about to
            // run, never on a stated decision, so it is never gated on
            // `payload_is_from_a_subagent` either.
            if let Some(reason) = command_guard_block(&store, db_path, &session_id, tool_input, session_cwd.as_deref())
            {
                return Some(HookOutput::Decision(serde_json::json!({
                    "decision": "block",
                    "reason": reason,
                })));
            }

            let mut input = ServeInput { project: session_project.clone(), ..Default::default() };
            if let Some(command) = absent_guard::proposed_command(tool_input) {
                input.add_command(command);
            }
            if let Some(file_path) = file_path {
                input.add_file(file_path);
            }
            // The Remember moment (`intent::Action::Remember`): nothing
            // derives it from a command or file path (see `intent`'s own
            // closed vocabulary doc comment), so this is the one place a
            // call to this memory's own remember/revise tool ever turns into
            // that moment. See `is_remember_moment`'s own doc comment for why
            // only the tool-name SUFFIX is compared, never the whole name.
            if is_remember_moment(tool_name) {
                input.add_moment(Action::Remember);
            }
            if input.is_empty() {
                // Nothing else would render, but a provisional sink warning
                // may still stand entirely on its own.
                return sink_warning.map(|block| HookOutput::Context { event_name, block });
            }
            let served = serve::serve(&store, &input);
            let rendered = render::render_text(&served.selection, &input.moments);
            let block = match (sink_warning, rendered) {
                (Some(warning), Some(rendered)) => Some(format!("{warning}\n\n{rendered}")),
                (Some(warning), None) => Some(warning),
                (None, Some(rendered)) => Some(rendered),
                (None, None) => None,
            };
            let block = block?;
            let ids: Vec<String> = served.selection.shown.iter().map(|r| r.id.clone()).collect();
            deliver::record_delivery(&mut store, &session_id, &session_id, "hook", &time::now_iso8601(), &ids);
            Some(HookOutput::Context { event_name, block })
        }
    }
}

// ------------------------------------------------------- Lane C: the capture
// guard (SPEC-ENFORCEMENT.md section 2). The wiring here is deliberately
// thin: every real decision lives in `serve::capture`'s pure functions; these
// three do only the I/O (read the rulebook/marker file, read the store,
// write the marker file back) and translate the result into `hook_once`'s
// own `HookOutput` shape.

/// C1 (SPEC-ENFORCEMENT.md 2.1, redesigned 2026-08-05 - see
/// `JUDGE-TRANSPORT.md` and `capture`'s own doc comment): cheap about
/// BLOCKING, deliberately - no judge call happens here, ever (that would pay
/// its multi-second latency in front of every single prompt, before the
/// model has even started answering; see `judge`'s own doc comment). Records
/// the raw prompt text and the store's current max event seq for THIS
/// session, unconditionally, on every non-empty prompt. It DOES also read
/// the fallback rulebook and pass it through - `flag_marker_text` stores that
/// match as a PROVISIONAL signal only (restored 2026-08-05 so C3 has
/// something to warn on within the same turn); it can never reach the
/// AUTHORITATIVE `tier` field, which stays `None` here regardless and is
/// Stop's alone to fill in, via the judge (`capture_stop_check`). Every
/// failure here - store unreadable, rulebook unreadable, marker file
/// unreadable/unwritable - is silent: this must never slow down or fail a
/// prompt (SPEC-ENFORCEMENT.md 1.1). NEVER called for a subagent - see the
/// `UserPromptSubmit` arm above and `payload_is_from_a_subagent`'s own doc
/// comment.
fn capture_flag(store: &thor_core::event_store::EventStore, db_path: &Path, session_id: &str, prompt_text: &str) {
    // seq_at_flag = the store's current max event seq: `get_next_seq` is
    // "the next seq a write would take", i.e. max + 1, so max is that minus
    // one (0 on a genuinely empty store - `saturating_sub` so this can never
    // wrap negative).
    let Ok(next_seq) = store.get_next_seq() else { return };
    let seq_at_flag = next_seq.saturating_sub(1);
    let marker_path = capture::default_marker_path(db_path);
    let existing = std::fs::read_to_string(&marker_path).ok();
    // The NARROW trigger set is the compiled-in default the moment no real
    // `guard-capture-rulebook.json` exists yet - see
    // `capture::resolve_rulebook_text`'s own doc comment for why this matters
    // most for self-adjudicate mode (its entire trigger signal IS this
    // rulebook firing at all).
    let rulebook_text =
        capture::resolve_rulebook_text(std::fs::read_to_string(capture::default_rulebook_path(db_path)).ok());
    let Some(new_text) =
        capture::flag_marker_text(existing.as_deref(), session_id, prompt_text, seq_at_flag, Some(&rulebook_text))
    else {
        return;
    };
    let _ = std::fs::write(&marker_path, new_text);
}

/// C2 (SPEC-ENFORCEMENT.md 2.2): the one Stop path allowed to open the
/// store, and only read-only - `EventStore::open_existing` never creates a
/// store (unlike `EventStore::new`), which matches that claim literally: by
/// the time a marker exists at all, C1 has already run in this same session
/// and the store already exists. Every failure - no marker, marker file
/// unreadable/malformed, store missing/corrupt/unreadable - falls through to
/// `None` (ALLOW), the same fail-open as the Response Guard right above this
/// call in `hook_once`. This is also SPEC-ENFORCEMENT.md's own safety-model
/// test 9 in wiring form: a debt whose payment cannot be proven (the store
/// will not open) never blocks.
///
/// The classification itself (judge, or its fallback) is deliberately
/// skipped whenever the debt is ALREADY paid - `capture::decide_stop` would
/// discard it anyway (a paid debt always wins over any verdict), so checking
/// `paid` first saves a real judge round trip on every turn where the model
/// already did the right thing.
fn capture_stop_check(db_path: &Path, session_id: &str) -> Option<HookOutput> {
    // `Off` is checked before anything else here is even read - the cheapest
    // possible "never blocks anything" (OPTION4-IMPLEMENTATION.md): no marker
    // file, no store, nothing.
    let mode = load_capture_mode(db_path);
    if mode == capture::CaptureMode::Off {
        return None;
    }

    let marker_path = capture::default_marker_path(db_path);
    let marker_text = std::fs::read_to_string(&marker_path).ok()?;
    let mut markers = capture::read_markers(&marker_text);
    let marker = markers.get(session_id)?.clone();

    let store = thor_core::event_store::EventStore::open_existing(db_path).ok()?;
    let kinds: Vec<_> = store.events_since(marker.seq_at_flag).ok()?.into_iter().map(|e| e.kind).collect();
    let paid = capture::debt_paid(&kinds);

    // Mode decides who judges: SelfAdjudicate never reads the judge config
    // and never spawns anything (`capture::decide_self_adjudicate` is pure);
    // Judge is the pre-existing path, unchanged. `Off` was already handled
    // above, so it can never reach this match.
    let (verdict, next) = match mode {
        capture::CaptureMode::Off => unreachable!("returned above"),
        capture::CaptureMode::SelfAdjudicate => capture::decide_self_adjudicate(&marker, paid),
        capture::CaptureMode::Judge => {
            let classification = if paid { None } else { classify_capture(db_path, &marker.prompt) };
            capture::decide_stop(&marker, paid, classification.as_ref())
        }
    };

    // Persist whatever the decision above decided: clear the marker (debt
    // paid, warn tier, no verdict, or already blocked once) or keep it with
    // blocked_once now true. Best-effort - a failed write here must never
    // stop the verdict already decided from being returned.
    match &next {
        Some(updated) => {
            markers.insert(session_id.to_string(), updated.clone());
        }
        None => {
            markers.remove(session_id);
        }
    }
    if let Ok(text) = serde_json::to_string(&markers) {
        let _ = std::fs::write(&marker_path, text);
    }

    match verdict {
        // Nothing else had anything to say, so this is where the one debt
        // that never gets paid voluntarily is collected.
        capture::StopVerdict::Allow => None,
        capture::StopVerdict::Block(reason) => Some(HookOutput::Decision(serde_json::json!({
            "decision": "block",
            "reason": reason,
        }))),
    }
}

/// Where this session's watermark lives: the store's tip as it stood when the
/// session began, keyed by session id, in a sidecar beside the store - the
/// same shape and the same place as every other per-session marker this
/// binary keeps.
fn watermark_path(db: &Path) -> std::path::PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("session-watermark.json")
}

/// Record where the log stood when this session started, once. Called from the
/// SessionStart arm; best effort, like every sidecar here - a watermark that
/// cannot be written means the crowding debt stays quiet, never that a turn
/// breaks.
fn record_session_watermark(db_path: &Path, session_id: &str) {
    let Ok(store) = EventStore::open_existing(db_path) else { return };
    let Ok((tip, _)) = store.contiguous_tip() else { return };
    let path = watermark_path(db_path);
    let mut marks: std::collections::BTreeMap<String, i64> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // Never move a watermark that already exists: a session start that fires
    // twice (a resume, a compact) must not forgive what was written between.
    marks.entry(session_id.to_string()).or_insert(tip);
    if let Ok(text) = serde_json::to_string(&marks) {
        let _ = std::fs::write(&path, text);
    }
}

/// Where the log stood when this session started, or `None` when no watermark
/// was ever recorded for it. `None` means silence, deliberately: a session
/// whose start was never seen (an older install, a hook that failed to write)
/// must not be handed every crowded fact in the store as though it made them.
fn session_watermark(db_path: &Path, session_id: &str) -> Option<i64> {
    let text = std::fs::read_to_string(watermark_path(db_path)).ok()?;
    let marks: std::collections::BTreeMap<String, i64> = serde_json::from_str(&text).ok()?;
    marks.get(session_id).copied()
}

/// The crowded facts THIS session wrote and has not dealt with.
///
/// WHY THIS IS FORCED AND THE EVALUATION IS NOT. The write response already
/// says "this may well never be shown there" the moment somebody stores a fact
/// onto a full pool. Saying it was not enough: measured across two real
/// sessions, the note was reported and then left alone, and the fact stayed
/// invisible. A maintenance step that depends on somebody remembering is the
/// same shape as the useful-mark was before it was fixed - it works exactly as
/// often as people feel like it.
///
/// So this holds the turn, like the judgement debt does, and for the same
/// reason: it is the only thing that ever produced maintenance. It is asked
/// BEFORE that older debt because this answer decays - the person who wrote
/// the fact still knows what it was for, and tomorrow nobody does.
///
/// WHAT "THIS SESSION" MEANS HERE, AND WHY IT IS NOT THE SESSION ID. A first
/// version filtered events on `session_id` and was UNREACHABLE in real use:
/// every write through the tool server is stamped with the constant "mcp"
/// (`mcp::SESSION_ID`), never the caller's own session, so the Stop hook -
/// which does know the real one - matched nothing and the debt never fired.
/// Its unit tests passed because they handed the same id to both sides. Found
/// by a real session running the test end to end and reporting that step 2
/// simply did not block.
///
/// So the boundary is a WATERMARK: the store's tip as it stood when this
/// session started (`session_watermark`). Anything written above it was
/// written during this session, whoever stamped it.
///
/// Beyond that watermark it holds no state. `capacity` is a pure function of
/// the store as it stands now, so the debt clears by construction the moment
/// the fact is folded away, re-anchored somewhere with room, or retracted.
///
/// It only ever asks about what THIS session made. A session that stores
/// nothing never sees it, and it never becomes a backlog nag about older mess.
/// THE THING THE OWNER ASKED FOR THREE DAYS RUNNING, and did not have until
/// 2026-08-09: the system finding, by itself, which facts deserve to reach the
/// gate.
///
/// The write gate asks its question of every heavy rule at the door. That
/// covers what is written from now on, and it covered the backlog exactly once
/// - in a sweep somebody had to decide to run. Everything else in the store
/// stayed as it was, and nothing was ever going to look at it again. "Ask when
/// somebody happens to touch it" is not a mechanism; it is a hope.
///
/// So this walks the whole store, every turn, and holds the turn on ONE rule
/// that names something concrete in its own text and has never been asked. It
/// does not decide the answer - that needs judgement, and the answer "there is
/// nothing to catch here" is a real answer. It decides WHO GETS ASKED, which
/// was the part that depended on somebody remembering.
///
/// Deliberately last among the debts and deliberately one at a time: this is a
/// backlog with hundreds in it, and a burn that never stops beats a sweep that
/// happens once.
/// Where the backlog burn remembers whose turn it already took.
fn teeth_asked_path(db: &Path) -> std::path::PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("teeth-asked.json")
}

/// ONCE per session, never once per turn.
///
/// THE DEFECT THIS PREVENTS, caught the same evening it was built: with 348
/// unanswered rules in the store, a per-turn debt is not a slow burn, it is a
/// wall - every single turn ends held, forever, and the only way to work is to
/// stop believing the debts. A maintenance nudge that makes the tool unusable
/// gets switched off, and then it protects nothing at all.
fn teeth_not_yet_asked_this_session(db: &Path, session_id: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(teeth_asked_path(db)) else { return true };
    !text.lines().any(|line| line.trim() == session_id)
}

/// Best effort, like every sidecar here: a sidecar that cannot be written
/// means the burn asks again next turn, never that a turn breaks.
fn record_teeth_asked(db: &Path, session_id: &str) {
    let path = teeth_asked_path(db);
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    text.push_str(session_id);
    text.push('\n');
    // Keep the tail only: this file is a set of recent sessions, not history.
    let kept: Vec<&str> = text.lines().rev().take(200).collect();
    let trimmed: String = kept.into_iter().rev().collect::<Vec<_>>().join("\n");
    let _ = std::fs::write(&path, format!("{trimmed}\n"));
}

/// How many unanswered rules one turn is asked about at once.
///
/// WHY A BATCH, since 2026-08-14. One per session is a burn that never
/// finishes: measured that day at 336 waiting, which is one a day for the
/// better part of a year, while the answer to each is usually one sentence.
/// The judgement debt had exactly this shape and exactly this fix. Five, not
/// all of them: the wall this is deliberately not is a debt that hands over
/// 336 rules and ends every turn until they are gone.
const TEETH_DEBT_BATCH_MAX: usize = 5;

fn teeth_debt(store: &EventStore) -> Option<String> {
    let mut candidates: Vec<(String, String, String)> = serve::live::live_items(store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire())
        .filter(|li| li.item.check.is_none())
        .filter(|li| !li.item.tags.iter().any(|t| model::store::teeth_answer(t).is_some()))
        .filter_map(|li| model::gate::candidate_literal(&li.item.text).map(|lit| (li.id.clone(), li.item.text.clone(), lit)))
        .collect();
    // Lowest id first: a stable order, so the same fact is asked until it is
    // answered rather than a different one every turn.
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    if candidates.is_empty() {
        return None;
    }
    let batch_len = candidates.len().min(TEETH_DEBT_BATCH_MAX);
    let left = candidates.len() - batch_len;
    let mut items_block = String::new();
    for (id, text, literal) in candidates.iter().take(TEETH_DEBT_BATCH_MAX) {
        items_block.push_str(&format!("\n- '{id}' names \"{literal}\": {text}"));
    }
    let left_note = if left > 0 {
        format!(" {left} more are waiting behind this batch.")
    } else {
        String::new()
    };
    Some(format!(
        "[THOR] {batch_len} rule(s) have never been asked whether they can refuse anything, and each one's own text names something a guard could look for - listed below. Answer ALL {batch_len} before ending the turn.{left_note} \
         Is there a text whose presence MEANS the mistake is happening? If YES, give that rule a check with that literal - forbidden for a command or for any file, absent for one named file - and re-anchor it if the check needs a command it does not have. If NO (an authorised action looks identical to an unauthorised one, or the rule is about something forgotten rather than something typed), add '{}<why not>' to its tags with revise - the reason IS the answer, a bare '{}' is refused, and mark is a verdict on where an item fired and cannot tag anything - and it stays exactly as it is. \
         Both answers settle a rule for good; only leaving one unanswered brings it back.{items_block}",
        model::store::NO_LITERAL_REASON_PREFIX,
        model::store::NO_LITERAL_TAG
    ))
}

/// Does this item ACTUALLY reach a block at one of its own bindings, as the
/// real ranker decides it?
///
/// THE DEFECT THIS CLOSES. `model::store::capacity` counts rivals of the same
/// weight or heavier and calls the pool full at `MAX_ITEMS`. That is right for
/// a WRITE-time note: it is a warning, deliberately pessimistic, and the gate
/// itself refuses only when every rival is strictly HEAVIER - see `capacity`'s
/// own doc comment for why equal weight is a note and not a refusal. It is the
/// wrong test for a debt that holds the turn. Equal-weight rivals outrank
/// nothing; closeness and the promotion prior settle those ties at serve time,
/// so an item can sit third of four in the real block while the estimate still
/// calls its pool full.
///
/// Measured 2026-08-13: a fact was folded out of a 24-claimant pool into the
/// shown four, `why` put it third of four, and the debt went on demanding an
/// answer about it every turn. That is the one shape of message this system is
/// least allowed to have - a report that no longer matches what is true. Worse,
/// the only way offered to silence it was the `crowded-on-purpose` tag, so
/// fixing the problem properly led straight to recording a decision that is
/// false.
///
/// So the estimate stays the TRIGGER and the real ranker becomes the JUDGE:
/// only an item that reaches no block at ANY of its bindings still owes an
/// answer.
fn reaches_a_block(store: &EventStore, id: &str, item: &model::item::Item) -> bool {
    use model::item::Binding;
    for binding in &item.bindings {
        // Pinned: `session_start` serves it whole at every session start, so it
        // never competes for a place and can never be crowded out of one.
        if matches!(binding, Binding::Always) {
            return true;
        }
        let mut input = ServeInput { project: item.project.clone(), ..Default::default() };
        match binding {
            Binding::Moment(action) => input.moments.push(action.clone()),
            // A DIRECTORY binding reaches no automatic surface: a file touch
            // offers the path, never its parent, so `rank::select` drops it
            // before comparing anything (see `model::store::capacity`). Feeding
            // it back as a Dir target here would match it against itself and
            // report an item as reachable that a real session can never see.
            Binding::Target { kind: model::item::TargetKind::Dir, .. } => continue,
            Binding::Target { kind, value } => {
                input.targets.push((*kind, value.clone()));
                // The real surface carries the command or path as context and
                // `rank::closeness` reads it. The binding's own value is the
                // closest honest stand-in for the moment this item was written
                // for; leaving it empty would score every candidate alike and
                // measure a ranking nobody ever sees.
                input.context = value.clone();
            }
            Binding::Always => continue,
        }
        if serve::serve(store, &input).selection.shown.iter().any(|r| r.id == id) {
            return true;
        }
    }
    false
}

fn crowding_debt(
    store: &EventStore,
    db_path: &Path,
    session_id: &str,
    current_project: Option<&str>,
) -> Option<String> {
    use thor_core::event_store::EventKind;
    let since = session_watermark(db_path, session_id)?;
    let events = store.get_all_events().ok()?;

    // Written during this session: a declare or a revise above the watermark.
    // Retractions are read too, because retracting IS one of the three ways out.
    let mut written: Vec<String> = Vec::new();
    let mut settled: std::collections::HashSet<String> = Default::default();
    for e in &events {
        if e.seq <= since {
            continue;
        }
        match e.kind {
            EventKind::FactCreated | EventKind::FactRevised => {
                if !written.contains(&e.entity_id) {
                    written.push(e.entity_id.clone());
                }
            }
            EventKind::FactRetracted => {
                settled.insert(e.entity_id.clone());
            }
            _ => {}
        }
    }

    for id in written {
        if settled.contains(&id) {
            continue;
        }
        let Ok(item) = model::store::show(store, &id) else { continue };
        // A fact scoped to a project this session is NOT working in was almost
        // certainly not written by this session. It cannot be told apart by the
        // event's own id - every tool-server write is stamped the constant
        // "mcp" (see this function's watermark note), so a second session
        // running concurrently against the SAME store lands its writes above
        // this session's watermark and they read as ours. The item's own scope
        // is the signal the id is not: a session in repo X gets nagged about a
        // crowded fact in repo Y that another session actually wrote. So the
        // nag is routed by project - this session hears only about facts in its
        // own project, or global ones (no project), which belong to everyone.
        // Measured 2026-08-14: a business-repo fact nagged a THOR-dev session
        // twice. When this session's own project cannot be resolved, nothing is
        // filtered - the watermark stands alone, exactly as before.
        if let (Some(cur), Some(p)) = (current_project, item.project.as_deref()) {
            if cur != p {
                continue;
            }
        }
        // Exit 3 taken: the crowd was judged deserved. See the tag's own doc
        // comment for the day this exit existed in the message but nowhere in
        // the code, and the item kept coming back every turn.
        if item.tags.iter().any(|t| t == model::store::CROWDED_ON_PURPOSE_TAG) {
            continue;
        }
        let Ok(model::store::Capacity::Crowded(note)) = model::store::capacity(store, &item) else {
            continue;
        };
        // The estimate above is the trigger, never the verdict: ask the real
        // ranker whether this item is actually kept out of every block it could
        // reach. See `reaches_a_block` for the day a fact that had just been
        // folded INTO the shown four kept being asked about anyway.
        if reaches_a_block(store, &id, &item) {
            continue;
        }
        return Some(format!(
            "[THOR] '{id}' was stored this session onto a place that is already full, so it will probably never be read there. The memory said so when you wrote it. Deal with it before ending the turn, in this order: (1) FOLD it - if an existing item almost says the same thing, revise that one to carry your point and retract yours; (2) RE-ANCHOR it - if it is really about a narrower file or command than the one it hangs on, move it there with revise; (3) LEAVE IT - and this exit is narrower than it sounds, so read the condition before taking it. It is honest ONLY when the seats are held by rules that can each REFUSE a write: archiving one of those is refused by the gate anyway, and folding it would spend a check. If the seats are held by DESCRIPTIONS instead, this is the wrong answer - archive a description to free its seat, since it stays fully findable and the rule that can actually block takes its place. When the condition really holds: say in your reply which items hold it, and add '{}' to its tags with revise - BARE, exactly as written here: this tag is matched whole, so appending a reason to it is silently ignored and this will keep asking every turn (that is the opposite of 'no-literal:<why>', which requires one). mark is a verdict on where an item fired and cannot tag anything. Recorded that way it is a decision rather than an oversight. Saying it only in prose settles nothing and this will ask again next turn. What you never do is raise its severity to make it visible: that pushes a heavier warning out to show a lighter one. The note said: {note} The item says: {}",
 model::store::CROWDED_ON_PURPOSE_TAG, item.text
        ));
    }
    None
}

/// How many times an item must have fired SINCE its last verdict before the
/// turn is held for another. High on purpose: this is about the handful of
/// rules that are in front of a reader constantly, not about everything
/// served.
///
/// "SINCE its last verdict", not "with no verdict ever", and the difference
/// is the whole point. A lifetime judged-set meant one answer settled an
/// item for good, which is the same defect `decay::is_stale` carried until
/// 2026-08-08: a verdict given once, about an item that has since drifted,
/// outranked a reader who would answer differently today. It also made the
/// cheap answer the damaging one, because the debt asks first about the
/// items that fire most - exactly the ones whose bindings are worth
/// revisiting. Forty more firings is a long way to earn a second question.
const JUDGEMENT_DEBT_AFTER: usize = 40;

/// How many owed items one blocked Stop asks about at once. Unbounded would
/// just move the problem this exists to fix (see `judgement_debt`'s own doc
/// comment) into a single enormous block instead of many small ones; this
/// keeps each block itself bounded while still settling the common case (a
/// handful owed) in one round instead of one Stop per id.
const JUDGEMENT_DEBT_BATCH_MAX: usize = 20;

/// Ask for a verdict on ONE item that has fired over and over and has never
/// once been judged, or nothing.
///
/// WHY THIS ONE IS ALLOWED TO BLOCK WHEN THE CAPTURE GUARD IS NOT. The
/// capture guard has to decide whether a piece of prose stated a durable
/// decision - a judgement, measured on a blind hold-out at 55% catch and
/// 11.4% FALSE blocks, and recorded as not deployable. This decides nothing:
/// the log knows exactly how often an item was served and whether a verdict
/// ever came back. A false block is not merely unlikely here, it is not
/// expressible.
///
/// It terminates by construction. Claude Code sets `stop_hook_active` on the
/// Stop that follows a block, and this whole arm returns early on that, so
/// the ask happens at most once per turn and cannot loop. A verdict buys
/// quiet, not immunity: the count resets, and another `JUDGEMENT_DEBT_AFTER`
/// firings earn another question. This comment used to say a judged item was
/// never asked about again, by anyone, forever - which was true of the
/// lifetime judged-set it once held, and stopped being true the same day the
/// fold below started counting since the last verdict.
///
/// `mark` is the only thing in this system that ever retires noise - a
/// serving count decides nothing, and silence decides nothing either (see
/// `serve::decay`). Without a verdict the loop simply never runs, which is
/// where this store stood for its whole life: four judgements, all on one
/// day, out of 325 items that had fired.
///
/// A PINNED ITEM IS NEVER ASKED ABOUT. The verdict this collects is "did it
/// belong where it fired", and for an `Always` binding the owner answered
/// that himself when he pinned it: it fires at every session start because
/// he chose that. Asking anyway produces a question with no honest answer -
/// "useful" is a lie when the rule simply did not come up, and "noise"
/// overrules his own decision - and a mechanism that asks unanswerable
/// questions teaches people to answer at random. Nineteen of the first
/// forty-two owed were of exactly that kind, found by trying to pay one.
/// What is left fires because a TRIGGER matched, and there the question is
/// real: the trigger can be wrong.
///
/// ASKS ABOUT THE WHOLE OWED SET IN ONE BLOCK, up to
/// `JUDGEMENT_DEBT_BATCH_MAX`, not one item per Stop. Unpinning a batch of
/// long-pinned items (see `serve unpin`) makes all of them owed at once -
/// each carried a large lifetime serving count from its time as `Always`,
/// which crosses `JUDGEMENT_DEBT_AFTER` the moment the pin comes off - and a
/// long conversation pays the FULL conversation history again on every Stop
/// it takes to answer, one id per turn. Measured 2026-08-13: five separate
/// one-item Stops to judge a backlog that batching would have settled in
/// one. The termination property is unchanged: still one block per Stop
/// (`stop_hook_active` still short-circuits the retry), still one verdict
/// call per id, just several ids asked about in that single block instead of
/// a queue paid out one Stop at a time.
fn judgement_debt(store: &EventStore, session_id: &str) -> Option<String> {
    use thor_core::event_store::EventKind;
    let events = store.event_kinds().ok()?;
    // Servings SINCE the last verdict, not servings ever: a judgement resets
    // its item's count to zero rather than removing it from the question
    // forever. Depends on `event_kinds()` yielding the log in order, which is
    // what it does - it is the same fold `usefulness::noise_since_last_useful`
    // performs on the other side of the same doctrine.
    let mut served: std::collections::HashMap<String, usize> = Default::default();
    for (kind, id) in events {
        match kind {
            EventKind::ItemServed => *served.entry(id).or_default() += 1,
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise => {
                served.insert(id, 0);
            }
            _ => {}
        }
    }
    // Deterministic: the most-served unjudged item, ties broken by id, so a
    // session that ignores the ask is asked the same thing next time rather
    // than being walked through a random tour of the backlog.
    let mut owed: Vec<(String, usize)> = served
        .into_iter()
        .filter(|(_, n)| *n >= JUDGEMENT_DEBT_AFTER)
        .collect();
    if owed.is_empty() {
        return None;
    }
    // Only now, and only when something is actually owed, is the whole live
    // set folded to drop the pinned ones - the empty case stays a cheap
    // count over event kinds, which is what every quiet turn pays.
    let live = serve::live::live_items(store);
    let pinned: std::collections::HashSet<&String> = live
        .iter()
        .filter(|li| li.item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always)))
        .map(|li| &li.id)
        .collect();
    owed.retain(|(id, _)| !pinned.contains(id));
    // And only what is still LIVE. The served/judged counts are folded from
    // event kinds, which keep every serving an item ever had, including the
    // ones it had before somebody retracted it. So a retracted item stayed
    // owed forever and could never be settled: the question is "did it belong
    // where it fired", and there is no honest answer for something that is
    // gone. Found the moment it bit, on 2026-08-08: merging 57 duplicates
    // turned all 57 into permanent unanswerable asks, and the very next Stop
    // asked about one of them by name.
    let live_ids: std::collections::HashSet<&String> = live.iter().map(|li| &li.id).collect();
    owed.retain(|(id, _)| live_ids.contains(id));
    if owed.is_empty() {
        return None;
    }
    // And only what THIS session was actually served. A lifetime count says
    // an item fired a hundred times; it says nothing about whether the
    // reader being asked was ever in the room. A rule scoped to another
    // project fires only there, so asking here produces the same
    // unanswerable question the pinned ones did - found on 2026-08-07 by
    // being asked about an acme-shop commit rule during work in thor-2.
    let seen: std::collections::HashSet<String> =
        store.served_ids_in_session(session_id).unwrap_or_default().into_iter().collect();
    owed.retain(|(id, _)| seen.contains(id));
    if owed.is_empty() {
        return None;
    }
    owed.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let total_owed = owed.len();
    let batch_len = total_owed.min(JUDGEMENT_DEBT_BATCH_MAX);
    let mut items_block = String::new();
    for (id, count) in owed.iter().take(JUDGEMENT_DEBT_BATCH_MAX) {
        let text = model::store::show(store, id).ok().map(|i| i.text).unwrap_or_default();
        items_block.push_str(&format!("\n- '{id}' ({count}x since last judged, if ever): {text}"));
    }
    let held_back = total_owed - batch_len;
    let held_back_note = if held_back > 0 {
        format!(" {held_back} more beyond this batch will follow on a later turn.")
    } else {
        String::new()
    };
    Some(format!(
        "[THOR] {batch_len} item(s) have fired repeatedly since they were last judged, if ever - listed below. Judge ALL {batch_len} before ending the turn: call mark once per id, noise:true if it did not belong where it fired, or plain if it helped.{held_back_note} FIRST look at whether each is still TRUE - if the code or the file says otherwise, revise it against what you can see there; that is a repair, not noise, and marking it noise would leave a wrong fact in place. Two noise judgements since the last mark of usefulness retire an item from every injection surface while leaving it findable; a mark of usefulness clears the noise recorded before it, and a noise mark recorded after it still counts. Nothing else in this system ever retires noise - a serving count decides nothing and silence decides nothing.{items_block}"
    ))
}

/// Which capture-guard mode is live right now, for THIS store
/// (OPTION4-IMPLEMENTATION.md): the one place that does the I/O
/// `capture::resolve_capture_mode` itself deliberately stays free of, so that
/// pure decision rule can be unit-tested without touching a filesystem at
/// all. Reads `guard-capture-mode.json` (explicit override, if present and
/// parseable) and whether a real, usable judge config exists
/// (`judge::default_judge_config_path`/`judge::parse_judge_config`) - the
/// signal `resolve_capture_mode` falls back to inferring from when no
/// explicit mode is set.
fn load_capture_mode(db_path: &Path) -> capture::CaptureMode {
    let mode_text = std::fs::read_to_string(capture::default_mode_config_path(db_path)).ok();
    let judge_configured = std::fs::read_to_string(judge::default_judge_config_path(db_path))
        .ok()
        .and_then(|text| judge::parse_judge_config(&text))
        .is_some();
    capture::resolve_capture_mode(mode_text.as_deref(), judge_configured)
}

/// The judge decides; its fallback (the deterministic rulebook, capped to
/// warn-only - `capture::fallback_decision`) never blocks. Every error - no
/// judge config beside the store, an unreadable/malformed config, a command
/// that cannot be spawned, a run that exceeds its configured timeout, or an
/// answer that does not parse into BLOCK/WARN/ALLOW - falls through to the
/// fallback. See `judge`'s own doc comment and `JUDGE-TRANSPORT.md` for the
/// whole fallback ladder. Only ever called from `capture_stop_check` (Stop)
/// - never from `capture_flag` (UserPromptSubmit), which is the whole point
/// of moving classification here (SPEC: "the judge runs at Stop, never at
/// UserPromptSubmit").
fn classify_capture(db_path: &Path, prompt_text: &str) -> Option<capture::FlagDecision> {
    let judge_config_text = std::fs::read_to_string(judge::default_judge_config_path(db_path)).ok();
    if let Some(config) = judge_config_text.as_deref().and_then(judge::parse_judge_config) {
        if let Some(verdict) = judge::run_judge(&config, prompt_text) {
            return capture::from_judge_verdict(verdict);
        }
    }
    let rulebook_text = std::fs::read_to_string(capture::default_rulebook_path(db_path)).ok();
    capture::fallback_decision(rulebook_text.as_deref(), prompt_text)
}

/// C3 (SPEC-ENFORCEMENT.md 2.3, restored 2026-08-05 - see `capture::sink_verdict`'s
/// own doc comment): decide what happens to a write at a mirror sink. Reuses
/// the store `hook_once` already has open for this PreToolUse call (unlike
/// C2, this arm never returns before the store is opened, so there is no
/// separate "read-only, may open its own" concern here). Every failure - no
/// marker, store read failure - falls through to `SinkVerdict::Allow`. NEVER
/// called for a subagent - see the `PreToolUse` arm above and
/// `payload_is_from_a_subagent`'s own doc comment.
fn capture_sink_check(
    store: &thor_core::event_store::EventStore,
    db_path: &Path,
    session_id: &str,
    file_path: Option<&str>,
) -> capture::SinkVerdict {
    // `Off` disables the whole capture guard, C3 included - checked first, no
    // marker file even read (OPTION4-IMPLEMENTATION.md).
    if load_capture_mode(db_path) == capture::CaptureMode::Off {
        return capture::SinkVerdict::Allow;
    }
    let marker_path = capture::default_marker_path(db_path);
    let Ok(marker_text) = std::fs::read_to_string(&marker_path) else { return capture::SinkVerdict::Allow };
    let markers = capture::read_markers(&marker_text);
    let Some(marker) = markers.get(session_id) else { return capture::SinkVerdict::Allow };
    let Ok(events) = store.events_since(marker.seq_at_flag) else { return capture::SinkVerdict::Allow };
    let kinds: Vec<_> = events.into_iter().map(|e| e.kind).collect();
    let paid = capture::debt_paid(&kinds);
    capture::sink_verdict(Some(marker), paid, file_path)
}

/// The Absent-check guard's own I/O wrapper (see `serve::absent_guard`'s
/// module doc comment for the full doctrine): read the once-per-session
/// marker and the store, then decide the LOCATION prohibition before the
/// CONTENT one - LOCATION FIRST, deliberately: it is the broader refusal (a
/// file need not carry any particular text to be refused, only to sit in
/// the wrong place), so when a write would trip both, the reason reported is
/// the one that would have refused it regardless of what it said. Only when
/// location finds nothing does this fall through to the pre-existing content
/// check (`absent_guard::find_violation`), unchanged. Both share the SAME
/// once-per-session-per-file marker - never two - so whichever finds a
/// violation first is also the only one that ever marks the file, and
/// neither fires again on it this session.
///
/// The location candidate pool is fetched separately from the content one:
/// `live::candidates_for` narrows by target KIND only (see its own doc
/// comment), so a `Path`-only fetch (what `ServeInput::add_file` builds)
/// would never even surface a `Dir`-bound item. A `Path` AND a `Dir`
/// placeholder target are added here instead - the VALUE never matters for
/// this narrowing, only the kind, and `absent_guard::find_location_violation`
/// alone decides real containment afterward. This candidate pool
/// deliberately never goes through `rank::select` at all (unlike the content
/// pool below it) - see `serve::absent_guard`'s own module doc comment for
/// why that function's approximate matching is the wrong tool for a location
/// prohibition, and so also carries no project scoping: a rule prohibiting
/// writes to a specific place is either true or not, regardless of which
/// project the current session happens to resolve to - scoping it out by
/// project would silently defeat the exact mistake this guard exists to
/// catch (working in the wrong project's session while still touching a
/// place a DIFFERENT project's rule protects).
///
/// Every failure - no file path, a tool this guard has no notion of, a
/// payload missing the field it needs, an unresolvable root, no matching
/// item, no check, a check of another form/severity/path, a stale anchor, an
/// unreadable marker file - falls through to `None` (ALLOW/nothing), never a
/// block: the same "any error, anywhere, is silence" doctrine every other
/// guard in this file already follows. Mirrors `capture_sink_check`'s own
/// shape (read store/marker, decide, no side effect on the non-block path).
#[allow(clippy::too_many_arguments)]
/// Write down what the gate just DID.
///
/// This is the measurement that did not exist until 2026-08-08. Every other
/// part of 2.0 was counted - items, checks, dead anchors, crowding - and the
/// one capability the whole version was built for, refusing a wrong write,
/// had no number at all. Nobody could say how often the gate fired, and
/// nobody could say how often it had something to say and said nothing,
/// which is how a once-per-session stand-down survived from the first day
/// until four independent reviews read the code out loud.
///
/// Opens its OWN handle: the guard holds a read-only store and a refusal
/// must never wait on, or be lost to, a writable one. Fail-silent from top
/// to bottom, for the same reason every sidecar here is - a log that cannot
/// take a measurement must never cost a refusal.
/// `subject` is the id the outcome is filed under: a rule id for a real
/// prohibition (read back out of the wording with `absent_guard::rule_id_of`
/// by the caller, which is where that knowledge belongs), or the NAME of a
/// guard when what stood aside was the guard itself rather than any one rule.
/// Head-neutral either way, so neither can ever create or move an item.
fn record_gate(db_path: &Path, session_id: &str, refused: bool, subject: &str, target: &str) {
    let rule_id = subject.to_string();
    let Ok(mut store) = thor_core::event_store::EventStore::open_existing(db_path) else { return };
    deliver::record_gate_outcome(
        &mut store,
        session_id,
        session_id,
        "hook",
        &time::now_iso8601(),
        refused,
        &rule_id,
        target,
    );
}

fn absent_guard_block(
    store: &EventStore,
    db_path: &Path,
    session_id: &str,
    tool_name: &str,
    file_path: Option<&str>,
    tool_input: Option<&Value>,
    project: Option<&str>,
    root: Option<&Path>,
) -> Option<String> {
    let file_path = file_path?;
    let content = absent_guard::proposed_content(tool_name, tool_input)?;

    // NOTE THE ABSENCE. The marker used to be read HERE, and an attempt it
    // recognised returned before a single rule was evaluated. It now lives at
    // the bottom of this function, after a verdict exists, where the only
    // thing it can still change is the WORDING of a refusal. Two defects died
    // with that move: a repeat no longer passes unexamined (see
    // `absent_guard::escalated`), and a LOCATION prohibition can no longer be
    // suppressed by a marker set for a content one - it says the path is out
    // of bounds, so there is no such thing as "the same attempt again" for it
    // and every write there is the same violation.
    let mut location_input = ServeInput::default();
    location_input.add_target(TargetKind::Path, file_path);
    location_input.add_target(TargetKind::Dir, file_path);
    let location_candidates = serve::live::candidates_for(store, &location_input);

    let mut input = ServeInput { project: project.map(str::to_string), ..Default::default() };
    input.add_file(file_path);
    let candidates = serve::live::candidates_for(store, &input);
    let ranked = serve::rank::select(&candidates, &input);

    // The staleness half of the doctrine (see `serve::absent_guard`'s own
    // module doc comment): observation only, decided from the SAME two
    // candidate pools already fetched above for the real block decision
    // below, but never influencing it - recorded (best effort, silent on any
    // I/O error) regardless of what `reason` ends up being.
    record_absent_guard_staleness(store, db_path, &location_candidates, &ranked, file_path, root);

    // The third arm is the content check for a rule bound to a DIRECTORY.
    // `rank::select` drops such an item on a KIND mismatch before it ever
    // compares a path, so the ranked pool above can never surface one - it
    // has to come from the same kind-narrowed pool the location check uses,
    // with `absent_guard` deciding real containment. Path-anchored content
    // rules keep their precedence: this only runs when neither arm above
    // found anything.
    // The last arm is the self-contained form: a rule carrying a `Forbidden`
    // check has no anchor to prove, so its BINDING carries its reach, and an
    // Always binding means every write to every file. It runs last because
    // an anchored rule is the more specific statement about this particular
    // file, and the more specific reason is the more useful one to report.
    // It needs no root: there is nothing to resolve.
    let always_candidates = serve::live::always_candidates(store);
    // Location first of the five, and now genuinely first: nothing returns
    // before it any more. It was the arm the old marker placement could
    // silence, which was the wrong way round - it is also the arm with the
    // least excuse for being silenced, because the way out of it is not to
    // satisfy the rule, it is to write somewhere else.
    let reason = absent_guard::find_location_violation(&location_candidates, file_path, root)
        .or_else(|| absent_guard::find_violation(&ranked, file_path, content, root))
        .or_else(|| absent_guard::find_dir_content_violation(&location_candidates, file_path, content, root))
        .or_else(|| absent_guard::find_forbidden_violation(&always_candidates, content))
        // And the mirror image of all four: not a literal that must not
        // appear, but one that must not disappear. A document that mirrors
        // this memory, and a fact about code that names something which has
        // to still be there, are the same case. Runs last because an
        // offending fragment someone can see beats an absence they cannot.
        // Fed the file's content AFTER the call, never the fragment the call
        // carries: the three arms above ask whether a forbidden literal
        // APPEARS, and the fragment is exactly right for that, but this one
        // asks whether a required literal REMAINS, and a replacement fragment
        // almost never contains it. See `absent_guard::content_after_write`
        // for the defect that cost, and for why a content it cannot resolve
        // means this arm simply does not run.
        .or_else(|| {
            absent_guard::content_after_write(tool_name, tool_input, root, file_path)
                .and_then(|after| absent_guard::find_missing_required(&location_candidates, file_path, &after, root))
        })?;

    // Per ATTEMPT, not per file: see `absent_guard::attempt_key`. Keying on
    // the file alone disarmed this guard for the rest of the session after
    // one block, so a second, different, genuinely forbidden write passed
    // unexamined.
    let attempt_text = absent_guard::attempt_text(tool_name, tool_input);
    let attempt = absent_guard::attempt_key(file_path, attempt_text.as_deref());
    let marker_path = absent_guard::default_marker_path(db_path);
    let marker_text = std::fs::read_to_string(&marker_path).ok();
    let markers = marker_text.as_deref().map(absent_guard::read_blocked).unwrap_or_default();
    let repeat = absent_guard::already_blocked(&markers, session_id, &attempt);
    if !repeat {
        if let Some(text) = absent_guard::mark_blocked_text(marker_text.as_deref(), session_id, &attempt) {
            let _ = std::fs::write(&marker_path, text);
        }
    }
    let reason = if repeat { absent_guard::escalated(&reason) } else { reason };
    if let Some(id) = absent_guard::rule_id_of(&reason) {
        record_gate(db_path, session_id, true, id, file_path);
    }
    Some(reason)
}

/// The staleness sidecar's own I/O wrapper (see `absent_guard::StaleRecord`'s
/// own doc comment for the file's shape): read whatever it already held,
/// fold in every finding from THIS call's own content and location scans,
/// write it back - best effort throughout, exactly like every other marker
/// file in this binary (an unreadable or unwritable sidecar is silence,
/// never a panic and never a reason to change what the caller does). Called
/// for its side effect alone, from a point in `absent_guard_block` that runs
/// whether or not a block is about to fire, and its result is never
/// inspected by anything that decides one.
///
/// Also stamps every finding it folds in with `now_seq` - the store's own
/// current max seq, the exact computation `capture_flag` already uses for
/// its unrelated `seq_at_flag` - onto `StaleRecord::seq_at_record` (via
/// `record_stale_text`'s own parameter of the same name). This is what lets
/// `stale_guard_stop_check` later answer "has this item been revised or
/// retracted SINCE it was recorded here". A store that will not even report
/// its own next seq is the one failure this best-effort function does not
/// fold into a wrong seq - it skips recording entirely instead, same as any
/// other failure here.
fn record_absent_guard_staleness(
    store: &EventStore,
    db_path: &Path,
    location_candidates: &[serve::live::LiveItem],
    ranked: &[serve::rank::RankedItem],
    file_path: &str,
    root: Option<&Path>,
) {
    let mut findings = absent_guard::stale_in_location(location_candidates, file_path, root);
    findings.extend(absent_guard::stale_in_content(ranked, file_path, root));
    findings.extend(absent_guard::stale_in_dir_content(location_candidates, file_path, root));
    // Every id this pass looked at, so one that has recovered is dropped
    // instead of being reported for the rest of its life.
    let scanned: Vec<String> =
        location_candidates.iter().map(|li| li.id.clone()).chain(ranked.iter().map(|r| r.id.clone())).collect();
    let Ok(next_seq) = store.get_next_seq() else { return };
    let now_seq = next_seq.saturating_sub(1);
    let stale_path = absent_guard::default_stale_path(db_path);
    let existing = std::fs::read_to_string(&stale_path).ok();
    if let Some(text) = absent_guard::record_stale_text(existing.as_deref(), &findings, &scanned, now_seq) {
        let _ = std::fs::write(&stale_path, text);
    }
}

/// The command guard: the Absent-check guard's THIRD anchor shape (see
/// `serve::absent_guard`'s own top-of-file doc comment, "THE COMMAND
/// guard") - a live Rule/Orientation bound to a `Command` target, carrying a
/// still-current `Check::Absent`/`Check::AbsentAll`, blocks a Bash-style tool
/// call whose own command string carries one of the check's forbidden
/// literals. Wired separately from `absent_guard_block` above, never folded
/// into it: that function's own early return (`file_path?`) already makes it
/// a no-op for every real command call (a Bash payload carries no
/// `file_path`), and this guard's own marker key (the matched item's own
/// anchor - see `absent_guard::find_command_violation`'s own doc comment)
/// is only known AFTER matching, unlike the file guard's, which is known
/// before any matching runs - the two shapes do not fit one function.
///
/// Every failure - no "command" field, an unresolvable root, no matching
/// item, no check, a check of another form, a stale anchor, an unreadable
/// marker file - falls through to `None` (ALLOW/nothing), never a block, the
/// same "any error, anywhere, is silence" doctrine `absent_guard_block`
/// itself already follows. Never emits a permission decision of "allow"
/// either, mirroring every other guard in this file.
fn command_guard_block(
    store: &EventStore,
    db_path: &Path,
    session_id: &str,
    tool_input: Option<&Value>,
    root: Option<&Path>,
) -> Option<String> {
    let command = absent_guard::proposed_command(tool_input)?;

    let mut input = ServeInput::default();
    input.add_target(TargetKind::Command, command);
    // The files and hosts the command NAMES, as doelen of their own. Without
    // these, a fact anchored at a file fired when that file was opened with a
    // file tool and stayed silent when a shell command read the same file -
    // measured 2026-08-07, and it cost a real mistake in a log-grep the very
    // fact warned about. Deliberately only the targets: the moments a command
    // implies are the file guard's business, and adding them here would change
    // what fires far beyond this defect.
    for path in serve::input::paths_in_command(command) {
        input.add_target(TargetKind::Path, &path);
    }
    for host in serve::input::hosts_in_command(command) {
        input.add_target(TargetKind::Host, &host);
    }
    let candidates = serve::live::candidates_for(store, &input);

    // The staleness half of the doctrine, scanned every call over the SAME
    // candidate pool the block decision below uses - never gated on the
    // marker check further down, unlike the file guard's own ordering: that
    // guard can cheaply skip its whole scan once its file is already
    // blocked because it knows its marker key (the file path) up front; this
    // guard only learns its own key (the matched anchor) as a side effect of
    // running the match below, so there is no equivalent early exit to take.
    record_command_guard_staleness(store, db_path, &candidates, command, root);

    let (reason, anchor) = absent_guard::find_command_violation(&candidates, command, root)?;

    // WHAT THIS USED TO DO, AND WHY IT WAS THE WORSE HALF OF A FIXED DEFECT.
    // This arm stood aside on a repeat, keyed on the ANCHOR alone, so ONE
    // refusal disarmed that rule for the whole session no matter what the next
    // command carried. The file arm had the same defect and lost it earlier the
    // same day; this arm kept it, and this is the arm that covers shell work -
    // where the irreversible rules live. Two independent reviews found it the
    // same evening, and both put it first.
    //
    // The old reasoning was about the KEY: a commit message differs on every
    // retry, so keying on the command text would suppress almost nothing. That
    // was true, and it stopped mattering the moment nothing gets suppressed.
    // The key now only decides the WORDING, so it carries the command's own
    // fingerprint alongside the anchor: the same command again is told it is a
    // repeat, a different one tripping the same rule gets its own plain
    // verdict, and neither of them gets through.
    let attempt = absent_guard::attempt_key(&anchor, Some(command));
    let marker_path = absent_guard::default_command_marker_path(db_path);
    let marker_text = std::fs::read_to_string(&marker_path).ok();
    let markers = marker_text.as_deref().map(absent_guard::read_blocked).unwrap_or_default();
    let repeat = absent_guard::already_blocked(&markers, session_id, &attempt);
    if !repeat {
        if let Some(text) = absent_guard::mark_blocked_text(marker_text.as_deref(), session_id, &attempt) {
            let _ = std::fs::write(&marker_path, text);
        }
    }
    let reason = if repeat { absent_guard::escalated(&reason) } else { reason };
    if let Some(id) = absent_guard::rule_id_of(&reason) {
        record_gate(db_path, session_id, true, id, command);
    }
    Some(reason)
}

/// `command_guard_block`'s own staleness I/O wrapper - mirrors
/// `record_absent_guard_staleness` exactly (best effort throughout: an
/// unreadable or unwritable sidecar is silence, never a panic and never a
/// reason to change what the caller does), folding into the SAME
/// `absent-guard-stale.json` sidecar that function already writes
/// (`StaleRecord`/`StaleFinding` are keyed by item id, not by "file" vs
/// "command" - see `absent_guard::stale_in_command`'s own doc comment).
fn record_command_guard_staleness(
    store: &EventStore,
    db_path: &Path,
    command_candidates: &[serve::live::LiveItem],
    command: &str,
    root: Option<&Path>,
) {
    let findings = absent_guard::stale_in_command(command_candidates, command, root);
    let scanned: Vec<String> = command_candidates.iter().map(|li| li.id.clone()).collect();
    let Ok(next_seq) = store.get_next_seq() else { return };
    let now_seq = next_seq.saturating_sub(1);
    let stale_path = absent_guard::default_stale_path(db_path);
    let existing = std::fs::read_to_string(&stale_path).ok();
    if let Some(text) = absent_guard::record_stale_text(existing.as_deref(), &findings, &scanned, now_seq) {
        let _ = std::fs::write(&stale_path, text);
    }
}

/// The stale-rule guard's own I/O wrapper (see `serve::stale_guard`'s module
/// doc comment for the full doctrine): the THIRD Stop-time check, reached
/// only when neither the Response Guard nor Lane C's capture guard (both
/// above, in `hook_once`'s own Stop arm) had anything to say.
///
/// `Off` (`stale_guard::resolve_mode`) is checked first, the cheapest
/// possible "never blocks anything" - the same shape `capture_stop_check`'s
/// own `Off` check already takes: no marker file, no sidecar, no store,
/// nothing read.
///
/// At most once per session (`stale_guard::already_blocked`/
/// `mark_blocked_text` - its OWN marker file, never `absent_guard`'s, which
/// keys on (session, file) for an unrelated prohibition): once this session
/// has already been told, it is not told again until the NEXT session.
///
/// Settlement is decided per item: for every entry in the stale sidecar,
/// this reads that ONE item's own history (`EventStore::get_events_by_entity`
/// - the same call `model::store::show`/`history` already use), filters it
/// to events after that entry's own `seq_at_record`, and asks
/// `stale_guard::item_settled` (built on the same kind-membership approach
/// `capture::debt_paid` already uses for its own, unrelated debt).
///
/// Every failure - no stale sidecar (nothing has ever gone stale), an
/// unreadable/malformed sidecar or marker, a store that will not open, a
/// per-item history lookup that fails - falls through to `None` (ALLOW),
/// fail-open exactly like every other guard in this file: an entry whose
/// settlement cannot be proven either way is never the reason a turn cannot
/// end silently. A single failed per-item lookup aborts the WHOLE check
/// (via `?`, propagating `None` immediately) rather than guessing for just
/// that one entry - the simplest rule that still never blocks on a doubt.
fn stale_guard_stop_check(db_path: &Path, session_id: &str) -> Option<HookOutput> {
    let mode_text = std::fs::read_to_string(stale_guard::default_mode_config_path(db_path)).ok();
    if stale_guard::resolve_mode(mode_text.as_deref()) == stale_guard::Mode::Off {
        return None;
    }

    let marker_path = stale_guard::default_marker_path(db_path);
    let marker_text = std::fs::read_to_string(&marker_path).ok();
    let blocked = marker_text.as_deref().map(stale_guard::read_blocked).unwrap_or_default();
    // The marker is READ here and acted on at the bottom, after the debt is
    // known. It used to return right here, recording a stand-aside before
    // anything had established there was something to withhold - so every
    // Stop after the first nudge logged one, including turns with an empty
    // sidecar and nothing to say, plus a store write per turn. Both reviews
    // caught it: a number that measures silence must not count silence about
    // nothing.
    let already_nudged = stale_guard::already_blocked(&blocked, session_id);

    let stale_text = std::fs::read_to_string(absent_guard::default_stale_path(db_path)).ok()?;
    let stale = absent_guard::read_stale(&stale_text);
    if stale.is_empty() {
        return None;
    }

    let store = thor_core::event_store::EventStore::open_existing(db_path).ok()?;
    let mut settled_ids = std::collections::HashSet::new();
    for (id, record) in &stale {
        let events = store.get_events_by_entity(id).ok()?;
        let kinds: Vec<_> =
            events.into_iter().filter(|e| e.seq > record.seq_at_record).map(|e| e.kind).collect();
        if stale_guard::item_settled(&kinds) {
            settled_ids.insert(id.clone());
        }
    }

    let remaining = stale_guard::outstanding(&stale, &settled_ids);
    if remaining.is_empty() {
        return None;
    }

    // NOW it is true: there is outstanding rot, and this session has already
    // been told once. This is the one place left in the binary that has
    // something to say and chooses not to say it, which is correct for a
    // maintenance nudge - one that blocked every turn would be routed around
    // within a day - and it is the exact shape that hid a broken gate for the
    // whole of 2.0, so it is counted. Filed under the guard's own name rather
    // than a rule id, because what stands aside is the guard, not one rule.
    if already_nudged {
        record_gate(db_path, session_id, false, "stale-guard", "stop");
        return None;
    }

    let reason = stale_guard::block_message(&remaining);
    if let Some(text) = stale_guard::mark_blocked_text(marker_text.as_deref(), session_id) {
        let _ = std::fs::write(&marker_path, text);
    }

    Some(HookOutput::Decision(serde_json::json!({
        "decision": "block",
        "reason": reason,
    })))
}

// ---------------------------------------------------- the Remember moment

/// This memory's own "write a fact" tool names - mirror `mcp::Mcp::remember`
/// and `mcp::Mcp::revise` (`mcp/src/lib.rs`), the two `#[tool]` methods that
/// call `model::store::declare`/`model::store::revise`. Named constants,
/// never inline literals, because the crate dependency between the two only
/// runs ONE way (mcp depends on serve, never the reverse - `mcp/Cargo.toml`),
/// so the compiler has no way to catch either string drifting out of sync
/// with the method it names; a rename of either `#[tool]` fn in
/// `mcp/src/lib.rs` must be echoed here by hand.
const MCP_REMEMBER_TOOL: &str = "remember"; // mirrors mcp::Mcp::remember
const MCP_REVISE_TOOL: &str = "revise"; // mirrors mcp::Mcp::revise

/// The tool-name half of a namespaced MCP tool call. Claude Code calls an MCP
/// tool as `mcp__<server-name>__<tool-name>` - the server-name half is
/// whatever the user's own Claude Code config happens to call the server
/// (already renamed once in this project's own history), so matching the
/// FULL string would silently stop working the next time it is renamed
/// again, and this binary has no way to keep a literal server name in sync
/// with a config file it never reads. Only the part after the LAST double
/// underscore is compared. A tool name with no double underscore at all (an
/// ordinary built-in tool - "Write", "Bash", ...) has nothing to strip, so
/// the whole name comes back unchanged; it will simply never equal
/// `MCP_REMEMBER_TOOL`/`MCP_REVISE_TOOL`.
fn mcp_tool_suffix(tool_name: &str) -> &str {
    tool_name.rsplit_once("__").map_or(tool_name, |(_, suffix)| suffix)
}

/// Whether this tool call is this memory's own `remember` or `revise` - the
/// moment of WRITING or CORRECTING a fact. Nothing else derives
/// `intent::Action::Remember` (see `intent`'s own closed vocabulary doc
/// comment - `from_command`/`from_path`/`from_draft` none of them produce
/// it), so this is the ONLY place that moment can ever fire from.
fn is_remember_moment(tool_name: &str) -> bool {
    let suffix = mcp_tool_suffix(tool_name);
    suffix == MCP_REMEMBER_TOOL || suffix == MCP_REVISE_TOOL
}

#[cfg(test)]
mod decay_notice_tests {
    use super::*;
    use model::item::{Binding, Check, Item, Kind, TargetKind};

    /// A checkout the way `project::resolve_project` recognises one, holding
    /// one file, plus a store on disk beside it.
    fn checkout(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("guarded.rs"), "fn kept() {}\n").unwrap();
        (dir, root)
    }

    fn rule_with_check(store: &mut EventStore, id: &str, project: &str, literal: &str) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("guarded.rs still says {literal}"),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "guarded.rs".to_string() }],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("the file stops saying it".to_string()),
            check: Some(Check::Contains { path: "guarded.rs".to_string(), literal: literal.to_string() }),
        };
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
    }

    /// THE DEFECT THIS CLOSES, and it is a mechanism defect rather than a
    /// bug. This scan is the only thing that looks at EVERY fact of a
    /// project, including ones whose file nobody touched all session. It used
    /// to print a count and stop there, while the two Stop-time guards that
    /// actually make somebody act only ever saw files written during the
    /// turn. So the thing that could find rot could not ask, and the things
    /// that ask could not see it - which is why a session only worked on
    /// staleness when the owner raised it himself. Recording into the same
    /// sidecar `serve::stale_guard` already reads is what turns the finding
    /// into a question.
    #[test]
    fn a_proof_that_comes_out_false_is_handed_to_the_stop_guard() {
        let (dir, root) = checkout("Some-Project");
        let db = dir.path().join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        rule_with_check(&mut store, "gone-literal", "Some-Project", "fn vanished()");

        let notice = decay_notice(&store, &db, Some(&root));
        assert!(notice.is_some(), "a false proof must still produce the owner's line");

        let sidecar = serve::absent_guard::default_stale_path(&db);
        let text = std::fs::read_to_string(&sidecar).expect("the finding must be recorded, not just counted");
        assert!(text.contains("gone-literal"), "the item has to be nameable at Stop: {text}");
    }

    /// The other half: a proof that HOLDS says nothing at all. A scan that
    /// recorded every fact it looked at would hand the Stop guard a backlog
    /// of non-problems, and a guard that cries wolf gets bypassed.
    #[test]
    fn a_proof_that_holds_is_never_recorded() {
        let (dir, root) = checkout("Some-Project");
        let db = dir.path().join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        rule_with_check(&mut store, "still-true", "Some-Project", "fn kept()");

        assert!(decay_notice(&store, &db, Some(&root)).is_none(), "nothing is wrong, so nothing is said");
        assert!(
            !serve::absent_guard::default_stale_path(&db).exists(),
            "a healthy store must not grow a staleness sidecar out of nowhere"
        );
    }
}

#[cfg(test)]
mod judgement_debt_tests {
    use super::*;
    use model::item::{Binding, Item, Kind};

    /// A crowded pool to write into: MAX_ITEMS items of equal weight already
    /// claiming one moment, so the next arrival gets the note rather than a
    /// refusal (a refusal needs HEAVIER rivals - see `model::store::capacity`).
    fn crowd_a_moment(store: &mut EventStore, project: &str) {
        const DISTINCT: [&str; 5] = [
            "a webhook retry backs off before it gives up entirely",
            "the estimator rounds a quote up to whole cents",
            "a spool label carries the batch it came from",
            "the scheduler skips a printer that is on hold",
            "an invoice number never restarts inside a year",
        ];
        for i in 0..model::item::MAX_ITEMS {
            let item = Item {
                id: format!("holder-{i}"),
                kind: Kind::Rule,
                text: DISTINCT[i % DISTINCT.len()].to_string(),
                bindings: vec![Binding::Moment(intent::Action::Deploy)],
                severity: None,
                project: Some(project.to_string()),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("holder {i} turns out not to matter")),
                check: None,
            };
            model::store::declare(store, "earlier", "earlier", "t", &item).expect("fixture must store");
        }
    }

    fn crowded_newcomer(id: &str, project: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "a shipment label is printed once and never reprinted silently".to_string(),
            bindings: vec![Binding::Moment(intent::Action::Deploy)],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("a label is reprinted without anyone noticing".to_string()),
            check: None,
        }
    }

    /// A crowded PATH pool: `MAX_ITEMS` items of equal weight claiming one
    /// file, whose texts share no words with the path, so closeness separates
    /// none of them from each other.
    fn crowd_a_path(store: &mut EventStore, project: &str) {
        const DISTINCT: [&str; 5] = [
            "a webhook retry backs off before it gives up entirely",
            "the estimator rounds a quote up to whole cents",
            "a spool label carries the batch it came from",
            "the scheduler skips a printer that is on hold",
            "an invoice number never restarts inside a year",
        ];
        for i in 0..model::item::MAX_ITEMS {
            let item = Item {
                id: format!("pathholder-{i}"),
                kind: Kind::Rule,
                text: DISTINCT[i % DISTINCT.len()].to_string(),
                bindings: vec![Binding::Target {
                    kind: model::item::TargetKind::Path,
                    value: "server/app.js".to_string(),
                }],
                severity: None,
                project: Some(project.to_string()),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("path holder {i} turns out not to matter")),
                check: None,
            };
            model::store::declare(store, "earlier", "earlier", "t", &item).expect("fixture must store");
        }
    }

    fn path_newcomer(id: &str, project: &str, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: text.to_string(),
            bindings: vec![Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "server/app.js".to_string(),
            }],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this newcomer turns out not to matter".to_string()),
            check: None,
        }
    }

    /// THE DEFECT THIS PREVENTS. `capacity` counts rivals of the same weight or
    /// heavier and calls the pool full - correct as a deliberately pessimistic
    /// WRITE-time warning, wrong as the debt's verdict. Equal weight outranks
    /// nothing; closeness settles those ties at serve time. Measured 2026-08-13
    /// on a fact that had just been folded INTO the shown four and was still
    /// asked about every turn, with the `crowded-on-purpose` tag as the only
    /// offered exit - so answering it honestly meant recording a decision that
    /// was false.
    #[test]
    fn a_crowded_estimate_settles_itself_when_the_item_really_reaches_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_path(&mut store, "p");
        record_session_watermark(&db, "now");
        // Same weight and same pool, so the estimate still calls it full - but
        // its text shares words with the path, which is what wins the tie.
        let mine = path_newcomer("mine", "p", "the server app entry point stays free of route handlers");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        assert!(
            matches!(model::store::capacity(&store, &mine), Ok(model::store::Capacity::Crowded(_))),
            "fixture sanity: the write-time estimate must still call this pool full"
        );
        assert!(
            crowding_debt(&store, &db, "now", None).is_none(),
            "an item the real ranker does show must not be asked about as though it were invisible"
        );
    }

    /// The other half, and the one that must not regress: an item that really
    /// cannot appear still holds the turn. Without it, "ask the real ranker"
    /// could quietly become "never ask anything".
    ///
    /// A DIRECTORY binding is the honest case, and the reason a plain crowded
    /// pool is not: measured while writing this, a newcomer of equal weight on
    /// a full PATH pool is shown anyway - it wins on recency and pushes an
    /// older holder out - so the write-time note overstated that case too. A
    /// Dir binding reaches no automatic surface at all (`rank::select` drops it
    /// before comparing a single path), so here the estimate and the ranker
    /// agree, and the debt is owed.
    #[test]
    fn an_item_that_can_never_appear_still_holds_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        record_session_watermark(&db, "now");
        let mut mine = path_newcomer("mine", "p", "everything under this folder is generated, never hand-edited");
        mine.bindings = vec![Binding::Target {
            kind: model::item::TargetKind::Dir,
            value: "server/generated".to_string(),
        }];
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        let asked = crowding_debt(&store, &db, "now", None).expect("an item no surface can reach must still hold the turn");
        assert!(asked.contains("mine"), "{asked}");
    }

    /// THE LAZINESS THIS REMOVES. The write response already said "this may
    /// well never be shown there". Across two real sessions that note was
    /// reported and then left alone, and the fact stayed invisible. A
    /// maintenance step that depends on remembering works exactly as often as
    /// people feel like it, which is the same defect the useful-mark had.
    #[test]
    fn a_fact_written_onto_a_full_place_holds_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        // What SessionStart does: mark where the log stood when we began.
        record_session_watermark(&db, "now");
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("mine", "p")).unwrap();

        let asked = crowding_debt(&store, &db, "now", None).expect("a crowded write must hold the turn");
        assert!(asked.contains("mine"), "{asked}");
        assert!(asked.contains("FOLD"), "it must say what to do, not just that something is wrong: {asked}");
        assert!(asked.contains("never do is raise its severity"), "{asked}");
    }

    /// It asks about what YOU made, never about somebody else's backlog. A
    /// session that wrote nothing crowded ends silently.
    #[test]
    fn an_earlier_sessions_crowded_fact_is_never_this_sessions_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        // Written BEFORE this session began - the watermark is taken after it.
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("theirs", "p")).unwrap();
        record_session_watermark(&db, "today");

        assert!(crowding_debt(&store, &db, "today", None).is_none(), "a fresh session inherits no older mess");
        assert!(
            crowding_debt(&store, &db, "never-started", None).is_none(),
            "a session whose start was never seen must stay silent, never inherit everything"
        );
    }

    /// IT CARRIES NO STATE, so it clears by construction. Retracting the fact
    /// is one of the three ways out, and nothing has to be told about it.
    #[test]
    fn retracting_the_crowded_fact_clears_the_debt_with_nothing_to_update() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("mine", "p")).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_some(), "fixture sanity");

        model::store::retract(&mut store, "mcp", "mcp", "t", "mine", "folded into the item that already said it")
            .unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_none(), "folding it away settles it, with no marker to update");
    }

    /// The other way out: move it somewhere with room. Same story - the debt
    /// is derived from the store as it stands, so re-anchoring settles it.
    #[test]
    fn re_anchoring_it_somewhere_with_room_clears_the_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_some(), "fixture sanity");

        let mut moved = mine.clone();
        moved.bindings = vec![Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "server/lib/labels.js".to_string(),
        }];
        model::store::revise(&mut store, "mcp", "mcp", "t", &mine, &moved).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_none(), "a place with room settles it");
    }

    /// The cross-session leak this closes, measured 2026-08-14: two Claude
    /// sessions share one store, every tool-server write is stamped the
    /// constant "mcp", so the second session's writes land above the first's
    /// watermark and read as the first's own. The item's PROJECT is the signal
    /// the id is not - a session working in one repo is nagged only about
    /// crowded facts in that repo, or global ones, never another repo's.
    #[test]
    fn a_crowded_fact_in_another_project_does_not_nag_this_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("theirs", "p")).unwrap();

        // A session working in a DIFFERENT project hears nothing about it.
        assert!(
            crowding_debt(&store, &db, "now", Some("other-repo")).is_none(),
            "a fact scoped to another project was another session's write, not this one's"
        );
        // The session whose project it belongs to still gets nagged.
        assert!(
            crowding_debt(&store, &db, "now", Some("p")).is_some(),
            "the session actually in that project must still hear it"
        );
        // With no project to route by, nothing is filtered - the watermark stands alone.
        assert!(
            crowding_debt(&store, &db, "now", None).is_some(),
            "an unresolved project falls back to the old behaviour"
        );
    }

    /// A rule as it exists in a store written BEFORE the gate started asking:
    /// it names something concrete and carries no check. The gate refuses to
    /// create one now, which is the point of the gate - so the only honest way
    /// to test the backlog burn is to put a legacy row in directly, exactly as
    /// the migration did.
    fn legacy_unanswered(store: &mut EventStore, id: &str, text: &str) {
        let mut item = crowded_newcomer(id, "p");
        item.text = text.to_string();
        let body = serde_json::to_string(&item).unwrap();
        store
            .append_event("legacy", id, "migration", thor_core::event_store::EventKind::FactCreated, id, None, &body)
            .unwrap();
    }

    #[test]
    fn the_burn_takes_one_turn_per_session_not_every_turn() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        assert!(teeth_not_yet_asked_this_session(&db, "s1"), "a fresh session has not been asked");
        record_teeth_asked(&db, "s1");
        assert!(!teeth_not_yet_asked_this_session(&db, "s1"), "the same session must not be held again");
        assert!(teeth_not_yet_asked_this_session(&db, "s2"), "a different session gets its own turn");
    }

    #[test]
    fn a_rule_naming_a_command_is_found_without_anybody_pointing_at_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_unanswered(&mut store, "names-a-flag", "Draai npm audit fix --force nooit op deze repo");

        let asked = teeth_debt(&store).expect("a rule naming a flag must be found by the sweep itself");
        assert!(asked.contains("names-a-flag"), "it must name which rule: {asked}");
        assert!(asked.contains("--force"), "and quote what it spotted: {asked}");
    }

    /// One rule per session is a burn that never finishes: 336 were waiting on
    /// 2026-08-14, and the answer to each is a single sentence. The batch is
    /// what turns a year into weeks. It must also say what it held back - five
    /// names with nothing after them read as the whole backlog.
    #[test]
    fn the_burn_asks_a_batch_and_says_what_it_held_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        for i in 0..TEETH_DEBT_BATCH_MAX + 2 {
            legacy_unanswered(&mut store, &format!("unarmed-{i}"), &format!("Draai npm audit fix --force-{i} nooit"));
        }

        let asked = teeth_debt(&store).expect("several rules are unanswered");
        for i in 0..TEETH_DEBT_BATCH_MAX {
            assert!(asked.contains(&format!("unarmed-{i}")), "the whole batch is named: {asked}");
        }
        assert!(
            !asked.contains(&format!("unarmed-{TEETH_DEBT_BATCH_MAX}")),
            "and it stops at the cap: {asked}"
        );
        assert!(asked.contains("2 more are waiting"), "silence about the rest would read as done: {asked}");
    }

    #[test]
    fn either_answer_settles_it_and_only_silence_brings_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_unanswered(&mut store, "answer-me", "Draai npm audit fix --force nooit op deze repo");
        assert!(teeth_debt(&store).is_some(), "fixture sanity");

        let item = model::store::show(&store, "answer-me").unwrap();
        let mut answered_no = item.clone();
        answered_no.tags.push(format!("{}a test fixture with nothing literal to catch", model::store::NO_LITERAL_REASON_PREFIX));
        model::store::revise(&mut store, "s", "l", "a", &item, &answered_no).unwrap();
        assert!(teeth_debt(&store).is_none(), "answering 'nothing to catch' must settle it for good");
    }

    /// The ground must not become noise. A rule that names nothing concrete
    /// has no question to answer, and asking anyway would teach the reader to
    /// dismiss this debt on sight.
    #[test]
    fn a_rule_that_names_nothing_concrete_is_never_asked() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let mut item = crowded_newcomer("pure-judgement", "p");
        item.text = "Vraag eerst toestemming voor je iets onomkeerbaars doet".to_string();
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        assert!(teeth_debt(&store).is_none(), "there is nothing here a guard could look for");
    }

    #[test]
    fn prose_that_merely_contains_a_slash_or_a_dot_is_not_a_path() {
        assert_eq!(model::gate::candidate_literal("werk dev->prod bij en/of herstart"), None);
        assert_eq!(model::gate::candidate_literal("dat kost 15-20 min. in totaal"), None);
        assert_eq!(
            model::gate::candidate_literal("wijzig files/deploy-watcher.sh en push"),
            Some("files/deploy-watcher.sh".to_string())
        );
    }

    /// A rule that already carries a check has been armed; asking again would
    /// be asking a settled question.
    #[test]
    fn a_rule_that_already_has_teeth_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let mut item = crowded_newcomer("already-armed", "p");
        item.text = "Draai npm audit fix --force nooit op deze repo".to_string();
        item.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        assert!(teeth_debt(&store).is_none());
    }

    /// The THIRD way out, and until 2026-08-09 the only one the message
    /// promised without the code honouring it. Saying "it belongs here" in a
    /// reply settled nothing, so the debt re-fired every turn and the only
    /// escape left was deleting a true fact.
    #[test]
    fn judging_the_crowd_deserved_settles_the_debt_without_moving_anything() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_some(), "fixture sanity");

        let mut judged = mine.clone();
        judged.tags.push(model::store::CROWDED_ON_PURPOSE_TAG.to_string());
        model::store::revise(&mut store, "mcp", "mcp", "t", &mine, &judged).unwrap();
        assert!(
            crowding_debt(&store, &db, "now", None).is_none(),
            "a recorded decision settles it, with the item exactly as crowded as before"
        );
    }

    /// THE DEFECT THIS PREVENTS, measured 2026-08-15 in a session that had just
    /// thinned 39 crowded facts: the tag is matched WHOLE, so writing it with a
    /// reason attached settles nothing and the debt asks again every turn. The
    /// worker had every reason to attach one - the neighbouring `no-literal`
    /// exit REQUIRES a reason - so the two exits read as one convention and are
    /// opposites. This is the trap; the next test is the message that names it.
    #[test]
    fn a_reason_appended_to_the_tag_does_not_settle_the_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        let mut judged = mine.clone();
        judged.tags.push(format!(
            "{}:de plek is eerlijk vol met zwaardere regels",
            model::store::CROWDED_ON_PURPOSE_TAG
        ));
        model::store::revise(&mut store, "mcp", "mcp", "t", &mine, &judged).unwrap();
        assert!(
            crowding_debt(&store, &db, "now", None).is_some(),
            "a reason attached to the tag is not the tag - the debt must keep asking, \
             which is exactly why the message has to say so"
        );
    }

    /// THE CONTRADICTION THIS CLOSES, reported by a session evaluation on
    /// 2026-08-16: the owner's standing requirement is zero crowding, while this
    /// message offered "sometimes the place is honestly full of heavier things"
    /// as an unconditional third exit. Both cannot be true, and a worker who
    /// follows the message ends up in breach of the requirement.
    ///
    /// The condition that makes exit 3 honest is narrow and now stated: seats
    /// held by rules that can each REFUSE a write. Those cannot be archived (the
    /// gate turns that down) and folding one would spend a check. A seat held by
    /// a description is the opposite case - it should be archived so the rule
    /// that can block takes the seat.
    #[test]
    fn leaving_it_is_offered_only_for_seats_held_by_rules_that_can_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        let message = crowding_debt(&store, &db, "now", None).expect("fixture sanity");
        assert!(
            message.contains("REFUSE a write"),
            "exit 3 must name the condition that makes it honest"
        );
        assert!(
            message.contains("archive a description"),
            "and must say what to do when the seats are held by descriptions instead"
        );
        assert!(
            !message.contains("full of heavier things"),
            "the unconditional wording contradicted the owner's zero-crowding requirement"
        );
    }

    /// A trap the code cannot forgive has to be named where the worker reads it.
    /// The `no-literal` exit already states its own form; this one did not, and
    /// that asymmetry is what cost a session an hour.
    #[test]
    fn the_message_says_the_tag_counts_only_bare() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        let message = crowding_debt(&store, &db, "now", None).expect("fixture sanity");
        assert!(message.contains("BARE"), "the message must say the tag takes no reason");
        assert!(
            message.contains("no-literal"),
            "and must name the neighbouring exit it is the opposite of"
        );
    }

    /// The tag settles the DEBT, not the crowding. An item that took exit 3 is
    /// still crowded out, and every count that measures reach must still say so
    /// - otherwise the escape hatch would quietly improve the numbers.
    #[test]
    fn taking_the_third_exit_does_not_pretend_the_place_has_room() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        let mut mine = crowded_newcomer("mine", "p");
        mine.tags.push(model::store::CROWDED_ON_PURPOSE_TAG.to_string());
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();
        assert!(
            matches!(model::store::capacity(&store, &mine), Ok(model::store::Capacity::Crowded(_))),
            "the tag is a decision about the debt, never a claim that the place is free"
        );
    }


    fn declare(store: &mut EventStore, id: &str, pinned: bool) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("something worth knowing about {id}"),
            bindings: if pinned {
                vec![Binding::Always]
            } else {
                vec![Binding::Moment(intent::Action::Commit)]
            },
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        };
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
    }

    fn serve_it(store: &mut EventStore, id: &str, times: usize) {
        serve_as(store, "s", id, times);
    }

    fn serve_as(store: &mut EventStore, session: &str, id: &str, times: usize) {
        for _ in 0..times {
            deliver::record_delivery(store, session, session, "t", "2026-08-07T00:00:00Z", &[id.to_string()]);
        }
    }

    /// THE DEFECT THIS PREVENTS, and it bit within minutes of being possible.
    /// The served and judged counts are folded from event kinds, which keep
    /// every serving an item ever had - including the ones from before
    /// somebody retracted it. So a retracted item stayed owed forever, and
    /// the ask had no honest answer: "did it belong where it fired" cannot be
    /// answered about something that is gone. On 2026-08-08 a de-duplication
    /// retracted 57 items and the very next Stop asked about one of them by
    /// name, quoting an empty text because `show` refuses a tombstone.
    #[test]
    fn a_retracted_item_is_never_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "gone", false);
        serve_it(&mut store, "gone", JUDGEMENT_DEBT_AFTER);
        assert!(judgement_debt(&store, "s").is_some(), "fixture sanity: it is owed while it is live");

        model::store::retract(&mut store, "t", "t", "t", "gone", "merged into another item").unwrap();
        assert!(
            judgement_debt(&store, "s").is_none(),
            "a retracted item must drop out of the debt, or it is owed forever with no way to settle it"
        );
    }

    /// THE DEFECT THIS PREVENTS: one answer settling an item for life. The
    /// debt used to hold a lifetime judged-set, so the first verdict about an
    /// item was also the last, however far the item drifted afterwards. That
    /// is the same shape `decay::is_stale` carried until 2026-08-08, and it
    /// bit hardest here, because the debt asks first about the items that
    /// fire most - the ones whose bindings are most worth a second look.
    ///
    /// A verdict now buys quiet, not immunity: the count resets, and another
    /// `JUDGEMENT_DEBT_AFTER` firings earn another question.
    #[test]
    fn a_judged_item_is_asked_about_again_after_it_has_fired_that_many_times_since() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "drifter", false);
        serve_it(&mut store, "drifter", JUDGEMENT_DEBT_AFTER);
        assert!(judgement_debt(&store, "s").is_some(), "fixture sanity: it is owed");

        serve::mark::record_useful(&mut store, "s", "s", "t", "2026-08-08T00:00:00Z", "drifter").unwrap();
        assert!(judgement_debt(&store, "s").is_none(), "a verdict must settle it for now");

        serve_it(&mut store, "drifter", JUDGEMENT_DEBT_AFTER - 1);
        assert!(judgement_debt(&store, "s").is_none(), "one short of the threshold is not owed yet");

        serve_it(&mut store, "drifter", 1);
        let asked = judgement_debt(&store, "s").expect("forty more firings must earn a second question");
        assert!(asked.contains("drifter"), "{asked}");
    }

    /// THE DEFECT THIS PREVENTS: asking for a verdict that has no honest
    /// answer. The question is "did it belong where it fired", and for a
    /// pinned item the owner answered that when he pinned it - so "useful"
    /// is a lie whenever the rule simply did not come up, and "noise"
    /// overrules his own decision. Nineteen of the first forty-two owed were
    /// of exactly that kind, found by trying to pay one.
    #[test]
    fn a_pinned_item_is_never_asked_about_however_often_it_fired() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "pinned-one", true);
        serve_it(&mut store, "pinned-one", JUDGEMENT_DEBT_AFTER + 5);
        assert!(
            judgement_debt(&store, "s").is_none(),
            "a pinned item fires by the owner's own choice - there is nothing to judge"
        );
    }

    /// And the mechanism is still armed for what fires because a TRIGGER
    /// matched, which is the case where the question is real.
    #[test]
    fn a_trigger_bound_item_is_still_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "on-a-moment", false);
        serve_it(&mut store, "on-a-moment", JUDGEMENT_DEBT_AFTER + 5);
        let reason = judgement_debt(&store, "s").expect("a trigger-bound item must still be asked about");
        assert!(reason.contains("on-a-moment"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS: asking this reader about something only
    /// SOMEBODY ELSE was ever served. A lifetime count says an item fired a
    /// hundred times; a rule scoped to another project fired all hundred of
    /// them there, and "did it belong where it fired" then has no form this
    /// session can answer. Found by being asked about an acme-shop commit
    /// rule during work in thor-2.
    #[test]
    fn an_item_this_session_never_saw_is_not_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "elsewhere", false);
        serve_as(&mut store, "another-session", "elsewhere", JUDGEMENT_DEBT_AFTER + 5);
        assert!(
            judgement_debt(&store, "mine").is_none(),
            "this session never saw it fire, so it has nothing truthful to say about it"
        );
        assert!(
            judgement_debt(&store, "another-session").is_some(),
            "the session that DID see it is still asked"
        );
    }

    /// Below the threshold nothing is owed: this is about the handful of
    /// rules constantly in front of a reader, not about everything served.
    #[test]
    fn firing_a_few_times_owes_nothing() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "rarely", false);
        serve_it(&mut store, "rarely", JUDGEMENT_DEBT_AFTER - 1);
        assert!(judgement_debt(&store, "s").is_none());
    }

    /// THE DEFECT THIS PREVENTS: unpinning a batch of long-pinned items (each
    /// carrying a large lifetime serving count from its time as `Always`)
    /// makes all of them owed in the same instant, and asking about them one
    /// per Stop turns a single backlog into that many separate blocked turns
    /// - each one repaying a long conversation's full history for a single
    /// id. One block must be able to carry more than one id.
    #[test]
    fn multiple_owed_items_are_all_asked_about_in_one_block() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "first", false);
        declare(&mut store, "second", false);
        serve_it(&mut store, "first", JUDGEMENT_DEBT_AFTER);
        serve_it(&mut store, "second", JUDGEMENT_DEBT_AFTER);

        let asked = judgement_debt(&store, "s").expect("two owed items must still produce one block");
        assert!(asked.contains("first"), "{asked}");
        assert!(asked.contains("second"), "{asked}");
    }

    /// A verdict on one of several owed items settles only that one - the
    /// others must still be owed and still asked about together, not
    /// silently cleared as a side effect of somebody else's answer.
    #[test]
    fn judging_one_of_several_leaves_the_rest_owed_together() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "answered", false);
        declare(&mut store, "still-owed-a", false);
        declare(&mut store, "still-owed-b", false);
        serve_it(&mut store, "answered", JUDGEMENT_DEBT_AFTER);
        serve_it(&mut store, "still-owed-a", JUDGEMENT_DEBT_AFTER);
        serve_it(&mut store, "still-owed-b", JUDGEMENT_DEBT_AFTER);

        serve::mark::record_useful(&mut store, "s", "s", "t", "2026-08-13T00:00:00Z", "answered").unwrap();

        let asked = judgement_debt(&store, "s").expect("two items are still owed");
        assert!(!asked.contains("answered"), "a settled item must not reappear: {asked}");
        assert!(asked.contains("still-owed-a"), "{asked}");
        assert!(asked.contains("still-owed-b"), "{asked}");
    }

    /// THE CAP MUST STAY A CAP, not become a new unbounded block under a
    /// different name - the whole point of batching was to stop paying a
    /// long conversation's full history once per owed id, not to risk one
    /// enormous block instead. Past the cap, it must say how many it held
    /// back rather than silently dropping them.
    #[test]
    fn the_batch_is_capped_and_names_how_many_it_held_back() {
        let mut store = EventStore::in_memory().unwrap();
        let n = JUDGEMENT_DEBT_BATCH_MAX + 3;
        for i in 0..n {
            let id = format!("owed-{i}");
            declare(&mut store, &id, false);
            serve_it(&mut store, &id, JUDGEMENT_DEBT_AFTER);
        }

        let asked = judgement_debt(&store, "s").expect("a large backlog is still owed");
        let shown = (0..n).filter(|i| asked.contains(&format!("owed-{i}"))).count();
        assert_eq!(shown, JUDGEMENT_DEBT_BATCH_MAX, "the block must show exactly the cap, not more: {asked}");
        assert!(asked.contains(&format!("{} more", n - JUDGEMENT_DEBT_BATCH_MAX)), "{asked}");
    }
}

#[cfg(test)]
mod remember_moment_tests {
    use super::*;

    /// THE DEFECT THIS PREVENTS: a namespaced call to this memory's own
    /// `remember` tool is not recognised at all, so `intent::Action::Remember`
    /// - which nothing else can ever derive - never fires on the one call it
    /// exists for.
    #[test]
    fn a_namespaced_remember_call_is_detected() {
        assert!(is_remember_moment("mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__remember"));
    }

    /// The other tool name this same detection must cover.
    #[test]
    fn a_namespaced_revise_call_is_detected() {
        assert!(is_remember_moment("mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__revise"));
    }

    /// THE DEFECT THIS PREVENTS: matching the WHOLE "mcp__<server>__<tool>"
    /// string, or hardcoding one particular server name, so a rename of the
    /// server in the user's own Claude Code config (already happened once in
    /// this project's own history) would silently stop detection working.
    /// Proven with two different server names, neither the one used above -
    /// this pins the SUFFIX match, not a literal.
    #[test]
    fn a_differently_named_server_is_still_detected_because_only_the_suffix_is_matched() {
        assert!(is_remember_moment("mcp__some-completely-different-server-name__remember"));
        assert!(is_remember_moment("mcp__thor2__revise"));
    }

    /// THE DEFECT THIS PREVENTS: the suffix match is so loose it fires on any
    /// tool at all - a built-in tool with no namespace, and a namespaced call
    /// to a DIFFERENT tool on this very server, must both come back false.
    #[test]
    fn an_unrelated_tool_produces_no_moment() {
        assert!(!is_remember_moment("Write"));
        assert!(!is_remember_moment("Bash"));
        assert!(!is_remember_moment("mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__lookup"));
        assert!(!is_remember_moment("mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__get"));
    }
}

/// The channel boundary itself: whatever `hook_once` returns or panics with,
/// this prints at most one JSON object and always exits 0. A silent panic
/// hook is installed first so a bug in the selection/render path cannot even
/// leak a Rust panic message - R5's "never speak on failure" applied at the
/// one place it has to hold regardless of what changes underneath it.
///
/// THE JUDGE REENTRANCY GUARD (see `serve::reentry`'s own doc comment and
/// `JUDGE-TRANSPORT.md`'s "Judge reentrancy" section for the whole hazard):
/// this is the FIRST thing `cmd_hook` does, before the panic hook, before
/// `hook_once`, before the store is ever opened, before ANY surface runs.
/// `serve::reentry::is_reentrant()` is true exactly when this process is a
/// descendant of a judge invocation (the child `claude` CLI process the
/// judge transport spawned, or anything THAT process in turn started - a
/// hook it fires, an MCP server it registers) - marked by
/// `judge::run_judge_command` via an inherited environment variable, never
/// by anything this process has to notice about its own call site. When
/// marked, this function does NOTHING AT ALL: no injection, no block, no
/// delivery event, no store write - it exits immediately, exactly as
/// successfully as a hook that simply had nothing to say. This is not an
/// error path and must never look like one; a marked invocation is the
/// EXPECTED shape for a hook firing inside the judge's own child session,
/// not a failure.
fn cmd_hook(db_path: &Path) {
    if serve::reentry::is_reentrant() {
        std::process::exit(0);
    }
    std::panic::set_hook(Box::new(|_| {}));
    let db_path = db_path.to_path_buf();
    let result = std::panic::catch_unwind(move || hook_once(&db_path));
    match result {
        Ok(Some(HookOutput::Context { event_name, block })) => {
            let out = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": event_name,
                    "additionalContext": block,
                }
            });
            let _ = serde_json::to_writer(std::io::stdout(), &out);
        }
        Ok(Some(HookOutput::ContextWithNotice { event_name, block, notice })) => {
            let out = serde_json::json!({
                "systemMessage": notice,
                "hookSpecificOutput": {
                    "hookEventName": event_name,
                    "additionalContext": block,
                }
            });
            let _ = serde_json::to_writer(std::io::stdout(), &out);
        }
        // The Response Guard's verdict is its own top-level shape, printed
        // verbatim - never wrapped in additionalContext.
        Ok(Some(HookOutput::Decision(v))) => {
            let _ = serde_json::to_writer(std::io::stdout(), &v);
        }
        // A WARN-tier verdict at Stop - kept as its own match arm rather
        // than folded into `Context`'s (not merely to save a few lines)
        // because the two mean different things, and no longer render the
        // same way either: `Context` is an injection surface handing the
        // model ONE field, `hookSpecificOutput.additionalContext`. `Warn` is
        // the Response Guard's own softer verdict, and has a second audience
        // `Context` never needs to reach - the owner, not just the model -
        // so it also sets a top-level `systemMessage` carrying the same
        // text, confirmed by Claude Code's own hooks documentation as the
        // field "shown to the user". See `HookOutput::Warn`'s own doc
        // comment for what that documentation settles and the one question
        // it still leaves open - if that remaining assumption is ever found
        // wrong, only this arm needs to change.
        Ok(Some(HookOutput::Warn { event_name, text })) => {
            let out = serde_json::json!({
                "systemMessage": text,
                "hookSpecificOutput": {
                    "hookEventName": event_name,
                    "additionalContext": text,
                }
            });
            let _ = serde_json::to_writer(std::io::stdout(), &out);
        }
        _ => {}
    }
    std::process::exit(0);
}

// -------------------------------------------------------- human-facing CLI

fn open_store_or_die(db_path: &Path) -> EventStore {
    match EventStore::new(db_path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("could not open store at {}: {e}", db_path.display());
            std::process::exit(1);
        }
    }
}

fn cmd_check(db_path: &Path, input: &ServeInput) {
    if input.is_empty() {
        println!("no intent detected - nothing would fire");
        return;
    }
    if input.moments.is_empty() {
        println!("detected: (no action; matching on target only)");
    } else {
        println!("detected: {}", input.moments.iter().map(Action::as_str).collect::<Vec<_>>().join(", "));
    }
    let store = open_store_or_die(db_path);
    let served = serve::serve(&store, input);
    match render::render_text(&served.selection, &input.moments) {
        Some(block) => println!("\n{block}"),
        None => println!("\n(no item governs this)"),
    }
}

fn cmd_why(db_path: &Path, input: &ServeInput) {
    let store = open_store_or_die(db_path);
    let served = serve::serve(&store, input);
    let shown_ids: std::collections::HashSet<&str> =
        served.selection.shown.iter().map(|r| r.id.as_str()).collect();
    println!("{} item(s) apply; the block would show {}.\n", served.all.len(), served.selection.shown.len());
    for ranked in &served.all {
        let mark = if shown_ids.contains(ranked.id.as_str()) { " " } else { "-" };
        let severity = ranked.item.severity.map(|s| format!("{s:?}")).unwrap_or_else(|| "none".to_string());
        println!("{mark} [{severity}] {}", ranked.item.text);
        println!("    id {}   kind {:?}", ranked.id, ranked.item.kind);
    }
}

fn cmd_audit(db_path: &Path) {
    let store = open_store_or_die(db_path);
    let rows = serve::audit::audit_rows(&store);
    println!("{:<28} {:<12} {:<13} {:>5}  last served", "id", "kind", "severity", "count");
    for row in &rows {
        let severity = row.item.severity.map(|s| format!("{s:?}")).unwrap_or_else(|| "none".to_string());
        let last = row.stats.last_served_at.clone().unwrap_or_else(|| "never".to_string());
        println!(
            "{:<28} {:<12} {:<13} {:>5}  {}",
            row.id,
            format!("{:?}", row.item.kind),
            severity,
            row.stats.times_served,
            last
        );
    }
    let never = rows.iter().filter(|r| r.stats.times_served == 0).count();
    println!("\ndeclared, never delivered: {never} of {}", rows.len());
}

// --------------------------------------------------- surface 1: session start

/// Preview of surface 1: prints every Always item (global plus `project`),
/// full and never capped. Does not record a delivery - only `hook`'s
/// SessionStart branch does that (see `hook_once`'s own doc comment). Prints
/// the resolved project first, so a person can see what `project::
/// resolve_project` decided (marker file, git root, or neither) without
/// reading source.
fn cmd_session_start(db_path: &Path, project: Option<&str>) {
    let store = open_store_or_die(db_path);
    println!("project: {}", project.unwrap_or("(none - global only)"));
    let candidates = serve::live::always_candidates(&store);
    let decay = DecayContext::load(&store);
    let items = serve::decay::retain_live(session_start::select(&candidates, project), &decay);
    match session_start::render(&items) {
        Some(block) => println!("{block}"),
        None => println!("(no standing rules apply)"),
    }
}

// -------------------------------------------------------- surface 3: prompt

/// Preview of surface 3: prints what a raw prompt resolves to, or says so
/// plainly when it resolves to nothing. Does not record a delivery - only
/// `hook`'s UserPromptSubmit branch does that.
fn cmd_prompt(db_path: &Path, text: &str) {
    let store = open_store_or_die(db_path);
    let candidates = serve::live::live_items(&store);
    let input = prompt::resolve(text, &candidates);
    if input.is_empty() {
        println!("nothing resolves - no moment and no declared target was named");
        return;
    }
    let decay = DecayContext::load(&store);
    let all = serve::decay::retain_live(serve::rank::select(&candidates, &input), &decay);
    let selection = render::cap(all);
    match render::render_text(&selection, &input.moments) {
        Some(block) => println!("{block}"),
        None => println!("(no item governs this)"),
    }
}

// ------------------------------------------------------- surface 4: lookup

/// Surface 4: free-text search over every live item, any project, archive
/// kinds (Report/Chunk) included - never an injection surface, so there is
/// no delivery to record here at all. Calls `search_best_effort`, which
/// silently degrades to plain text match without the `semantic` feature, or
/// with it but no usable model/sidecar - so this one call site is correct in
/// every build without an `#[cfg]` of its own.
fn cmd_search(db_path: &Path, query: &str) {
    let store = open_store_or_die(db_path);
    let vectors_path = serve::semantic_paths::default_vectors_path(db_path);
    let hits = lookup::search_best_effort(&store, &vectors_path, None, query);
    // How many its own expiry rule held back, so a thin answer is never
    // mistaken for an empty memory (see lookup::search_with_expired).
    let withheld = lookup::search_with_expired(&store, query).1;
    if hits.is_empty() {
        println!("no matches for '{query}'");
    } else {
        for hit in &hits {
            println!("{:<28} {:<12} {}", hit.id, format!("{:?}", hit.item.kind), hit.item.text);
        }
    }
    if withheld > 0 {
        println!(
            "({withheld} match(es) held back: their own expiry date has passed. `get <id>` still shows them whole.)"
        );
    }
}

/// Surface 4's other door: an explicit request for exactly one Lookup's key.
fn cmd_lookup_key(db_path: &Path, key: &str) {
    let store = open_store_or_die(db_path);
    match lookup::by_key(&store, key) {
        Some(hit) => println!("{}", hit.item.text),
        None => println!("no lookup found for key '{key}'"),
    }
}

/// A human line for a code answer's provenance - shared by `cmd_search_code`
/// and `cmd_status` so the two never phrase "how stale is this" differently.
fn format_code_provenance(p: &lookup::CodeProvenance) -> String {
    let current = match &p.current_commit {
        Ok(c) => c.clone(),
        Err(e) => format!("unknown ({e})"),
    };
    let differ = p.files_differ.map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".to_string());
    let uncommitted =
        p.uncommitted_changed.map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".to_string());
    format!(
        "code index at commit {}, current commit {current}, {differ} file(s) differ, {uncommitted} file(s) uncommitted",
        p.indexed_commit
    )
}

/// Surface 4's code door: search the code index against a repository,
/// printing its own drift line first, then every hit with the commit it
/// came from. A human-facing lookup, so a real error (a missing index, an
/// unreachable repository) is reported plainly and exits non-zero, never
/// swallowed.
fn cmd_search_code(index_db: &Path, repo: &Path, query: &str, limit: usize) {
    match lookup::search_code(index_db, repo, query, limit) {
        Ok(answer) => {
            println!("{}", format_code_provenance(&answer.provenance));
            if answer.hits.is_empty() {
                println!("no matches for '{query}'");
                return;
            }
            for hit in &answer.hits {
                println!(
                    "{}:{}-{} (commit {})\n{}\n",
                    hit.path, hit.start_line, hit.end_line, hit.commit_id, hit.text
                );
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Who defines and who uses one name. Provenance first, deliberately: a line
/// number from an index three commits behind is a wrong line number, and the
/// reader has to see that before the list, not after it.
fn cmd_where_used(index_db: &Path, repo: &Path, name: &str, limit: usize) {
    match lookup::where_used(index_db, repo, name, limit) {
        Ok(usage) => {
            println!("{}", format_code_provenance(&usage.provenance));
            if usage.defined_at.is_empty() && usage.referenced_at.is_empty() {
                println!("the index knows no symbol named '{name}'");
                return;
            }
            println!("defined at ({}):", usage.defined_at.len());
            for s in &usage.defined_at {
                println!("  {}:{}", s.path, s.line);
            }
            println!("used at ({}):", usage.referenced_at.len());
            for s in &usage.referenced_at {
                println!("  {}:{}", s.path, s.line);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// A file's shape. "Never indexed" and "indexed, defines nothing" are printed
/// as different sentences, because they call for different next steps.
fn cmd_outline(index_db: &Path, path: &str) {
    match lookup::outline(index_db, path) {
        Ok(None) => {
            println!(
                "'{path}' is not in the code index - check the path is repository-relative, or \
                 the file was added after the index was last built"
            );
        }
        Ok(Some(names)) if names.is_empty() => {
            println!("'{path}' is indexed but defines no named symbols");
        }
        Ok(Some(names)) => {
            println!("{path} ({} definitions):", names.len());
            for (name, line) in &names {
                println!("  {line}: {name}");
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

// ------------------------------------------------------------------ status

/// What this store knows right now (CONTRACT gap 3): the fact-store half
/// (item counts per kind, how many are declared but never fired) always
/// runs; the code-index half only when both `--index-db` and `--repo` are
/// given - printed as a plain sentence either way, so "not configured" is
/// never confused with "configured and silent".
fn cmd_status(db_path: &Path, index_db: Option<&Path>, repo: Option<&Path>) {
    let store = open_store_or_die(db_path);
    let s = status::store_status(&store);
    println!("{:<12} {:>6}", "kind", "count");
    for row in &s.counts {
        println!("{:<12} {:>6}", format!("{:?}", row.kind), row.count);
    }
    println!("\ndeclared, never fired: {} of {} fireable item(s)", s.declared_never_fired, s.fireable_total);
    println!(
        "served repeatedly, never marked useful: {} of {} fireable item(s) - a count to look at, not a filter",
        s.served_never_marked, s.fireable_total
    );
    println!(
        "retired as noise (called noise {}+ times since the last mark of usefulness): {} of {} fireable item(s) - still fully findable via search",
        serve::decay::NOISE_MARKS_BEFORE_STALE, s.decayed, s.fireable_total
    );
    println!("missing falsifier: {} of {} fireable item(s)", s.missing_falsifier, s.fireable_total);
    println!("\n{}", s.semantic_search);
    #[cfg(feature = "semantic")]
    print_vectors_report(&store, &serve::semantic_paths::default_vectors_path(db_path));

    match (index_db, repo) {
        (Some(index_db), Some(repo)) => match lookup::code_index_status(index_db, repo) {
            Ok(p) => println!("\n{}", format_code_provenance(&p)),
            Err(e) => println!("\ncode index: {e}"),
        },
        (None, None) => println!("\ncode index: not configured (pass --index-db and --repo to check one)"),
        _ => println!("\ncode index: both --index-db and --repo are required to check one"),
    }
}

// ---------------------------------------------- surface 4: vectors (semantic)

/// Print how many vectors are stored and whether they still match the
/// current content - shared by `cmd_status`'s own semantic block and the
/// dedicated `vectors-status` command, so the two can never phrase this
/// differently.
#[cfg(feature = "semantic")]
fn print_vectors_report(store: &EventStore, vectors_path: &Path) {
    match serve::vectors::report(store, vectors_path) {
        Ok(r) => {
            println!("vectors sidecar : {}", vectors_path.display());
            println!("model_id        : {}", r.model_id_stored.as_deref().unwrap_or("(none - never built)"));
            println!("expected        : {}", r.model_id_expected);
            println!(
                "model_id        : {}",
                if r.model_id_matches() { "matches" } else { "MISMATCH - vectors_build must run again" }
            );
            println!("stored vectors  : {} (of {} live item(s))", r.stored_count, r.live_count);
            println!("missing (no vector yet)                : {}", r.missing);
            println!("stale (content changed since embedding) : {}", r.stale);
            println!("orphaned (no longer a live item)        : {}", r.orphaned);
        }
        Err(e) => println!("semantic vectors: could not be read ({e})"),
    }
}

/// Rebuild the sidecar from every live, non-Lookup item (feature `semantic`).
#[cfg(feature = "semantic")]
fn cmd_vectors_build(db_path: &Path, model_dir: Option<&Path>, vectors_db: Option<&Path>) {
    let store = open_store_or_die(db_path);
    let model_dir = model_dir.map(PathBuf::from).or_else(serve::semantic_paths::default_model_dir);
    let Some(model_dir) = model_dir else {
        eprintln!("no per-user data directory could be resolved for the model - pass --model-dir");
        std::process::exit(1);
    };
    let vectors_db = vectors_db.map(PathBuf::from).unwrap_or_else(|| serve::semantic_paths::default_vectors_path(db_path));
    match serve::vectors::build(&store, &model_dir, &vectors_db) {
        Ok(n) => println!("built {n} vector(s) at {}", vectors_db.display()),
        Err(e) => {
            eprintln!("could not build vectors: {e}");
            std::process::exit(1);
        }
    }
}

/// How many vectors are stored and whether they still match the current
/// content (feature `semantic`).
#[cfg(feature = "semantic")]
fn cmd_vectors_status(db_path: &Path, vectors_db: Option<&Path>) {
    let store = open_store_or_die(db_path);
    let vectors_db = vectors_db.map(PathBuf::from).unwrap_or_else(|| serve::semantic_paths::default_vectors_path(db_path));
    print_vectors_report(&store, &vectors_db);
}

// -------------------------------------------------------------------- mark

/// Record that `id` actually helped. A human-facing, deliberate write (R5's
/// "write and declare" side): a real failure is reported plainly, never
/// swallowed the way `hook`'s delivery telemetry is.
fn cmd_mark(db_path: &Path, id: &str, noise: bool) {
    let mut store = open_store_or_die(db_path);
    let now = time::now_iso8601();
    let written = if noise {
        mark::record_noise(&mut store, "cli", "cli", "cli", &now, id)
    } else {
        mark::record_useful(&mut store, "cli", "cli", "cli", &now, id)
    };
    match written {
        Ok(_) if noise => println!(
            "marked noise: {id} ({}+ of these, with no mark of usefulness, retires it from the injection surfaces; it stays findable via search)",
            serve::decay::NOISE_MARKS_BEFORE_STALE
        ),
        Ok(_) => println!("marked useful: {id} (clears the noise recorded before it; a later noise mark still counts)"),
        Err(e) => {
            eprintln!("could not record the mark for {id}: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod subagent_detection_tests {
    use super::payload_is_from_a_subagent;
    use serde_json::json;

    /// The documented shape (Claude Code hooks reference): `agent_id` is a
    /// non-empty string present only inside a Task-tool subagent call.
    #[test]
    fn a_payload_carrying_a_real_agent_id_is_a_subagent() {
        let payload = json!({"hook_event_name": "PreToolUse", "session_id": "s1", "agent_id": "a1dca2c0feb7f44fb"});
        assert!(payload_is_from_a_subagent(&payload));
    }

    /// The ordinary shape for the owner's own main session: no `agent_id` key
    /// at all. This is the case that must never misfire - see this
    /// function's own doc comment.
    #[test]
    fn a_payload_with_no_agent_id_is_not_a_subagent() {
        let payload = json!({"hook_event_name": "PreToolUse", "session_id": "s1"});
        assert!(!payload_is_from_a_subagent(&payload));
    }

    /// Defensive: an empty string or a non-string value must not be read as
    /// "present" - only a real, non-empty id counts.
    #[test]
    fn an_empty_or_non_string_agent_id_is_not_a_subagent() {
        assert!(!payload_is_from_a_subagent(&json!({"agent_id": ""})));
        assert!(!payload_is_from_a_subagent(&json!({"agent_id": null})));
        assert!(!payload_is_from_a_subagent(&json!({"agent_id": 42})));
    }
}
