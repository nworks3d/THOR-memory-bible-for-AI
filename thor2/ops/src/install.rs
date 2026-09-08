//! Installs THOR's hooks into an agent's `settings.json`: SessionStart,
//! PreToolUse and UserPromptSubmit, each pointing at the same `serve hook`
//! command (see `serve/src/bin/serve.rs`'s `cmd_hook`, which branches on the
//! payload's own `hook_event_name`).
//!
//! Non-negotiables (all enforced structurally, not by convention - CONTRACT
//! R1/R7): a back-up is written before anything else touches the file; the
//! file must already be valid JSON or the whole run refuses (never silently
//! starts a fresh settings file over a file the agent could not parse); a
//! second run adds nothing that is already there; and any hook this tool did
//! not put there is never touched, moved, or removed - only appended past.

use serde_json::{json, Value};
use serve::respond;
use std::fs;
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

/// The per-user directory this installer puts a store in when it was not told
/// where to put one: `LOCALAPPDATA` / `XDG_DATA_HOME` / `HOME/.local/share`,
/// then `thor2`. Same resolution order as `serve::semantic_paths`, so a
/// machine ends up with one THOR directory rather than one per component.
///
/// Deliberately NOT next to the binaries. A store beside a `cargo build`
/// output lives in `target/`, and `cargo clean` is a command people run
/// without thinking twice about it - which would take the memory with it.
pub fn default_data_dir() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| Path::new(&h).join(".local").join("share")))?;
    Some(base.join("thor2"))
}

/// The user's home directory: `USERPROFILE` on Windows, then `HOME`. The one
/// thing every default path below is relative to.
fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(PathBuf::from).ok()
}

/// Claude Code's per-user settings file, where its `hooks` and `permissions`
/// live: `~/.claude/settings.json`.
///
/// WHY DEFAULTING THIS IS NOT THE "never guess a file I write to" violation it
/// looks like. That rule exists so this tool never rewrites the WRONG file.
/// This is not a guess: it is the single documented per-user location, the
/// same on every machine Claude Code runs on, and `install_hooks` only ever
/// appends to it and writes a `.bak` first. Requiring a newcomer to type a
/// path they have never needed to know was the actual reported blocker; a
/// well-known default that is printed before it is used removes it without
/// giving up the safety, which is the backup and the append-only writer, not
/// the absence of a default.
pub fn default_settings_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// Claude Code's per-user MCP config: `~/.claude.json`. A tool server written
/// under its top-level `mcpServers` object is available in EVERY project, which
/// is what a global THOR install wants. `install_tool_server` preserves every
/// other key in that (large, stateful) file and backs it up first.
///
/// Defaulted for the same reason as `default_settings_path`, and it matters
/// more here: leaving the MCP target unset gave a newcomer a memory the agent
/// could READ but never WRITE to - a THOR that quietly does half its job. The
/// working default makes writing-back the normal outcome instead of the one
/// you had to know to ask for.
pub fn default_user_mcp_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude.json"))
}

/// What happened to the store itself on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    Created,
    AlreadyThere,
}

/// Make sure a store exists at `db`, creating an empty one if it does not.
///
/// WHY THE INSTALLER HAS TO DO THIS. `EventStore::open_existing` refuses a
/// missing file on purpose, and `doctor` uses it - so on a brand new machine
/// the documented "check it works before you install anything" step failed
/// with "no THOR store at ...", which reads like a broken program rather than
/// like a first run. Everything that WRITES creates the store as a side
/// effect, so the store used to appear halfway through the first session,
/// after the health check had already said no. Creating it here puts the
/// steps back in an order a person can follow.
///
/// An existing store is opened by nobody and touched by nothing: this returns
/// early, so a re-run can never disturb a real memory.
pub fn ensure_store(db: &Path) -> anyhow::Result<StoreOutcome> {
    if db.exists() {
        return Ok(StoreOutcome::AlreadyThere);
    }
    if let Some(parent) = db.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    // `new` is the constructor that creates the file and lays down the
    // schema; `open_existing` is the one that never creates. See both doc
    // comments in `core::event_store`.
    EventStore::new(db)?;
    Ok(StoreOutcome::Created)
}

/// What happened to the response-guard rulebook on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulebookOutcome {
    Written,
    AlreadyThere,
}

#[derive(Debug, Clone)]
pub struct RulebookReport {
    pub outcome: RulebookOutcome,
    /// Resolved once here, via `serve::respond::default_rulebook_path`, so a
    /// caller never has to know that function exists just to print where the
    /// file went.
    pub path: PathBuf,
}

/// The rulebook this installer seeds when none exists yet: byte-identical to
/// the five rules shipped at the repository root as
/// `guard-response-rulebook.example.json` (a plain-language TLDR rule, a
/// no-disclaimers rule, an ask-before-checking rule, a length rule, and an
/// evidence rule) - baked in at compile time so a copied-out binary carries
/// it too, with no example file it has to find sitting next to it.
const RESPONSE_RULEBOOK_TEMPLATE: &str = include_str!("../../guard-response-rulebook.example.json");

/// Give a store a working Response Guard from its first session, without
/// ever touching a rulebook that is already there.
///
/// THE GAP THIS CLOSES. `respond::default_rulebook_path` always resolves to
/// one fixed place beside the store, and `install` is the one place that
/// already knows `db` before anything else runs - so where to put this is
/// never in question here. Without this function, the `Stop` hook
/// `standard_hooks` wires in is live from the very first run, but reads a
/// file that is not there: `respond::block_reason` and `respond::guard_verdict`
/// both fail open on a missing rulebook BY DESIGN (a guard that watches
/// replies must never itself become the reason a reply cannot be given - see
/// respond.rs's own doc comment), so it runs on every reply and blocks
/// nothing, silently, until someone finds the example file in the
/// repository and copies it over by hand. Measured gap: that is exactly what
/// a new install did, with nothing anywhere saying so.
///
/// Checked on EVERY run, the same as `ensure_store` - never only on a store
/// this run just created. A store from before this function existed has
/// exactly the same missing file and deserves the same fix the next time
/// `install` runs against it, not only on a brand new one.
///
/// An existing file - the owner's own tuned rulebook, or one an earlier
/// install already seeded - is never opened, merged or backed up: there is
/// nothing to back up FROM when nothing is being overwritten. Same stance as
/// `ensure_store` toward an existing store: only ever write into an absence.
/// This is also deliberately a NEUTRAL default, not the owner's own live
/// rulebook: a new user is meant to end up with working rules, never with
/// somebody else's wording imposed without being asked - see the seeded note
/// `walk-through-the-answer-guard-once` in `working_contract` for the other
/// half of that.
pub fn seed_response_rulebook(db: &Path) -> anyhow::Result<RulebookReport> {
    let path = respond::default_rulebook_path(db);
    if path.exists() {
        return Ok(RulebookReport { outcome: RulebookOutcome::AlreadyThere, path });
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(&path, RESPONSE_RULEBOOK_TEMPLATE)?;
    Ok(RulebookReport { outcome: RulebookOutcome::Written, path })
}

/// One hook this installer knows how to place: which event fires it, an
/// optional matcher (Claude Code's PreToolUse groups carry one; SessionStart
/// and UserPromptSubmit do not), and the exact command line to run.
#[derive(Debug, Clone)]
pub struct HookSpec {
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub command: String,
}

/// The three hooks the CONTRACT asks for: session start, before a tool call,
/// and per prompt - all three calling the same `serve hook --db <db>` command,
/// exactly like `serve/src/bin/serve.rs`'s own `hook` subcommand expects
/// (it tells the events apart by the JSON payload's `hook_event_name`, not by
/// which command line ran it).
///
/// The `--db` goes BEFORE the subcommand, because that is where `serve`'s CLI
/// puts it (`serve --db <DB> hook`, a global option on the parser, not on the
/// subcommand). This file had it the other way round until 2026-08-03 and was
/// caught the first time anyone actually RAN the command it writes: every hook
/// would have exited with "unexpected argument '--db'". Hooks fail open by
/// design, so nothing would have complained - the memory would simply never
/// have spoken again, which is this project's own worst failure class.
pub fn standard_hooks(serve_exe: &str, db: &str) -> Vec<HookSpec> {
    let command = format!("\"{serve_exe}\" --db \"{db}\" hook");
    vec![
        HookSpec { event: "SessionStart", matcher: None, command: command.clone() },
        HookSpec { event: "PreToolUse", matcher: Some("*"), command: command.clone() },
        HookSpec { event: "UserPromptSubmit", matcher: None, command: command.clone() },
        // Surface 5, the Response Guard. The `hook` command branches on the
        // payload's own `hook_event_name`, so the Stop hook runs the SAME
        // command - it is the payload, not the command line, that tells them
        // apart. Leaving this one out is exactly the regression that let a
        // whole session of untidy replies through (see serve::respond).
        HookSpec { event: "Stop", matcher: None, command },
    ]
}

/// What happened to one hook on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    Added,
    AlreadyPresent,
    /// An older hook of ours was pointing at a binary that is no longer on
    /// disk, and this run repointed it. See `stale_thor_command`.
    Replaced,
}

