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
            if model::check::run(check, &root) == model::check::Outcome::Fails {
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
    if !found.is_empty() {
        if let Ok(next_seq) = store.get_next_seq() {
            let stale_path = absent_guard::default_stale_path(db);
            let existing = std::fs::read_to_string(&stale_path).ok();
            if let Some(text) =
                absent_guard::record_stale_text(existing.as_deref(), &found, next_seq.saturating_sub(1))
            {
                let _ = std::fs::write(&stale_path, text);
            }
        }
    }

    let _ = std::fs::write(&stamp, &today);
    Some(format!(
        "[THOR] {project}: {dead} anchor(s) point at a file that is not there, so those facts fire nowhere, and {failing} proof(s) now come out false. Run `doctor --checkouts` to see them, or ask me to repair them."
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
        if already_fired {
            return None;
        }
        let msg = payload.get("last_assistant_message").and_then(|v| v.as_str()).unwrap_or("");
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

        // The one debt nobody ever pays voluntarily. Independent of the
        // capture guard above it - that one can be switched off and usually
        // is, and this must not go quiet with it.
        if let Some(reason) = EventStore::open_existing(db_path).ok().and_then(|s| judgement_debt(&s, &session_id)) {
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
/// the ask happens at most once per turn and cannot loop. The set it draws
/// from only ever shrinks: a judged item is never asked about again, by
/// anyone, forever.
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
    let (id, count) = owed.first()?;
    let remaining = owed.len();
    let text = model::store::show(store, id).ok().map(|i| i.text).unwrap_or_default();
    Some(format!(
        "[THOR] '{id}' has been served {count} times and has never once been judged, and {remaining} item(s) are in that state. Judge this one before ending the turn: call mark with its id, noise:true if it did not belong where it fired, or plain if it helped. Two noise judgements since the last mark of usefulness retire an item from every injection surface while leaving it findable; a mark of usefulness clears the noise recorded before it, and a noise mark recorded after it still counts. Nothing else in this system ever retires noise - a serving count decides nothing and silence decides nothing. The item says: {text}"
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
fn record_gate(db_path: &Path, session_id: &str, refused: bool, reason: &str, target: &str) {
    let Some(rule_id) = absent_guard::rule_id_of(reason) else { return };
    let rule_id = rule_id.to_string();
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
        .or_else(|| absent_guard::find_violation(&ranked, content, root))
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
    record_gate(db_path, session_id, true, &reason, file_path);
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
    if findings.is_empty() {
        return;
    }
    let Ok(next_seq) = store.get_next_seq() else { return };
    let now_seq = next_seq.saturating_sub(1);
    let stale_path = absent_guard::default_stale_path(db_path);
    let existing = std::fs::read_to_string(&stale_path).ok();
    if let Some(text) = absent_guard::record_stale_text(existing.as_deref(), &findings, now_seq) {
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

    let marker_path = absent_guard::default_command_marker_path(db_path);
    let marker_text = std::fs::read_to_string(&marker_path).ok();
    let markers = marker_text.as_deref().map(absent_guard::read_blocked).unwrap_or_default();
    // The command arm keys on the ANCHOR alone, deliberately, and unlike the
    // file arm that choice is reasoned in this module's own doc comment: two
    // runs that trip the same rule rarely share the same text (a commit
    // message differs on every retry by design), so keying on the command
    // would suppress almost nothing and the protection would hold in name
    // only. The file arm had no such reasoning, which is why only that one
    // moved to a per-attempt key.
    if absent_guard::already_blocked(&markers, session_id, &anchor) {
        // The one place in the gate that still stands aside on purpose, and
        // therefore the one place that has to say so out loud. If this
        // number ever grows large, the reasoning above is wrong and this arm
        // needs the file arm's treatment.
        record_gate(db_path, session_id, false, &reason, command);
        return None;
    }
    if let Some(text) = absent_guard::mark_blocked_text(marker_text.as_deref(), session_id, &anchor) {
        let _ = std::fs::write(&marker_path, text);
    }
    record_gate(db_path, session_id, true, &reason, command);
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
    if findings.is_empty() {
        return;
    }
    let Ok(next_seq) = store.get_next_seq() else { return };
    let now_seq = next_seq.saturating_sub(1);
    let stale_path = absent_guard::default_stale_path(db_path);
    let existing = std::fs::read_to_string(&stale_path).ok();
    if let Some(text) = absent_guard::record_stale_text(existing.as_deref(), &findings, now_seq) {
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
    if stale_guard::already_blocked(&blocked, session_id) {
        return None;
    }

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