/// The executable a hook command calls, when the command has the shape this
/// installer writes: `"<exe>" --db "<db>" hook`.
fn exe_in_command(command: &str) -> Option<&str> {
    let rest = command.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// True iff `group` holds a command that calls a binary with the same FILE
/// NAME as `exe`, at a path that no longer exists.
///
/// THE DEFECT THIS CLOSES. Move the binaries and run this again, and the old
/// hooks stayed: the skip check compares the whole command line, so a
/// different path read as a different hook and the installer simply appended
/// a second one. The stale entry then calls a binary that is not there, and a
/// hook that cannot run FAILS OPEN - the agent carries on and the memory
/// never speaks again, with nothing reporting it. There is no uninstall
/// either, so nothing else would ever have taken it out.
///
/// Deliberately narrow on two counts. Only a binary with the SAME file name
/// is considered, so nobody else's hook is ever touched. And only one that is
/// GONE: a hook pointing at a binary that still exists is left exactly where
/// it is, because somebody running two stores on purpose must not silently
/// lose one of them.
fn stale_thor_command(group: &Value, exe: &str) -> Option<String> {
    let ours = Path::new(exe).file_name()?;
    for hook in group.get("hooks")?.as_array()? {
        if hook.get("type").and_then(Value::as_str) != Some("command") {
            continue;
        }
        let command = hook.get("command").and_then(Value::as_str)?;
        let Some(their_exe) = exe_in_command(command) else { continue };
        if Path::new(their_exe).file_name() == Some(ours) && !Path::new(their_exe).exists() {
            return Some(command.to_string());
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct InstallReport {
    pub results: Vec<(String, HookOutcome)>,
    pub backup_path: Option<std::path::PathBuf>,
    /// The commands this run repointed, each one an older hook of ours that
    /// was calling a binary no longer on disk. Reported so a move never
    /// happens silently.
    pub replaced: Vec<String>,
}

/// True iff `group` (one element of `hooks.<event>`) already contains a
/// `{"type":"command","command": command}` entry, regardless of its matcher.
/// Matching on the command line is deliberate: it is the one field that
/// identifies "this is THOR's hook" without also depending on how a matcher
/// happened to be phrased.
fn group_has_command(group: &Value, command: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("type").and_then(Value::as_str) == Some("command")
                    && h.get("command").and_then(Value::as_str) == Some(command)
            })
        })
        .unwrap_or(false)
}

fn new_group(spec: &HookSpec) -> Value {
    let mut group = json!({
        "hooks": [ { "type": "command", "command": spec.command } ]
    });
    if let Some(matcher) = spec.matcher {
        group["matcher"] = json!(matcher);
    }
    group
}

/// Install `specs` into the settings JSON at `path`.
///
/// - Missing file: starts from `{}` (a fresh install has nothing to preserve).
/// - Existing file that is not valid JSON, or not a JSON object, or whose
///   `hooks` key (or one `hooks.<event>` key) is not the shape this tool
///   expects: refused with a reason. Nothing is written.
/// - Otherwise: the file is backed up to `<path>.bak` first (copied verbatim,
///   before any parse result is acted on), then for each spec, its event's
///   array gets exactly one new group appended IF no existing group in that
///   array already carries the same command - every other group (anyone
///   else's hook, or THOR's own from an earlier install) is left byte-for-
///   byte as it was.
pub fn install_hooks(path: &Path, specs: &[HookSpec]) -> anyhow::Result<InstallReport> {
    let existed = path.exists();
    let raw = if existed { fs::read_to_string(path)? } else { "{}".to_string() };

    let mut root: Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid JSON ({e}) - refusing to touch it; fix the JSON first, or point --settings at a different file",
            path.display()
        )
    })?;
    anyhow::ensure!(
        root.is_object(),
        "{} does not contain a JSON object at the top level - refusing to touch it",
        path.display()
    );

    if root.get("hooks").is_some() {
        anyhow::ensure!(
            root["hooks"].is_object(),
            "{}'s \"hooks\" key is not a JSON object - refusing to touch a shape this tool does not recognise",
            path.display()
        );
    } else {
        root["hooks"] = json!({});
    }

    for spec in specs {
        if let Some(existing) = root["hooks"].get(spec.event) {
            anyhow::ensure!(
                existing.is_array(),
                "{}'s \"hooks.{}\" key is not a JSON array - refusing to touch a shape this tool does not recognise",
                path.display(),
                spec.event
            );
        }
    }

    let backup_path = if existed {
        let backup = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => format!("{ext}.bak"),
            None => "bak".to_string(),
        });
        fs::write(&backup, &raw)?;
        Some(backup)
    } else {
        None
    };

    let mut results = Vec::new();
    let mut replaced = Vec::new();
    for spec in specs {
        let array = root["hooks"]
            .as_object_mut()
            .unwrap()
            .entry(spec.event)
            .or_insert_with(|| json!([]));
        let array = array.as_array_mut().expect("checked above: this key is an array");

        if array.iter().any(|group| group_has_command(group, &spec.command)) {
            results.push((spec.event.to_string(), HookOutcome::AlreadyPresent));
            continue;
        }

        // Before appending, look for one of OURS that has gone stale: same
        // binary name, path that no longer exists. Repointing it beats
        // appending a second hook beside a dead one, which is what used to
        // happen after a move (see `stale_thor_command`).
        let ours = exe_in_command(&spec.command).unwrap_or_default().to_string();
        let mut repointed = None;
        for group in array.iter_mut() {
            let Some(old) = stale_thor_command(group, &ours) else { continue };
            if let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                for hook in hooks.iter_mut() {
                    if hook.get("command").and_then(Value::as_str) == Some(old.as_str()) {
                        hook["command"] = json!(spec.command);
                    }
                }
            }
            repointed = Some(old);
            break;
        }

        match repointed {
            Some(old) => {
                replaced.push(old);
                results.push((spec.event.to_string(), HookOutcome::Replaced));
            }
            None => {
                array.push(new_group(spec));
                results.push((spec.event.to_string(), HookOutcome::Added));
            }
        }
    }

    let pretty = serde_json::to_string_pretty(&root)?;
    fs::write(path, pretty + "\n")?;

    Ok(InstallReport { results, backup_path, replaced })
}

/// The notes a brand new memory starts with, and what happened to each.
#[derive(Debug, Clone)]
pub struct SeededItem {
    pub id: String,
    pub stored: bool,
    /// The gate's own words, when it refused this one.
    pub refusal: Option<String>,
}

/// The starting notes: how to write something down so it comes back.
///
/// WHY THESE LIVE IN THE MEMORY AND NOT IN A PAGE. An agent that stores notes
/// in a shape that never fires produces a memory which fails silently, and
/// neither side notices for weeks. A documentation page fixes that only for
/// someone who reads it, and a page can be skipped; what the memory hands over
/// at the start of every conversation cannot.
///
/// WHY THESE FOUR AND NOT MORE. Two of the four things a new writer gets wrong
/// are already caught by the gate, which refuses a rule with no binding and a
/// rule with no falsifier and says exactly what to fix. Seeding those would be
/// telling someone what they are about to be told anyway. These four are the
/// ones nothing can catch: a wrong anchor, a second copy, routing around a
/// refusal, and expecting words alone to forbid something. Each fails quietly.
///
/// They are ordinary notes. Unpin one and it stops arriving; rewrite it in your
/// own words; throw it out. Nothing here treats them as special afterwards.
pub fn working_contract() -> Vec<model::item::Item> {
    let rule = |id: &str, text: &str, falsifier: &str| model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: text.to_string(),
        bindings: vec![model::item::Binding::Always],
        severity: None,
        project: None,
        tags: vec!["working-contract".to_string()],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: None,
    };

    vec![
        rule(
            "anchor-what-the-fact-is-about",
            "Anchor a fact to the file or command it is really about, never to a path that merely \
             appears in the sentence. An anchor matching nothing fires nowhere, and nothing says \
             so: the fact is silent from the day it was written.",
            "A fact whose anchor names a path that is not there turns out to be served anyway.",
        ),
        rule(
            "correct-instead-of-duplicating",
            "Search before storing. When something changed, revise the item that already says it \
             rather than storing a second copy. Two items saying one thing take two of the few \
             places a block has, and the older one goes on being served.",
            "Storing a second copy of an existing fact turns out to serve a reader better than \
             revising the original.",
        ),
        model::item::Item {
            tags: vec![
                "working-contract".to_string(),
                format!(
                    "{}a forgotten setup step leaves no text behind to catch",
                    model::store::NO_LITERAL_REASON_PREFIX
                ),
            ],
            ..rule(
                "a-new-project-is-one-command",
                "Giving a folder its own memory is ONE command, run FROM the folder holding the                  code: `install --project <name>`. It writes the marker, reads the code once, and                  keeps that reading fresh on every commit. The owner names it - never you.",
                "A folder set up with that one command turns out to be missing its marker, its                  code reading or its refresh on commit.",
            )
        },
        model::item::Item {
            tags: vec![
                "working-contract".to_string(),
                format!(
                    "{}a life fact and a work fact are written in the same words",
                    model::store::NO_LITERAL_REASON_PREFIX
                ),
            ],
            ..rule(
                "two-lanes-work-and-life",
                "Two memories live here. Anything about the owner's own life - a recipe, a book, \
                 a training log, an expense - goes to the library with `shelve`, never \
                 `remember`. The work memory is for code and projects.",
                "A recipe stored with remember turns out to be found as easily as one filed \
                 with shelve.",
            )
        },
        model::item::Item {
            tags: vec![
                "working-contract".to_string(),
                format!(
                    "{}an invented name looks exactly like a real one, there is no text to catch",
                    model::store::NO_LITERAL_REASON_PREFIX
                ),
            ],
            ..rule(
                "never-open-a-place-the-owner-did-not-name",
                "Never invent a place to file something - not a scope, not a shelf, not a register \
                 key. Put it where something already lives, and if honestly nothing fits, ASK the \
                 owner what the new one should be called and use his word. A name you thought of \
                 is a name he will never look under.",
                "An agent opens a scope or a shelf under a name of its own and the owner finds it \
                 again later without being told where it went.",
            )
        },
        model::item::Item {
            tags: vec![
                "working-contract".to_string(),
                format!(
                    "{}a pile and one long thing are the same characters, only their subject differs",
                    model::store::NO_LITERAL_REASON_PREFIX
                ),
            ],
            ..rule(
                "one-entry-is-one-thing",
                "One entry is ONE thing. Given a schema AND an injury AND the equipment AND a \
                 measurement, file them as separate entries with labels, not as one block: an \
                 entry holding five things is found by one of them and corrected by none.",
                "A single entry holding several subjects turns out to be as findable and as \
                 correctable as the same content split into one entry per subject.",
            )
        },
        rule(
            "a-refusal-is-the-gate-working",
            "A refusal names the exact reason and what to do instead, and nothing is written when \
             it fires. Fix what it names. Never reword a fact just to get past it and never report \
             it as a bug: it is the one moment a bad entry can still be stopped.",
            "A refusal is found that names no reason and no fix, or that fires on an entry with \
             nothing wrong with it.",
        ),
        rule(
            "words-inform-a-proof-forbids",
            "A rule backed by words alone can inform, never forbid. Only a check that runs and \
             holds may refuse. contains proves the rule still describes this project; absent, \
             absent_all and forbidden refuse a write that introduces the forbidden text. Most \
             rules carry none, by design.",
            "A rule carrying no runnable check is found blocking a write.",
        ),
        // The fifth, and the only one here that is not about WRITING a fact.
        // It earned its place from the field, 2026-08-09: a session told to
        // report literally what it got, including when nothing happened, did
        // markedly better work than one left to summarise. Everything else in
        // this contract keeps the memory honest; this one keeps the report
        // honest, and a memory read through a flattering report is no better
        // than a wrong one.
        rule(
            "say-what-actually-happened",
            "Say what actually happened, including when nothing did. A step that fired nothing, a \
             check that found nothing, a number you could not get: that is a result and it belongs \
             in the report. Leaving it out reads as success, and the reader cannot tell the \
             difference.",
            "A report omits a step that produced nothing and the reader turns out not to be misled \
             by the omission.",
        ),
        // The sixth, added 2026-08-09, and the only one a newcomer meets as a
        // REFUSAL before they meet it as advice. Without it the very first
        // serious rule someone writes is turned away by a question they have
        // never been asked, which is the worst possible first impression of a
        // gate that is doing exactly the right thing.
        rule(
            "answer-whether-a-rule-can-refuse",
            "A rule you call expensive, or that names a command, flag or path, is asked one \
             question first: is there a text whose presence MEANS the mistake is happening? If \
             yes, add a check with that literal. If no, tag it no-literal:<why not> - the reason \
             is the answer. Both answers are fine; silence is not.",
            "A rule marked irreversible or costly is stored without ever being asked whether it \
             can refuse anything.",
        ),
        // Eleven more below, all bound Always like the ten above. Not about
        // WRITING a fact this time - these are the generic honesty,
        // agent-spawning and memory-hygiene lessons an agent needs on ANY
        // project, picked from the owner's own standing rules and rewritten
        // clean of his language, his business and his machines: nothing
        // here holds only because of who is running it or what it is
        // running on.
        rule(
            "never-fabricate-a-measured-value",
            "Never invent or fabricate a measured or tuned value - a benchmark, a timing, a \
             count. Flag it as unknown instead of writing a plausible guess down as if it were \
             real.",
            "A fabricated value sits in a report indistinguishable from a real one, and nothing \
             in the text marks it as guessed.",
        ),
        rule(
            "count-coverage-never-claim-it",
            "Never claim something is covered everywhere on the strength of memory. Count it \
             with something that walks the real source, and state the number. A remembered \
             impression is not a count.",
            "A coverage claim made from memory matches an exact count of the real source \
             closely enough that counting stops being worth the trouble.",
        ),
        rule(
            "verify-against-the-real-source",
            "Verify a requested check against the real file and line, the commit, or a freshly \
             fetched source, never a summary, a memory, or an assumption. Source unreadable: \
             say 'not checked', no verdict.",
            "A claim verified only against a summary or a memory turns out to match the real \
             source closely enough that the distinction stops mattering.",
        ),
        rule(
            "never-verify-with-the-changed-mechanism",
            "Never verify a change using the very mechanism the change itself touched - the \
             measurement would confirm itself. Verify on an independent path, or have someone \
             else read it against the source.",
            "A measurement taken through the changed mechanism reproduces independently often \
             enough that taking a second, independent path stops being worth it.",
        ),
        rule(
            "name-a-model-on-every-agent",
            "Split independent work across parallel agents, and name an explicit, \
             cheapest-fitting model on each one rather than letting it inherit the session's \
             model. Escalate only after a cheap model fails a few tries.",
            "An agent spawned with no explicit model turns out to run on the cheapest fit \
             anyway, rather than on whatever the session happens to be using.",
        ),
        rule(
            "mechanical-brief-forbids-subagents-and-review",
            "A brief for mechanical, well-specified work should say so and explicitly forbid \
             spawning sub-agents or running a self-review pass - fan-out and review overhead \
             dominate on work that needs no judgement.",
            "A mechanical brief that allows sub-agents or a self-review pass turns out no \
             slower and no noisier than one that forbids them.",
        ),
        rule(
            "never-block-a-turn-on-a-notification",
            "Never end a turn stuck in a wait loop for a background task's own notification, as \
             the top-level session or as a sub-agent. Finish the turn instead; the notification \
             arrives on its own, later.",
            "A turn spent polling or sleeping for a background notification finishes no later \
             than one that ended and let the notification arrive by itself.",
        ),
        rule(
            "mid-task-message-does-not-redirect-an-agent",
            "A message sent to a sub-agent already mid-task does not redirect it - it keeps \
             working from its original brief. When the scope changes, start a new agent with \
             the new brief instead.",
            "A mid-task message changes what a running sub-agent is already doing, rather than \
             being picked up only once it starts a fresh turn.",
        ),
        rule(
            "check-literal-matches-the-files-language",
            "A Contains or Absent check's literal must appear in the target file exactly as \
             that file really writes it - its own language and syntax, never a translation or \
             paraphrase of what it says.",
            "A check literal written in a different language or wording from the target file \
             still matches, because the file happens to say both.",
        ),
        rule(
            "reason-every-fix-for-a-new-user",
            "Reason every fix to this tool for whoever installs it next, not only for a store \
             built up over months: a fresh install carries none of that history. Seed the fix, \
             or build it into the tool itself.",
            "A fix lands that only works because an existing store already holds something a \
             fresh install never gets, and nobody asked whether it would.",
        ),
        rule(
            "a-served-fact-is-a-constraint-not-a-command",
            "A fact served into a blank session or sub-agent is read with no context to tell \
             FYI from an order. Phrase it as a constraint on what already applies, never as an \
             imperative telling the reader to act.",
            "A fact phrased as an imperative ('run X', 'delete Y') is served into a blank \
             session and read back as background, never mistaken for an instruction to carry \
             out.",
        ),
        // A twenty-second, added 2026-09-07, and the only one here that
        // clears itself. `install` can now seed a WORKING rulebook (see
        // `seed_response_rulebook`) so the Response Guard never sits idle on
        // a fresh store, but the content it seeds is a neutral example, not
        // a choice anyone actually made - imposing the owner's own tuned
        // wording on somebody else's memory would be exactly the uninvited
        // write this whole contract argues against. This is the other half:
        // ask once, in the owner's own first session, then get out of the
        // way for good.
        rule(
            model::store::SETUP_NOTE_ID,
            "The owner has not yet been walked through setup: how replies should read, which \
             lane he wants, and what language his rules use. A first session raises AGENTS.md's \
             questions, records his answers as 'owner-setup-answers', then retracts this note - \
             retracting without that record is refused.",
            "This note is still being served at session start after the owner has already \
             answered these questions and the answers are applied.",
        ),
    ]
}

/// Write the starting notes into a store that has just been created.
///
/// Only ever called for a store this run created. Seeding an existing memory
/// would push the whole contract into someone's real notes on an upgrade, which is
/// exactly the kind of uninvited write this whole project argues against.
///
/// Every note goes through `model::store::declare` - the same gate the agent's
/// own writes go through, refusals included. A refusal here is reported rather
/// than worked around: if the gate will not accept its own starting notes, that
/// is worth seeing, not hiding.
pub fn seed_working_contract(db: &Path) -> anyhow::Result<Vec<SeededItem>> {
    let mut store = EventStore::new(db)?;
    let mut out = Vec::new();
    for item in working_contract() {
        let id = item.id.clone();
        match model::store::declare(&mut store, "install", "install", "installer", &item) {
            Ok(_) => out.push(SeededItem { id, stored: true, refusal: None }),
            Err(e) => out.push(SeededItem { id, stored: false, refusal: Some(e.to_string()) }),
        }
    }
    Ok(out)
}

/// The combined outcome of `ensure_and_seed_store`: what happened to the
/// store itself, the starting notes (empty when the store already existed),
/// and the response-guard rulebook beside it.
#[derive(Debug)]
pub struct BootstrapReport {
    pub store: StoreOutcome,
    pub seeded: Vec<SeededItem>,
    pub rulebook: RulebookReport,
}

/// Bring a store from nothing to one that answers - the exact sequence
/// `install`'s own CLI runs by hand (`ensure_store`, then only on a store
/// this call created `seed_working_contract`, then `seed_response_rulebook`
/// on every call regardless) - collapsed into one call for a caller with no
/// settings.json to write into and no business going near one.
///
/// WHY THIS IS SEPARATE FROM `install`. `install`'s CLI always resolves a
/// settings.json (defaulting under HOME/USERPROFILE when none is named) and
/// refuses outright when the `serve` binary it would wire hooks to is not
/// there - both wrong for a container entry point, which ships neither
/// `serve` nor an agent's configuration to touch. This function is the store
/// half alone: no hooks, no tool-server registration, no project marker.
/// See `ops/src/bin/initstore.rs`, the binary built on exactly this.
pub fn ensure_and_seed_store(db: &Path) -> anyhow::Result<BootstrapReport> {
    let store = ensure_store(db)?;
    let seeded = match store {
        StoreOutcome::Created => seed_working_contract(db)?,
        StoreOutcome::AlreadyThere => Vec::new(),
    };
    let rulebook = seed_response_rulebook(db)?;
    Ok(BootstrapReport { store, seeded, rulebook })
}

/// What happened to the tool-server registration on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOutcome {
    Added,
    AlreadyPresent,
    Replaced,
}

#[derive(Debug, Clone)]
pub struct ServerReport {
    pub outcome: ServerOutcome,
    /// The command the previous entry under this name pointed at, when this
    /// run replaced one. Reported so an upgrade never happens silently.
    pub replaced: Option<String>,
    pub backup_path: Option<std::path::PathBuf>,
}

/// Register the tool server an agent calls to WRITE to its memory, under
/// `mcpServers.<name>` in `path`.
///
/// The hooks alone are a one-way street: they hand facts to the agent and can
/// stop a wrong write, but nothing the agent decides ever gets back into the
/// memory. That is why this is part of installing rather than a follow-up
/// step someone reads about later and skips.
///
/// Same non-negotiables as `install_hooks`, for the same reasons: back up
/// first, refuse a file that is not valid JSON rather than starting a fresh
/// one over it, and never touch an entry this tool did not put there.
///
/// One deliberate difference. Hooks live in a LIST, so a foreign hook is
/// simply appended past. A server lives under a NAME, and there can only be
/// one `thor`. An existing entry that differs is therefore REPLACED, and the
/// command it used to point at comes back in the report. Refusing instead
/// would make an upgrade impossible - the 1.0 entry would stay registered and
/// the person would believe 2.0 was installed - and replacing it quietly
/// would be worse still.
pub fn install_tool_server(
    path: &Path,
    name: &str,
    exe: &str,
    args: &[String],
) -> anyhow::Result<ServerReport> {
    let existed = path.exists();
    let raw = if existed { fs::read_to_string(path)? } else { "{}".to_string() };

    let mut root: Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid JSON ({e}) - refusing to touch it; fix the JSON first, or point --mcp-json at a different file",
            path.display()
        )
    })?;
    anyhow::ensure!(
        root.is_object(),
        "{} does not contain a JSON object at the top level - refusing to touch it",
        path.display()
    );

    if root.get("mcpServers").is_some() {
        anyhow::ensure!(
            root["mcpServers"].is_object(),
            "{}'s \"mcpServers\" key is not a JSON object - refusing to touch a shape this tool does not recognise",
            path.display()
        );
    } else {
        root["mcpServers"] = json!({});
    }

    let entry = json!({
        "type": "stdio",
        "command": exe,
        "args": args,
    });

    let previous = root["mcpServers"].get(name).cloned();
    let outcome = match &previous {
        Some(old) if *old == entry => ServerOutcome::AlreadyPresent,
        Some(_) => ServerOutcome::Replaced,
        None => ServerOutcome::Added,
    };

    // Nothing to write, so nothing to back up either: a re-run on an already
    // correct file leaves it byte-for-byte alone, including its formatting.
    if outcome == ServerOutcome::AlreadyPresent {
        return Ok(ServerReport { outcome, replaced: None, backup_path: None });
    }

    let backup_path = if existed {
        let backup = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => format!("{ext}.bak"),
            None => "bak".to_string(),
        });
        fs::write(&backup, &raw)?;
        Some(backup)
    } else {
        None
    };

    root["mcpServers"]
        .as_object_mut()
        .expect("checked above: this key is an object")
        .insert(name.to_string(), entry);

    let pretty = serde_json::to_string_pretty(&root)?;
    fs::write(path, pretty + "\n")?;

    let replaced = previous
        .as_ref()
        .and_then(|p| p.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|_| outcome == ServerOutcome::Replaced);

    Ok(ServerReport { outcome, replaced, backup_path })
}

/// What happened to the project marker on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerOutcome {
    Written,
    AlreadyThere,
}

/// Write the marker that gives a folder its own memory scope.
///
/// Without one, everything a person stores while working in that folder lands
/// in the shared memory every project sees, and search there competes with
/// projects they were not asking about. THOR gets worse the more it is used,
/// which is the opposite of the promise.
///
/// A marker that already names a DIFFERENT key is refused. Re-scoping a
/// project silently would strand every fact already filed under the old key:
/// they stay in the store, they simply stop being served here, and nothing
/// says so.
pub fn write_project_marker(dir: &Path, key: &str) -> anyhow::Result<MarkerOutcome> {
    let key = key.trim();
    anyhow::ensure!(!key.is_empty(), "a project key cannot be blank");
    anyhow::ensure!(
        !key.contains('\n') && !key.contains('\r'),
        "a project key is one line, and {key:?} is not"
    );

    let marker = dir.join(serve::project::MARKER_FILE_NAME);
    if marker.exists() {
        let current = fs::read_to_string(&marker)?;
        let current = current.trim();
        anyhow::ensure!(
            current == key,
            "{} already scopes this folder to {current:?} - refusing to re-scope it to {key:?}, \
             which would strand every fact already filed under {current:?}",
            marker.display()
        );
        return Ok(MarkerOutcome::AlreadyThere);
    }

    fs::write(&marker, format!("{key}\n"))?;
    Ok(MarkerOutcome::Written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> Vec<HookSpec> {
        standard_hooks("C:\\fake\\serve.exe", "C:\\fake\\thor.db")
    }

    /// The defect this guards against, and it is the worst kind this project
    /// has: a hook command that is written correctly-looking but does not
    /// parse. `serve` takes `--db` as a GLOBAL option, before the subcommand.
    /// With it after, every hook exits 1 with "unexpected argument '--db'" -
    /// and hooks fail open, so nothing complains and the memory simply stops
    /// speaking. Caught only by running the command this function writes.
    #[test]
    fn the_command_puts_db_before_the_subcommand() {
        let specs = standard_hooks("C:\\thor2\\bin\\serve.exe", "C:\\thor2\\thor.db");
        for spec in &specs {
            assert_eq!(
                spec.command, "\"C:\\thor2\\bin\\serve.exe\" --db \"C:\\thor2\\thor.db\" hook",
                "the {} hook must call serve the way serve's own CLI parses it",
                spec.event
            );
            let db_at = spec.command.find("--db").expect("the command must name --db");
            let sub_at = spec.command.find(" hook").expect("the command must call the hook subcommand");
            assert!(db_at < sub_at, "--db is a global option, so it comes first: {}", spec.command);
        }
    }

    #[test]
    fn a_fresh_settings_file_gets_all_four_hooks_added() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let report = install_hooks(&path, &specs()).unwrap();
        assert_eq!(report.results.len(), 4);
        assert!(report.results.iter().all(|(_, o)| *o == HookOutcome::Added));
        assert!(report.backup_path.is_none(), "nothing existed yet, so there is nothing to back up");

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["UserPromptSubmit"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["PreToolUse"][0]["matcher"], json!("*"));
        assert!(written["hooks"]["SessionStart"][0].get("matcher").is_none());
        // Surface 5: the Response Guard on the Stop hook - the one that was
        // missing and let a whole session of untidy replies through.
        assert_eq!(written["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    /// The defect this guards against: a naive installer appends its group
    /// unconditionally, so running `install` twice (a re-run, a second agent
    /// setup pass) leaves TWO identical hook entries firing the same command
    /// on every session start.
    #[test]
    fn a_second_install_adds_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_hooks(&path, &specs()).unwrap();
        let second = install_hooks(&path, &specs()).unwrap();

        assert!(
            second.results.iter().all(|(_, o)| *o == HookOutcome::AlreadyPresent),
            "the second run must recognise every hook as already installed: {:?}",
            second.results
        );
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for event in ["SessionStart", "PreToolUse", "UserPromptSubmit", "Stop"] {
            assert_eq!(
                written["hooks"][event].as_array().unwrap().len(),
                1,
                "event {event} must still carry exactly one group after a second install"
            );
        }
    }

    /// The other defect this guards against: an installer that "cleans up"
    /// or rewrites the hooks array wholesale would silently delete another
    /// tool's hook (or an earlier hand-written one) the moment it runs.
    #[test]
    fn install_leaves_a_foreign_hook_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": [
                        { "hooks": [ { "type": "command", "command": "\"C:\\\\some\\\\other-tool.exe\" backup" } ] }
                    ]
                },
                "permissions": { "allow": ["Bash(ls:*)"] }
            }))
            .unwrap(),
        )
        .unwrap();

        let report = install_hooks(&path, &specs()).unwrap();
        assert_eq!(
            report.results.iter().find(|(e, _)| e == "SessionStart").unwrap().1,
            HookOutcome::Added,
            "our own SessionStart hook must still be added alongside the foreign one"
        );

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = written["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 2, "the foreign hook must survive, plus our own");
        assert!(
            session_start.iter().any(|g| group_has_command(g, "\"C:\\\\some\\\\other-tool.exe\" backup")),
            "the pre-existing foreign hook must be byte-identical, not merged or rewritten"
        );
        // Unrelated top-level keys must survive completely untouched too.
        assert_eq!(written["permissions"]["allow"][0], json!("Bash(ls:*)"));
    }

    /// Refusal test: invalid JSON must never be silently replaced with a
    /// fresh `{}` (that would look like a successful install while quietly
    /// discarding every setting the file used to hold).
    #[test]
    fn install_refuses_invalid_json_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = "{ this is not valid json ,,, ";
        fs::write(&path, original).unwrap();

        let err = install_hooks(&path, &specs());
        assert!(err.is_err(), "invalid JSON must be refused, not repaired or replaced");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "a refused install must leave the file exactly as it was"
        );
    }

    #[test]
    fn install_refuses_a_non_object_hooks_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, serde_json::to_string(&json!({ "hooks": "not-an-object" })).unwrap()).unwrap();
        let err = install_hooks(&path, &specs());
        assert!(err.is_err(), "an unrecognised \"hooks\" shape must be refused, not overwritten");
    }

    #[test]
    fn install_backs_up_the_original_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = serde_json::to_string_pretty(&json!({ "hooks": {}, "marker": "original" })).unwrap();
        fs::write(&path, &original).unwrap();

        let report = install_hooks(&path, &specs()).unwrap();
        let backup = report.backup_path.expect("an existing file must be backed up");
        assert_eq!(fs::read_to_string(&backup).unwrap(), original, "the backup must hold the pre-install content");
    }

    /// The defect this guards against, and it is this project's worst class:
    /// move the binaries, run install again, and the old hooks stayed. They
    /// call a binary that is gone, and a hook that cannot run fails OPEN - so
    /// the agent carries on and the memory simply never speaks again, with
    /// nothing reporting it and no uninstall to clean it up.
    #[test]
    fn a_hook_whose_binary_is_gone_is_repointed_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A real binary at the new location, so only the OLD one is missing.
        let new_exe = dir.path().join("serve.exe");
        fs::write(&new_exe, b"not really a binary, but it exists").unwrap();
        // The GONE binary's path uses THIS platform's own separators, never a
        // Windows literal. `stale_thor_command` decides staleness with
        // `Path::file_name`, and on Linux a backslash is an ordinary character:
        // "C:\gone\serve.exe" is then ONE filename component, not
        // ".../serve.exe", so it never matched our file name and the repoint
        // silently did not fire. That made this test pass on Windows and fail on
        // the Linux CI runner - caught the first time a v2 tag actually built,
        // on 2026-08-14.
        let gone = dir.path().join("gone");
        let old_command =
            format!("\"{}\" --db \"{}\" hook", gone.join("serve.exe").display(), gone.join("thor.db").display());
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": old_command } ] } ] }
            }))
            .unwrap(),
        )
        .unwrap();

        let specs =
            standard_hooks(&new_exe.display().to_string(), &dir.path().join("thor.db").display().to_string());
        let report = install_hooks(&path, &specs).unwrap();

        assert_eq!(
            report.results.iter().find(|(e, _)| e == "SessionStart").unwrap().1,
            HookOutcome::Replaced
        );
        assert!(
            report.replaced.iter().any(|c| c == &old_command),
            "the replacement must be reported, or the move happened silently: {:?}",
            report.replaced
        );
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let groups = written["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "it must be repointed in place, never appended beside the dead one");
        assert!(!format!("{groups:?}").contains("gone"), "the dead path must be gone: {groups:?}");
    }

    /// The other half, and the reason this is narrow: somebody running two
    /// stores on purpose must not silently lose one. A hook whose binary is
    /// still THERE is left exactly as it is.
    #[test]
    fn a_hook_whose_binary_still_exists_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let other_exe = dir.path().join("serve.exe");
        fs::write(&other_exe, b"exists").unwrap();
        let their_command = format!("\"{}\" --db \"C:\\\\other\\\\thor.db\" hook", other_exe.display());
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": their_command } ] } ] }
            }))
            .unwrap(),
        )
        .unwrap();

        let specs = standard_hooks(&other_exe.display().to_string(), "C:\\mine\\thor.db");
        let report = install_hooks(&path, &specs).unwrap();

        assert_eq!(report.results.iter().find(|(e, _)| e == "SessionStart").unwrap().1, HookOutcome::Added);
        assert!(report.replaced.is_empty(), "nothing was dead, so nothing may be replaced");
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["hooks"]["SessionStart"].as_array().unwrap().len(),
            2,
            "both stores keep their own hook"
        );
    }

    /// A foreign tool's hook is never touched, dead or alive: the file name
    /// has to match ours before anything is considered.
    #[test]
    fn a_foreign_hook_with_a_dead_path_is_still_not_ours_to_repoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let new_exe = dir.path().join("serve.exe");
        fs::write(&new_exe, b"exists").unwrap();
        let foreign = "\"C:\\gone\\other-tool.exe\" backup";
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": foreign } ] } ] }
            }))
            .unwrap(),
        )
        .unwrap();

        let report = install_hooks(&path, &standard_hooks(&new_exe.display().to_string(), "db")).unwrap();
        assert!(report.replaced.is_empty());
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            format!("{:?}", written["hooks"]["SessionStart"]).contains("other-tool.exe"),
            "somebody else's hook must survive untouched"
        );
    }

    fn server_args(db: &str) -> Vec<String> {
        vec!["--db".to_string(), db.to_string()]
    }

    /// The defect this guards against: `doctor` refuses a missing store on
    /// purpose, so the documented "check it before you install anything" step
    /// used to fail on every brand new machine. The installer creates one, and
    /// the store it creates has to be a real openable store, not a blank file.
    #[test]
    fn ensure_store_creates_one_that_actually_opens() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("fresh").join("thor.db");
        assert_eq!(ensure_store(&db).unwrap(), StoreOutcome::Created);
        assert!(db.exists(), "the store file must be there afterwards");
        thor_core::event_store::EventStore::open_existing(&db)
            .expect("the store the installer creates must open with the constructor that never creates");
    }

    /// The defect this guards against: a re-run that "makes sure" the store is
    /// there and quietly replaces a real memory with an empty one.
    #[test]
    fn ensure_store_never_touches_an_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let before = fs::read(&db).unwrap();

        assert_eq!(ensure_store(&db).unwrap(), StoreOutcome::AlreadyThere);
        assert_eq!(fs::read(&db).unwrap(), before, "a second run must not write a single byte");
    }

    #[test]
    fn a_fresh_file_gets_the_tool_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        let report =
            install_tool_server(&path, "thor", "C:\\thor2\\bin\\mcp.exe", &server_args("C:\\thor2\\thor.db")).unwrap();

        assert_eq!(report.outcome, ServerOutcome::Added);
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["thor"]["command"], json!("C:\\thor2\\bin\\mcp.exe"));
        assert_eq!(written["mcpServers"]["thor"]["type"], json!("stdio"));
        assert_eq!(written["mcpServers"]["thor"]["args"][1], json!("C:\\thor2\\thor.db"));
    }

    /// The defect this guards against: a re-run that rewrites an already
    /// correct file, which churns its formatting and writes a `.bak` that says
    /// something changed when nothing did.
    #[test]
    fn a_second_registration_leaves_the_file_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        install_tool_server(&path, "thor", "mcp.exe", &server_args("thor.db")).unwrap();
        let after_first = fs::read(&path).unwrap();

        let second = install_tool_server(&path, "thor", "mcp.exe", &server_args("thor.db")).unwrap();
        assert_eq!(second.outcome, ServerOutcome::AlreadyPresent);
        assert!(second.backup_path.is_none(), "nothing changed, so nothing may be backed up");
        assert_eq!(fs::read(&path).unwrap(), after_first, "an unchanged run must not rewrite the file");
    }

    /// The upgrade case, and the reason this one replaces rather than refuses:
    /// a 1.0 entry left in place would keep answering while the person believes
    /// 2.0 is installed. Replacing is right; replacing SILENTLY is not, so the
    /// old command has to come back in the report.
    #[test]
    fn an_older_registration_is_replaced_and_the_old_command_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "mcpServers": { "thor": { "command": "C:\\old\\thor.exe", "args": ["mcp"] } }
            }))
            .unwrap(),
        )
        .unwrap();

        let report = install_tool_server(&path, "thor", "C:\\thor2\\bin\\mcp.exe", &server_args("thor.db")).unwrap();
        assert_eq!(report.outcome, ServerOutcome::Replaced);
        assert_eq!(
            report.replaced.as_deref(),
            Some("C:\\old\\thor.exe"),
            "the report must name what it replaced, or the upgrade happened silently"
        );
        assert!(report.backup_path.is_some(), "replacing an entry must back the file up first");
    }

    /// The defect this guards against: an installer that writes its own
    /// `mcpServers` object wholesale deletes every other tool the person had
    /// registered, and they find out one at a time over the following days.
    #[test]
    fn registration_leaves_a_foreign_server_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "mcpServers": { "something-else": { "command": "other.exe", "args": ["run"] } },
                "unrelated": { "kept": true }
            }))
            .unwrap(),
        )
        .unwrap();

        install_tool_server(&path, "thor", "mcp.exe", &server_args("thor.db")).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["something-else"]["command"], json!("other.exe"));
        assert_eq!(written["mcpServers"]["something-else"]["args"][0], json!("run"));
        assert_eq!(written["unrelated"]["kept"], json!(true));
        assert_eq!(written["mcpServers"]["thor"]["command"], json!("mcp.exe"));
    }

    #[test]
    fn registration_refuses_invalid_json_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        let original = "{ not json at all ,,,";
        fs::write(&path, original).unwrap();

        assert!(install_tool_server(&path, "thor", "mcp.exe", &server_args("thor.db")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original, "a refused run must leave the file exactly as it was");
    }

    #[test]
    fn the_marker_writes_the_key_and_a_rerun_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(write_project_marker(dir.path(), "my-project").unwrap(), MarkerOutcome::Written);
        let marker = dir.path().join(serve::project::MARKER_FILE_NAME);
        assert_eq!(fs::read_to_string(&marker).unwrap().trim(), "my-project");
        assert_eq!(write_project_marker(dir.path(), "my-project").unwrap(), MarkerOutcome::AlreadyThere);
    }

    /// The defect this guards against: re-scoping a folder strands every fact
    /// already filed under the old key. They stay in the store and simply stop
    /// being served here, which is invisible from every surface.
    #[test]
    fn the_marker_refuses_to_rescope_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        write_project_marker(dir.path(), "first-key").unwrap();

        let err = write_project_marker(dir.path(), "second-key").expect_err("re-scoping must be refused");
        let msg = err.to_string();
        assert!(msg.contains("first-key"), "the refusal must name the key already there: {msg}");
        assert!(msg.contains("second-key"), "and the one that was asked for: {msg}");
        assert_eq!(
            fs::read_to_string(dir.path().join(serve::project::MARKER_FILE_NAME)).unwrap().trim(),
            "first-key",
            "a refused re-scope must leave the marker alone"
        );
    }

    #[test]
    fn the_marker_refuses_a_blank_key() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_project_marker(dir.path(), "   ").is_err());
        assert!(!dir.path().join(serve::project::MARKER_FILE_NAME).exists());
    }

    /// The contradiction this guards against, and it would be an embarrassing
    /// one: a memory whose own write gate refuses the starting notes it ships
    /// with. Every refusal here is a real refusal, so this test is the gate
    /// judging its own contract.
    #[test]
    fn the_gate_accepts_every_starting_note() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();

        let seeded = seed_working_contract(&db).unwrap();
        assert_eq!(seeded.len(), working_contract().len());
        for s in &seeded {
            assert!(s.stored, "the gate refused its own starting note {}: {:?}", s.id, s.refusal);
        }
    }

    /// The defect this guards against: a starting note that is stored but
    /// bound to nothing fires nowhere, so a new user gets an empty session
    /// start and no sign that anything was seeded at all.
    #[test]
    fn every_starting_note_arrives_at_session_start() {
        for item in working_contract() {
            assert!(
                item.bindings.contains(&model::item::Binding::Always),
                "{} must be pinned, or it is seeded and then never seen",
                item.id
            );
            assert!(item.falsifier.is_some(), "{} must name what would prove it wrong", item.id);
            assert!(
                item.text.chars().count() <= 300,
                "{} is {} characters, over the limit the gate enforces",
                item.id,
                item.text.chars().count()
            );
        }
    }

    /// The defect this guards against: seeding on every run, which would push
    /// the starting notes into a real memory on an upgrade. The CLI only calls this
    /// for a store it just created; this proves the second call is refused
    /// rather than quietly doubling everything up.
    #[test]
    fn seeding_twice_stores_nothing_the_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        seed_working_contract(&db).unwrap();

        let again = seed_working_contract(&db).unwrap();
        for s in &again {
            assert!(!s.stored, "{} was stored a second time - the memory now holds it twice", s.id);
        }
    }

    /// The defect `ensure_and_seed_store` exists to fix: a container's entry
    /// point calling `mcp` straight against an empty volume got "no THOR
    /// store at ... this command never creates one" and nothing that speaks
    /// the tool protocol. This is the whole bootstrap in one call, proving it
    /// leaves a store with the full working contract (22 notes, matching
    /// `working_contract().len()`) and the response-guard rulebook, not the
    /// bare file `ensure_store` alone would leave.
    #[test]
    fn ensure_and_seed_store_creates_22_notes_and_the_rulebook_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");

        let report = ensure_and_seed_store(&db).unwrap();

        assert_eq!(report.store, StoreOutcome::Created);
        assert_eq!(working_contract().len(), 22, "this test's own claim of 22 starting notes is now stale");
        let stored = report.seeded.iter().filter(|s| s.stored).count();
        assert_eq!(stored, 22, "expected all 22 starting notes to be stored, got {stored}");
        assert_eq!(report.rulebook.outcome, RulebookOutcome::Written);
        assert!(report.rulebook.path.exists(), "the rulebook path reported back must be the one actually written");
    }

    /// The other half of the same defect: a second start against a volume
    /// that already holds a real memory (this container's own second run, or
    /// a volume someone pointed at their existing store) must never reseed it
    /// or touch the rulebook again - see `ensure_store` and
    /// `seed_response_rulebook`'s own "only ever write into an absence"
    /// stance, which this call inherits.
    #[test]
    fn ensure_and_seed_store_leaves_an_existing_store_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_and_seed_store(&db).unwrap();
        let before = fs::read(&db).unwrap();
        let rulebook_path = respond::default_rulebook_path(&db);
        let rulebook_before = fs::read(&rulebook_path).unwrap();

        let second = ensure_and_seed_store(&db).unwrap();

        assert_eq!(second.store, StoreOutcome::AlreadyThere);
        assert!(second.seeded.is_empty(), "a store that already existed must not be seeded again");
        assert_eq!(second.rulebook.outcome, RulebookOutcome::AlreadyThere);
        assert_eq!(fs::read(&db).unwrap(), before, "a second call must not write a single byte to the store");
        assert_eq!(fs::read(&rulebook_path).unwrap(), rulebook_before, "nor to the rulebook beside it");
    }

    /// The new starter note rides the same three tests above (they iterate
    /// `working_contract()` generically), plus this one, targeted: its text
    /// was deliberately kept free of a backtick, a `--flag`, or a path, so it
    /// needs no `no-literal:` tag to pass Ground 11 (`model::gate::declare`'s
    /// "was this rule ever asked whether it can refuse" check). Asserting
    /// its tags are exactly `working-contract` proves that by construction,
    /// not by hoping the gate happens to agree.
    #[test]
    fn the_answer_guard_note_is_present_and_carries_no_literal_tag() {
        let items = working_contract();
        let note = items
            .iter()
            .find(|i| i.id == model::store::SETUP_NOTE_ID)
            .expect("the first-session answer-guard note must be part of the working contract");
        assert_eq!(note.kind, model::item::Kind::Rule);
        assert!(note.bindings.contains(&model::item::Binding::Always));
        assert!(note.falsifier.is_some());
        assert_eq!(
            note.tags,
            vec!["working-contract".to_string()],
            "a text with no backtick, flag or path needs no no-literal tag"
        );
    }

    /// THE GAP THIS CLOSES: a fresh store got the `Stop` hook wired in from
    /// the very first run, but no rulebook for it to read - `respond::block_reason`
    /// fails open on a missing file BY DESIGN, so the guard ran on every
    /// reply and blocked nothing, silently, until someone found the example
    /// file in the repository and copied it over by hand.
    #[test]
    fn seed_response_rulebook_writes_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();

        let report = seed_response_rulebook(&db).unwrap();
        assert_eq!(report.outcome, RulebookOutcome::Written);
        assert_eq!(report.path, respond::default_rulebook_path(&db));
        assert!(report.path.exists());
        assert_eq!(fs::read_to_string(&report.path).unwrap(), RESPONSE_RULEBOOK_TEMPLATE);
    }

    /// The defect this guards against: an installer that "helpfully"
    /// refreshes the rulebook on every run would silently overwrite an
    /// owner's own tuned rules the next time a binary gets rebuilt.
    #[test]
    fn seed_response_rulebook_leaves_an_existing_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let path = respond::default_rulebook_path(&db);
        let owners_own = r#"[{"id":"owners-own-rule","any_of":["x"],"reminder":"mine"}]"#;
        fs::write(&path, owners_own).unwrap();

        let report = seed_response_rulebook(&db).unwrap();
        assert_eq!(report.outcome, RulebookOutcome::AlreadyThere);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            owners_own,
            "an existing rulebook must never be overwritten, seeded or not"
        );
    }

    /// Required to actually prove the seeded file is usable, not merely
    /// present: parsed with the guard's own type (`respond::parse_opt_in_rules`),
    /// never hand-checked as JSON, so a shape only respond.rs itself would
    /// reject is caught here too.
    #[test]
    fn the_seeded_rulebook_parses_with_the_guards_own_rule_type() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let report = seed_response_rulebook(&db).unwrap();

        let text = fs::read_to_string(&report.path).unwrap();
        let rules = respond::parse_opt_in_rules(&text);
        assert_eq!(rules.len(), 5, "the five shipped rules must all parse");

        let ids: Vec<&str> = rules.iter().map(|r| r.base.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "no-plain-language-tldr",
                "ask-user-to-check-or-fetch",
                "no-reflexive-disclaimers",
                "answer-is-too-long",
                "checked-claim-needs-evidence",
            ]
        );

        let tldr = rules.iter().find(|r| r.base.id == "no-plain-language-tldr").unwrap();
        assert_eq!(tldr.base.min_chars, 600);
        assert!(tldr.base.any_of.contains(&"gepusht".to_string()));
        assert!(tldr.base.none_of.contains(&"tldr".to_string()));

        let length_rule = rules.iter().find(|r| r.base.id == "answer-is-too-long").unwrap();
        assert_eq!(length_rule.base.min_chars, 1000);
        assert!(
            !length_rule.list_request_any_of.is_empty(),
            "the length rule must keep its list exemption"
        );

        let evidence = rules.iter().find(|r| r.base.id == "checked-claim-needs-evidence").unwrap();
        assert!(evidence.none_of_patterns.contains(&"commit_sha".to_string()));
        assert!(evidence.none_of_patterns.contains(&"path_line".to_string()));
        assert!(!evidence.base.reminder.is_empty());
    }
}
