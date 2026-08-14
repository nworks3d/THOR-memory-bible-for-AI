//! The MCP surface: the door an agent uses to read and write THOR 2.0's
//! memory directly, over stdio (`rmcp`, mirroring the running 1.0 server's
//! own recipe in `thor/src/mcp.rs` - same crate, same stdio transport, a much
//! smaller tool set because 2.0 has a much smaller model: five kinds and one
//! write gate instead of one kind doing three jobs).
//!
//! Fourteen tools, and deliberately no more. Six keep a fact honest, two make
//! or unmake a standing rule, three read, three answer questions about the
//! code:
//!
//!   remember  declare a NEW item, through `model::store::declare` - the
//!             write gate's refusal grounds apply unconditionally, and a
//!             refusal comes back as a loud, reasoned reply, never silence
//!             and never a partial write.
//!   revise    correct an EXISTING item, through `model::store::revise` -
//!             the same gate, plus its own field-preservation check (no
//!             field the item already carried may silently vanish).
//!   retract   take a wrong item OUT (`model::store::retract`). A reason is
//!             required. Nothing is deleted: the log keeps every step.
//!   resolve   settle an entity with more than one head
//!             (`model::store::resolve`), the state `get` calls DIVERGED.
//!   pin       add the Always binding (`model::store::revise`) so an item is
//!             served in full at every session start - idempotent, and
//!             refused (loudly) when this kind may carry no binding at all.
//!   unpin     remove the Always binding (`model::store::revise`) -
//!             idempotent, and refused (loudly) when that would leave the
//!             item with no binding left to ever fire on.
//!   get       show one item whole, by id, through `model::store::show`.
//!   history   walk one item's whole life (`model::store::history`), oldest
//!             first, retractions included.
//!   lookup    search - every project, archive kinds included, exactly the
//!             doctrine `serve::lookup` documents for surface 4 (OPZOEKEN);
//!             also the only door onto a `Lookup` item's own key.
//!   search_code  substring search over the indexed SOURCE
//!             (`serve::lookup::search_code`).
//!   where_used  who defines and who uses one symbol name
//!             (`serve::lookup::where_used`) - the blast radius of a change.
//!   outline   what one file declares, in line order
//!             (`serve::lookup::outline`).
//!   mark      record that an item actually helped
//!             (`serve::mark::record_useful`) - recorded and reported, but
//!             since 2026-08-03 it decides nothing about what fires.
//!   status    what the store holds right now: counts per kind, how many
//!             fireable items never fired, how many were served repeatedly, how many
//!             carry no falsifier (`serve::status::store_status`).
//!
//! Every code answer carries the commit its index was read at and whether the
//! checkout has moved on since, so a stale line number is always a LABELLED
//! stale line number.
//!
//! Why retract, resolve and history are here and were not before: without
//! them an agent can fill this memory but cannot maintain it. A fact that is
//! simply wrong has no way out, a diverged entity stays stuck forever, and
//! nobody can see what a fact used to say. All three read and write through
//! `model`, so the write gate still governs everything that lands.
//!
//! Why pin and unpin are here and were not before: a standing rule in 2.0 is
//! an item bound `Always` (served in full at every session start, by the
//! hook that selects only `Always`-bound items - see this crate's own note
//! below on why that hook has no tool of its own here) - but nothing in this
//! crate could add or remove that binding during a session.
//! 1.0 had `pin`/`unpin`; 2.0 simply had no tool onto the same capability, so
//! an agent could notice a rule ought to stand (or ought to stop standing)
//! and have no way to act on it. Both go through `model::store::revise`, so
//! the same write gate that governs `revise` governs these two as well.
//!
//! What is NOT here, on purpose: a tool for session start, the moment of
//! action, or the prompt surface. Those three are hooks (CONTRACT.md) - they
//! fire on their own, unprompted, at the moment they apply. Handing an agent
//! a tool that calls them itself would recreate exactly the "one ranked pool
//! an agent chooses from" that 2.0 was built to remove. A companion test
//! under this crate's own `tests/` directory greps this crate's source for
//! each of those three surfaces' own entry points and fails if any of them
//! is ever named in this crate's code.
//!
//! Failure policy at this boundary (CONTRACT.md's two policies, applied to
//! an MCP tool rather than a CLI): `remember`/`revise`/`pin`/`unpin`/`mark`
//! are write and declare, so a refusal is always prose with the reason and
//! the fix, never swallowed. `get`/`lookup`/`status` are read, so a broken or
//! missing store comes back as a plain, honest error text - never a panic,
//! and never an empty reply that could be mistaken for "nothing found".

use model::item::{Binding, Item, Kind, Severity, TargetKind};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use thor_core::event_store::EventStore;
use thor_core::inbox::{self, InboxOp};

const SESSION_ID: &str = "mcp";
const LINEAGE_ID: &str = "mcp";
const ACTOR: &str = "mcp";

/// What the body becomes: replaced wholesale, added to, or patched in one
/// place. Exactly one of the three, or none at all.
///
/// WHY THE SMALL EDITS EXIST. Replacing the whole body was the only way to
/// correct anything, so fixing one word in a 290-character rule meant
/// retyping all 290 and only then finding out whether the result still fit
/// the limit. That is friction on precisely the act this memory most needs
/// to be easy, and it is why corrections got skipped in favour of storing a
/// second copy - the exact defect `correct-instead-of-duplicating` exists to
/// prevent.
///
/// Nothing here bypasses anything: the result is handed to the same gate,
/// so the length limit, the typography rules and every other ground still
/// apply to what comes out.
fn edited_text(current: &str, args: &ReviseArgs) -> Result<String, String> {
    let modes = [args.text.is_some(), args.append.is_some(), args.replace_from.is_some()];
    if modes.iter().filter(|m| **m).count() > 1 {
        return Err(
            "give exactly one of text, append or replace_from - say what the body IS, or say what \
             to change about it, never both"
                .to_string(),
        );
    }
    if let Some(t) = &args.text {
        return Ok(t.clone());
    }
    if let Some(extra) = &args.append {
        let extra = extra.trim();
        if extra.is_empty() {
            return Err("append with nothing to add - omit it, or pass the words".to_string());
        }
        return Ok(format!("{} {extra}", current.trim_end()));
    }
    if let Some(from) = &args.replace_from {
        let Some(to) = &args.replace_to else {
            return Err("replace_from needs replace_to - pass an empty string to delete".to_string());
        };
        if from.is_empty() {
            return Err("replace_from cannot be empty - it would match at every position".to_string());
        }
        // A substring that is not there would make this a silent no-op that
        // still reports success, which is the worst kind of edit: the caller
        // believes the correction landed.
        if !current.contains(from.as_str()) {
            return Err(format!(
                "{from:?} is not in this item's text, so there is nothing to replace - read it \
                 with get first"
            ));
        }
        return Ok(current.replacen(from.as_str(), to, 1));
    }
    Ok(current.to_string())
}

/// Append `model::gate::warnings` to a write that already SUCCEEDED.
///
/// A warning is not a refusal and must never read like one: the item is
/// stored, and the note is about how well it will work, not about whether it
/// was allowed. Hence the wording, and hence the placement after the success
/// line rather than instead of it.
///
/// WHY IT HAS TO BE APPENDED, not prepended. `apply_captured` classifies a
/// drained write by the SUCCESS PREFIX of this very string ("stored ",
/// "revised ", "retracted ") - see its own note on why refusals share no
/// prefix to key on. Putting anything in front of that prefix would silently
/// reclassify every warned write as not-applied, and the queue would then
/// replay writes that had already landed.
///
/// Without this, `warnings` was a complete tested function that nothing ever
/// called, which is the same as not having built it.
fn with_warnings(
    success: String,
    item: &model::item::Item,
    root: Option<&std::path::Path>,
    store: Option<&thor_core::event_store::EventStore>,
) -> String {
    let mut notes: Vec<String> = model::gate::warnings(item).iter().map(|w| w.to_string()).collect();
    // The half of the capacity check that is a note rather than a refusal.
    // Without this call it was dead code: constructed in `model::store` and
    // read by nothing but its own unit test, so a writer filling a pool that
    // is already full of equals was told nothing at all.
    if let Some(store) = store {
        if let Ok(model::store::Capacity::Crowded(note)) = model::store::capacity(store, item) {
            notes.push(note);
        }
    }
    if let Some(root) = root {
        notes.extend(file_aware_warnings(item, root));
        notes.extend(suggested_check(item, root));
    }
    if notes.is_empty() {
        return success;
    }
    let notes: Vec<String> = notes.iter().map(|n| format!("  note: {n}")).collect();
    format!("{success}\n{}", notes.join("\n"))
}

/// Whether `line` is a comment, judged narrowly and on purpose.
///
/// The census that first counted hollow checks read a leading `#` as a
/// comment marker everywhere, which made a Markdown heading and a C
/// `#define` look like commentary: it reported 28 comment-only checks where
/// hand-checking found 6. This is the corrected rule, and it is deliberately
/// conservative - it would rather miss a real comment than call working code
/// one, because the warning it feeds is about weakening a proof.
fn looks_like_comment(line: &str, extension: &str) -> bool {
    let s = line.trim_start();
    match extension {
        // A heading is content, not commentary.
        "md" | "markdown" => false,
        _ if s.starts_with("#define") || s.starts_with("#include") => false,
        "sh" | "py" | "yml" | "yaml" | "toml" | "ps1" if s.starts_with('#') => true,
        _ => s.starts_with("//") || s.starts_with("<!--") || s.starts_with("/*") || s.starts_with('*'),
    }
}

/// The half of the hollow-check problem the write gate cannot see, because it
/// never reads a file: a literal that occurs everywhere, and one that only
/// ever sits in a comment.
///
/// The second is the dangerous one. A check on a function name matched a
/// passing remark in a comment while the real implementation lived in another
/// file, so the rule looked enforced and reached nobody who touched the code
/// it was about. Measured in a live repository, three checks failed that way.
///
/// A warning, never a refusal: a literal in a doc comment IS sometimes the
/// only place a file states what it does, and that is a weaker proof rather
/// than a wrong one.
fn file_aware_warnings(item: &model::item::Item, root: &std::path::Path) -> Vec<String> {
    let Some(model::item::Check::Contains { path, literal }) = item.check.as_ref() else {
        return Vec::new();
    };
    let Ok(body) = std::fs::read_to_string(root.join(path.replace('\\', "/"))) else {
        return Vec::new();
    };
    let extension = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");

    let mut out = Vec::new();
    let hits: Vec<&str> = body.lines().filter(|l| l.contains(literal.as_str())).collect();
    if hits.len() >= 25 {
        out.push(format!(
            "{literal:?} occurs {} times in {path}, so this check cannot tell you much - a whole \
             phrase or a signature would prove more",
            hits.len()
        ));
    }
    if !hits.is_empty() && hits.iter().all(|l| looks_like_comment(l, extension)) {
        out.push(format!(
            "{literal:?} appears only inside comments in {path}. That passes while the behaviour \
             it guards may live in another file entirely - check the anchor points at the code, \
             not at a remark about it"
        ));
    }
    out
}

/// The other half: a rule with NO check whose own falsifier names a real
/// file. That falsifier is already saying what would prove the rule wrong,
/// and half the time the file it names is right there - so the check writes
/// itself and nobody thought to ask for one.
///
/// A suggestion and nothing more. It never guesses a literal, because a
/// guessed proof is worse than none: it would put a rule back among the ones
/// allowed to block, on the strength of something nobody chose.
fn suggested_check(item: &model::item::Item, root: &std::path::Path) -> Vec<String> {
    if item.check.is_some() || !item.kind.can_fire() {
        return Vec::new();
    }
    let Some(falsifier) = item.falsifier.as_deref() else { return Vec::new() };
    for raw in falsifier.split_whitespace() {
        let token = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-');
        if token.len() < 5 || !token.contains('/') || token.contains("://") {
            continue;
        }
        if root.join(token).is_file() {
            return vec![format!(
                "this rule has no check, and its falsifier names {token}, which is really there. \
                 A contains check on a distinctive line of that file would prove this rule is still \
                 about something real, and would refuse a write to {token} that REMOVES that line. \
                 It does NOT refuse a write that introduces something forbidden - only absent, \
                 absent_all and forbidden do that, and only where a literal can honestly be named"
            )];
        }
    }
    Vec::new()
}

const INSTRUCTIONS: &str = "THOR 2.0's agent surface: fourteen tools, no more. retract removes an item \
that is simply WRONG (a reason is required; nothing is deleted, history still walks it), resolve \
settles an id that get reports as DIVERGED, history walks one id's whole life oldest first, and \
search_code, where_used and outline answer the three CODE questions - find text in the source, who defines and uses a symbol, what one file declares - each with the commit the index was read at on every answer. This \
memory is yours to MAINTAIN, not only to fill: correct with revise, remove with retract, and never \
store a second copy of something that already exists. remember declares a NEW \
item (model::store::declare) - a Rule or Orientation needs at least one binding (a Moment, a \
Target, or Always), a falsifier, and stays under 300 characters; a refusal names the exact reason \
and what to do instead, and nothing is written when it fires - that is the gate doing its job, not \
a bug to report. remember and revise both also take an optional check_kind/check_path, plus either \
check_literal (a single literal) or check_literals (a SET of them) - check_kind one of \
path_exists/contains/absent/absent_all/forbidden - a machine-runnable proof of the fact's own \
currency, alongside the required prose falsifier, never instead of it: a rule whose check currently \
HOLDS is the ONLY kind of rule this memory ever uses to block a write outright; a rule backed by \
prose alone can inform, but can never block. Use check_kind absent_all with check_literals when ONE \
rule forbids SEVERAL literals together in one specific file - e.g. a file that must never regain a \
TODO it was just cleaned of. Use check_kind forbidden with check_literals (never check_path - \
forbidden takes none) when the SAME kind of set has nothing to anchor to at all - e.g. a typography \
rule banning six punctuation characters wherever they might be written is ONE forbidden item with a \
six-entry set, never six near-identical items each banning one, and never absent_all anchored to one \
directory as a formality (that only proves the rule current for THAT directory, and leaves it \
silently not firing everywhere else the rule was meant to apply). Prefer forbidden when the rule \
forbids something self-contained; prefer the anchored forms (absent_all/absent/contains/path_exists) \
when the rule names a real file that could move or content that could change - that near-duplicate \
shape either way is exactly what this memory otherwise spends effort consolidating away. revise \
corrects an EXISTING \
item (model::store::revise) through \
the same gate, plus its own rule: omit a parameter to keep its current value, pass an empty string \
to clear severity/project/expires/key/falsifier (or check_kind, to clear the check), and no field \
the item already carried may silently disappear. pin adds the Always binding to an existing item \
(model::store::revise), so it joins the \
standing rules served at every session start; it is idempotent (pinning an already-pinned item \
does nothing) and refused when this kind may carry no binding at all. unpin removes the Always \
binding the same way; it is idempotent too, and refused when that would leave the item with no \
binding left to ever fire on - never a silent way to create a rule that can no longer fire. get \
shows one item whole by id. lookup searches every project, archive kinds (Report, \
Chunk) included, never scoped to 'the current project' the way session start is - call it BEFORE \
remember so you revise an existing item instead of storing a near-duplicate; pass key for a \
Lookup item's own exact key instead of query. mark records that an item actually helped - worth \
doing, and since 2026-08-03 nothing is ever withheld for the lack of it. status reports live \
counts per kind, how many Rule/Orientation items exist, how many were declared but have never \
once fired, how many were served repeatedly without ever being marked useful (a count to look \
at, not a filter), and how many carry no falsifier. There is deliberately NO tool for session \
start, the moment of action, or the prompt surface: those three fire on their own, from hooks, at \
the moment they apply - calling them yourself would recreate the single ranked pool THOR 2.0 was \
built to remove. A refusal from remember, revise, pin or unpin is not an error in this tool; it is \
the gate telling you exactly what would break and how to fix it.";

/// One target binding as an MCP argument: a JSON-schema-friendly stand-in for
/// `model::item::Binding::Target`, since `TargetKind` itself has no direct
/// schema derive wired up for external callers.
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct TargetArg {
    /// One of: path, dir, symbol, command, project, route, host.
    pub kind: String,
    /// The exact value this binding must match - no globs, no bare role name
    /// (main.rs, index.js, ...), no bare host (localhost, 0.0.0.0). The
    /// write gate refuses anything that would match everything.
    pub value: String,
}

/// Turn the three flat MCP fields (moments, targets, always) into the
/// `Binding` list `model::item::Item` actually stores. The one place both
/// `remember` and `revise` build a binding list, so the two tools can never
/// disagree about what a moment/target name means.
/// A check that claims to refuse must be shown to refuse, here, before the
/// write lands. See `serve::prove` for the defect this closes; the short
/// version is that "important" was a word anybody could write, and the store
/// counted it as protection without ever trying it.
///
/// Only a check that is TRIED and stays SILENT is refused. A check that cannot
/// be tried without a working copy passes - reporting it as unproven belongs
/// to `doctor`, not to a door that would otherwise refuse every file-reading
/// proof this store has.
fn refuse_an_unproven_check(item: &model::item::Item) -> Result<(), String> {
    match serve::prove::prove(&item.id, item) {
        serve::prove::Proof::Silent(attempt) => Err(format!(
            "REFUSED: this check does not actually refuse anything. The attempt it should have stopped - {attempt} - \
             went straight through the guard. A rule that looks armed and fires nothing is worse than one with no \
             check at all, because it is counted as protection. Fix the literal so it appears verbatim in the \
             command or content that makes the mistake, or anchor the rule to the command it is really about."
        )),
        serve::prove::Proof::NotProvable(why)
            if why.starts_with("a forbidden check reaches only") =>
        {
            Err(format!("REFUSED: {why}. As written it can never fire, whatever it looks like."))
        }
        _ => Ok(()),
    }
}

fn build_bindings(moments: &[String], targets: &[TargetArg], always: bool) -> Result<Vec<Binding>, String> {
    let mut bindings = Vec::new();
    for name in moments {
        bindings.push(model::gate::parse_moment(name).map_err(|e| e.to_string())?);
    }
    for t in targets {
        let kind = TargetKind::from_str(&t.kind).map_err(|e| e.to_string())?;
        bindings.push(Binding::Target { kind, value: t.value.clone() });
    }
    if always {
        bindings.push(Binding::Always);
    }
    Ok(bindings)
}

// The four flat MCP check fields (check_kind/check_path/check_literal/
// check_literals) are turned into `model::item::Check` by
// `model::gate::build_check` - the one shared home this crate and
// `model/src/bin/model.rs`'s CLI both call, so the two tools can never
// disagree about which combinations are valid. It used to be a
// byte-for-byte duplicated copy in each of the two binaries (gate.rs was
// locked at the time); now that it is not, both call sites were moved onto
// the one real function instead - check_literals (added for the absent_all
// set form) went through that same one function too, never a second copy of
// its argument-shape rules here. On `revise`, the "omit everything keeps the
// existing check" and "check_kind '' clears it" rules stay handled ONE LEVEL
// UP, in `revise` itself, exactly the way
// `severity`/`project`/`expires`/`key`/`falsifier` already handle their own
// "omit keeps, empty string clears" convention outside any shared parser -
// `gate::build_check` never sees either case.
//
// check_literal (singular) and check_literals (plural) are two SEPARATE
// fields rather than one field carrying a delimiter, and this is why a set
// never needed a delimiter design at all: an MCP tool argument is already a
// typed JSON value, so "a set of literals" is simply a JSON array - no
// syntax for an agent to get right, and no punctuation character (these
// literals are themselves punctuation, e.g. an em dash or an ellipsis) can
// ever collide with a separator that does not exist. See `gate::build_check`'s
// own doc comment for the full reasoning, including the CLI side's
// equivalent (a repeatable `--check-literals` flag, for the same reason).

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RememberArgs {
    /// Caller-chosen id, kept forever. Pick something short, stable and
    /// grep-able (e.g. "no-force-push-main"), not a sentence.
    pub id: String,
    /// One of: rule, orientation, report, lookup, chunk.
    pub kind: String,
    /// The fact itself. A Rule/Orientation is refused past 300 characters -
    /// move reasoning into a Report instead of lengthening this.
    pub text: String,
    /// One of: irreversible, costly, house_style. Meaningless (and refused
    /// as a binding target would be) on a Report/Lookup/Chunk.
    #[serde(default)]
    pub severity: Option<String>,
    /// Project this item belongs to. Omit for a global, cross-project item.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// ISO-8601 date. Only a Report may carry this - a Rule, Orientation,
    /// Lookup or Chunk with an expiry is refused (they last until revised).
    #[serde(default)]
    pub expires: Option<String>,
    /// Required for a Lookup: the exact key a future `lookup` call names to
    /// get this item back. Meaningless for the other four kinds.
    #[serde(default)]
    pub key: Option<String>,
    /// What observation would prove this fact wrong, one sentence. Required
    /// for a Rule or Orientation - they never expire, so this is the only
    /// thing that ever names when one has gone stale.
    #[serde(default)]
    pub falsifier: Option<String>,
    /// One of: path_exists, contains, absent, absent_all, forbidden. A
    /// machine-runnable check this item can prove itself against, alongside
    /// (never instead of) falsifier - see model::check::run. Only a
    /// Rule/Orientation may carry one. This is the ONLY kind of rule this
    /// memory ever uses to block a write outright, and only while the check
    /// currently HOLDS (see serve's write guard): a rule backed by prose
    /// alone can inform, but can never block. Every kind except forbidden
    /// requires check_path; path_exists refuses check_literal/
    /// check_literals; contains/absent require check_literal; absent_all
    /// and forbidden require check_literals instead (see each field's own
    /// note). Prefer forbidden when the rule forbids something
    /// self-contained, with nothing to anchor it to (a banned character or
    /// word, wherever it might be written); prefer absent_all when the rule
    /// is really about one file that could move, or content that could
    /// change. Omit check_kind, check_path, check_literal and check_literals
    /// all four for no check at all.
    #[serde(default)]
    pub check_kind: Option<String>,
    /// The exact file this check inspects, relative to the root the checker
    /// runs against. Required together with check_kind, for every check_kind
    /// EXCEPT forbidden - forbidden carries no path at all, and is refused
    /// if one is given.
    #[serde(default)]
    pub check_path: Option<String>,
    /// The exact literal a "contains" or "absent" check_kind looks for.
    /// Refused together with "path_exists", or together with check_literals;
    /// required with "contains"/"absent".
    #[serde(default)]
    pub check_literal: Option<String>,
    /// A SET of literals to forbid together, for check_kind "absent_all" (in
    /// one specific file) or "forbidden" (everywhere, no file at all) - one
    /// rule that forbids several things at once (e.g. every punctuation
    /// character a house typography style bans) is ONE item with a set
    /// here, never several near-identical items each forbidding one
    /// literal. Give every literal in the set as its own array entry -
    /// never joined into one string with a separator, since these literals
    /// are themselves punctuation and could collide with whatever separator
    /// was picked. Refused together with check_literal (use exactly one of
    /// the two); refused empty; refused with any check_kind other than
    /// "absent_all"/"forbidden".
    #[serde(default)]
    pub check_literals: Vec<String>,
    /// Action names this item fires on (repeatable), e.g. ["push"]. Only a
    /// Rule/Orientation may bind to a moment.
    #[serde(default)]
    pub moments: Vec<String>,
    /// Exact targets this item fires on (repeatable). Only a Rule/Orientation
    /// may bind to a target; the value must be the real path/command/etc,
    /// never a glob and never a bare role name.
    #[serde(default)]
    pub targets: Vec<TargetArg>,
    /// Bind this item to the pinned Always layer (served in full at every
    /// session start). Only a Rule/Orientation may set this.
    #[serde(default)]
    pub always: bool,
}

/// `Default` is derived on purpose. Every field added here used to break
/// every exhaustive struct literal in the crate's own tests at once - eleven
/// of them the last time - which makes a small extension look expensive and
/// pushes it into never happening. A literal that spells out only what it
/// cares about and ends in `..Default::default()` does not care what is
/// added later.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct ReviseArgs {
    /// The id of the existing item to correct.
    pub id: String,
    /// New text, replacing the whole body. Omit to keep the current text
    /// unchanged. For a SMALL correction to a long item, prefer `append` or
    /// `replace_from`/`replace_to` below - retyping a 290-character rule to
    /// fix one word is friction on exactly the maintenance this memory needs
    /// most, and it is the reason corrections get skipped.
    #[serde(default)]
    pub text: Option<String>,
    /// Add this to the END of the current text, with one space between.
    /// Refused together with `text` (say what the body is, or say what to add
    /// to it, never both). The result still goes through the whole gate, so a
    /// 300-character limit is enforced on what comes out, not on what you
    /// typed.
    #[serde(default)]
    pub append: Option<String>,
    /// Replace the FIRST occurrence of this substring in the current text
    /// with `replace_to`. Refused unless `replace_to` is given too, refused
    /// together with `text`, and refused when the substring is not actually
    /// in the current text - a silent no-op would report success while
    /// changing nothing.
    #[serde(default)]
    pub replace_from: Option<String>,
    /// What `replace_from` becomes. Pass an empty string to delete the
    /// substring.
    #[serde(default)]
    pub replace_to: Option<String>,
    /// One of: irreversible, costly, house_style. Omit to keep the current
    /// value; pass "" to clear it.
    #[serde(default)]
    pub severity: Option<String>,
    /// Omit to keep the current project; pass "" to make it global.
    #[serde(default)]
    pub project: Option<String>,
    /// Replaces the whole tag list. Omit to keep the current tags. An empty
    /// list is REFUSED if the item already carried tags - nothing may vanish
    /// silently on a revise; say so with a real (possibly shorter) list.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Omit to keep the current expiry; pass "" to clear it.
    #[serde(default)]
    pub expires: Option<String>,
    /// Omit to keep the current key; pass "" to clear it.
    #[serde(default)]
    pub key: Option<String>,
    /// Omit to keep the current falsifier; pass "" to clear it (a Rule or
    /// Orientation left with none is refused, same as at creation).
    #[serde(default)]
    pub falsifier: Option<String>,
    /// One of: path_exists, contains, absent, absent_all, forbidden. Omit
    /// ALL FOUR (check_kind, check_path, check_literal, check_literals) to
    /// keep the item's current check untouched; pass check_kind as "" to
    /// clear it (refused if check_path, check_literal or check_literals is
    /// also given - clearing takes none of them). Give check_kind, check_path
    /// (omitted for forbidden - see its own note) and whichever of
    /// check_literal/check_literals the kind takes, together, to replace the
    /// check wholesale. See RememberArgs' own note on what a check is for,
    /// which form to prefer, and the same combination rules (check_path
    /// required with check_kind except for forbidden; path_exists refuses
    /// check_literal/check_literals; contains/absent require check_literal;
    /// absent_all and forbidden require check_literals instead).
    #[serde(default)]
    pub check_kind: Option<String>,
    /// See check_kind's own note on the omit/clear/replace convention.
    /// Refused outright if check_kind is "forbidden" - that kind carries no
    /// path at all.
    #[serde(default)]
    pub check_path: Option<String>,
    /// See check_kind's own note on the omit/clear/replace convention.
    #[serde(default)]
    pub check_literal: Option<String>,
    /// The set form of check_literal, for check_kind "absent_all" or
    /// "forbidden" - see RememberArgs' own note on why this is a separate
    /// repeatable field rather than a delimited string. Part of the same
    /// omit/clear/replace convention as check_kind: an empty list here reads
    /// the same as never mentioning it, exactly like omitting check_kind/
    /// check_path/check_literal does, since a list has no separate way to
    /// say "given, but deliberately empty".
    #[serde(default)]
    pub check_literals: Vec<String>,
    /// Replaces the moment bindings. Give this, `targets`, and/or `always`
    /// TOGETHER to replace the WHOLE binding list in one call - when none of
    /// the three are given, the existing bindings are kept untouched.
    #[serde(default)]
    pub moments: Option<Vec<String>>,
    /// Replaces the target bindings - see `moments`' own note on how the
    /// three binding fields combine.
    #[serde(default)]
    pub targets: Option<Vec<TargetArg>>,
    /// Replaces whether this item is bound Always - see `moments`' own note.
    #[serde(default)]
    pub always: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PinArgs {
    /// The id to pin (add the Always binding to it).
    pub id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UnpinArgs {
    /// The id to unpin (remove the Always binding from it).
    pub id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct GetArgs {
    /// The id to show.
    pub id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LookupArgs {
    /// Free-text search over every live item's text and tags, any project,
    /// archive kinds (Report/Chunk) included - never a Lookup item (those
    /// answer only to their own `key`, see below).
    #[serde(default)]
    pub query: Option<String>,
    /// An exact Lookup key. When given, this is the ONLY parameter used -
    /// `query` is ignored - and only a real Lookup item can ever answer it.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MarkArgs {
    /// The id of the item being judged.
    pub id: String,
    /// Set true to record the OPPOSITE: this item did not belong where it
    /// fired. Two noise judgements, with no mark of usefulness, retire it
    /// from the injection surfaces; it stays fully findable via lookup.
    #[serde(default)]
    pub noise: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct HistoryArgs {
    /// The id whose whole life to walk.
    pub id: String,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct RetractArgs {
    /// The id to retract.
    pub id: String,
    /// Why it is wrong or no longer applies. Required, and a blank one is
    /// refused: without it nobody can later tell a decision from an accident.
    pub reason: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ResolveArgs {
    /// The diverged id.
    pub id: String,
    /// The revision hash that survives.
    pub keep: String,
    /// Every other current head, in full. Leaving one out fails rather than
    /// silently discarding it.
    pub discard: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct WhereUsedArgs {
    /// The bare symbol name: a function, class, struct or variable.
    pub name: String,
    /// Most sites to return per side. Defaults to 30.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct OutlineArgs {
    /// Repository-relative path, exactly as the index stores it.
    pub path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SearchCodeArgs {
    /// Case-insensitive substring to find in the indexed source.
    pub query: String,
    /// Most hits to return. Defaults to 10.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// The one answer when a code tool is called on a server that has no index.
/// Said plainly, because an empty result would read as "your code does not
/// contain that" - the silent-does-less failure this whole rebuild is against.
const NO_CODE_INDEX: &str = "no code index is configured for this server - start it with \
--code-index-db and --repo, or build one first with the codeindex command";

/// How stale this answer is, in one sentence. Defined once and shared by every
/// code tool: provenance is prose, not a boolean, and "the checkout could not
/// be read" is a different answer from "nothing drifted". Collapsing those two
/// is exactly how a stale hit reads as a fresh one.
fn drift_line(p: &serve::lookup::CodeProvenance) -> String {
    match &p.current_commit {
        Err(reason) => format!(
            "indexed at commit {}. The checkout could not be read just now ({reason}), so \
             whether it has moved on is UNKNOWN - open the real file before trusting a line \
             number.",
            p.indexed_commit
        ),
        Ok(current) if current == &p.indexed_commit && p.uncommitted_changed == Some(0) => {
            format!("indexed at commit {}, checkout matches.", p.indexed_commit)
        }
        Ok(current) => {
            let files = p
                .files_differ
                .map(|n| n.to_string())
                .unwrap_or_else(|| "an unknown number of".to_string());
            let dirty = p
                .uncommitted_changed
                .map(|n| n.to_string())
                .unwrap_or_else(|| "an unknown number of".to_string());
            format!(
                "indexed at commit {}, checkout at {current}: {files} file(s) differ and {dirty} \
                 carry uncommitted changes - open the real file before trusting a line number.",
                p.indexed_commit
            )
        }
    }
}

/// Where the code index lives, when one is configured. Both halves are needed
/// together: the database holds the chunks, the checkout is what provenance is
/// measured against, and a hit without provenance is the "answer from an
/// unknown moment" this whole index exists to avoid.
#[derive(Clone)]
pub struct CodeIndexPaths {
    pub index_db: std::path::PathBuf,
    pub repo: std::path::PathBuf,
}

#[derive(Clone)]
pub struct ThorMcpServer {
    store: Arc<Mutex<EventStore>>,
    code: Option<CodeIndexPaths>,
    /// Where this store's vector sidecar lives, so `lookup` can search on
    /// meaning and not only on the letter. `None` for a store with no file of
    /// its own (every in-memory test store), and then `lookup` is literal -
    /// which is what those tests are asserting about anyway.
    vectors: Option<std::path::PathBuf>,
    /// A loaded embedder, reused across `lookup` calls instead of reloaded
    /// from disk on every one of them.
    ///
    /// THE DEFECT THIS CLOSES (GAP 1, found auditing this server,
    /// 2026-08-03): `serve::lookup::search_best_effort` loads the ONNX model
    /// from disk on every call (roughly a second, see `serve::embed::
    /// Embedder`'s own doc comment). That is fine for the CLI, which makes
    /// one call and exits, but this server is long-lived: every `lookup`
    /// call was paying the full load cost instead of paying it once for the
    /// life of the process. `None` until the first `lookup` that reaches the
    /// semantic path loads it; every later call reuses what is already here
    /// (see `serve::lookup::search_best_effort_cached`, the function this
    /// cache is threaded through).
    #[cfg(feature = "semantic")]
    embedder_cache: Arc<Mutex<Option<serve::embed::Embedder>>>,
    /// The checkout this server was started in, if it is one. The write
    /// gate's neighbourhood toll needs somewhere to run a check against, and
    /// `None` simply means no toll - the same fail-open rule every other
    /// check in this system follows when its anchor does not resolve.
    root: Option<Arc<PathBuf>>,
    /// Replica divert mode: when set, `remember`, `revise` and `retract` do
    /// NOT write to this store. They append the call to the capture inbox at
    /// this path instead, for the authority to replay (see
    /// `thor_core::inbox` for why a replica may never append to its own log).
    /// `None` on the authority itself, which is every local session.
    inbox: Option<Arc<PathBuf>>,
    #[allow(dead_code)] // read only inside the #[tool_handler] macro expansion
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ThorMcpServer {
    pub fn new(store: EventStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            code: None,
            vectors: None,
            #[cfg(feature = "semantic")]
            embedder_cache: Arc::new(Mutex::new(None)),
            root: None,
            inbox: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Same server, with a code index wired up so `search_code` can answer.
    /// Without this the tool still exists and says plainly that no index is
    /// configured - it never returns an empty result that reads like "your
    /// code does not contain that".
    pub fn with_code_index(store: EventStore, code: CodeIndexPaths) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            code: Some(code),
            vectors: None,
            #[cfg(feature = "semantic")]
            embedder_cache: Arc::new(Mutex::new(None)),
            root: None,
            inbox: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Point `lookup` at this store's vector sidecar.
    ///
    /// The defect this exists to close, found on the owner's first real day
    /// (2026-08-03): his own benchmark for the whole tool is "ask me about
    /// the BBQ recipes in my memory during a coding session". Asked through
    /// the agent for "bbq recept" it answered NOTHING, while the identical
    /// question on the command line answered fine. Two doors onto surface 4
    /// had drifted apart - the CLI called `search_best_effort` (letter AND
    /// meaning), this server called `search` (letter only), so a two-word
    /// question found nothing because no item contains that exact string.
    /// The agent is the door he actually uses.
    pub fn with_vectors(mut self, vectors: std::path::PathBuf) -> Self {
        self.vectors = Some(vectors);
        self
    }

    /// Point the write gate's neighbourhood toll at a checkout: adding a
    /// fact to a target that already holds one whose own proof has gone
    /// false is then refused, naming what to settle (see
    /// `model::store::declare_in`). Without this the toll does not exist.
    pub fn with_root(mut self, root: PathBuf) -> Self {
        self.root = Some(Arc::new(root));
        self
    }

    /// Put this server in replica DIVERT mode: `remember`, `revise` and
    /// `retract` are captured to `path` instead of written to the store.
    ///
    /// Only a replica ever does this. The authority's own server must never
    /// be given an inbox: it would then capture its own writes and wait
    /// forever for someone to drain them into itself.
    pub fn with_capture_inbox(mut self, path: PathBuf) -> Self {
        self.inbox = Some(Arc::new(path));
        self
    }

    /// In divert mode, capture `args` as a call to `tool` and return the
    /// reply the caller should see. `None` on the authority, where the write
    /// proceeds normally.
    ///
    /// A capture that cannot be written is reported as a FAILED write, not a
    /// queued one. The whole point of this path is that the thought is not
    /// lost, and a caller told "queued" for a line that never reached disk
    /// would have no reason to say it again.
    fn capture(&self, tool: &str, args: &impl Serialize) -> Option<String> {
        let path = self.inbox.as_ref()?;
        let value = match serde_json::to_value(args) {
            Ok(v) => v,
            Err(e) => {
                return Some(format!(
                    "error: this connector queues writes for the main machine, and this one could \
                     not be serialized ({e}), so NOTHING was saved. Say it again in a session on \
                     the main machine."
                ))
            }
        };
        let op = InboxOp::new(tool, value);
        let capture_id = op.capture_id.clone();
        match inbox::append(path, &op) {
            Ok(()) => Some(format!(
                "queued for the main machine ({tool}, capture {capture_id}). This connector is a \
                 replica: it never writes to the log directly, so the item does not exist here yet \
                 and will not turn up in lookup here. It is applied on the next drain, and the \
                 write gate runs there - a refusal shows up in that drain's report, not here."
            )),
            Err(e) => Some(format!(
                "error: the write could NOT be queued ({e}), so nothing was saved anywhere. Do not \
                 assume it landed."
            )),
        }
    }

    /// Run a sync store closure off the async runtime so a slow store call
    /// never blocks the transport. `Ok`/`Err` both become the tool's plain
    /// text reply - a refusal is prose with the reason and the fix, never a
    /// swallowed failure and never a bare boolean.
    fn blocking<F>(&self, f: F) -> impl std::future::Future<Output = String>
    where
        F: FnOnce(&mut EventStore) -> Result<String, String> + Send + 'static,
    {
        let store = self.store.clone();
        async move {
            match tokio::task::spawn_blocking(move || {
                let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                f(&mut s).unwrap_or_else(|e| e)
            })
            .await
            {
                Ok(text) => text,
                Err(join_err) => format!(
                    "error: an internal task failed ({join_err}); check with get or status before \
                     retrying any write - do not assume it landed, and do not assume it did not"
                ),
            }
        }
    }

    #[tool(description = "Declare a NEW memory item (Rule/Orientation/Report/Lookup/Chunk), through the write gate (model::store::declare). A Rule/Orientation with no binding, no falsifier, or over 300 characters is REFUSED with the exact reason and what to fix instead - that is the gate doing its job, not a failure of this tool, and nothing is written when it fires. Call lookup first so you revise an existing item instead of storing a near-duplicate. Optionally set check_kind plus either check_literal or check_literals (check_kind one of path_exists/contains/absent/absent_all/forbidden) to give a Rule or Orientation a machine-runnable proof of its own currency, alongside its required prose falsifier, never instead of it: a rule whose check currently HOLDS is the only kind of rule this memory ever uses to block a write outright (see serve's write guard); prose alone can inform, but can never block. path_exists/contains/absent/absent_all also need check_path, the exact file the check inspects; forbidden takes no check_path at all. Use check_kind absent_all with check_literals (a set) when ONE rule forbids SEVERAL literals together in one specific file; use check_kind forbidden with check_literals instead when the rule forbids something self-contained with nothing to anchor to (e.g. a typography rule banning six different punctuation characters wherever they are written is one forbidden item with a six-entry set, never six near-identical items each banning one, and never absent_all anchored to one directory as a formality - that only proves the rule current for that directory, leaving it silently not firing anywhere else it was meant to apply). A forbidden check reaches wherever its BINDING says, and there are exactly two bindings that carry a reach it can honour: always (every file write) and a command target (that one command, matched with sudo, a full path and a .exe suffix all taken off first). That second form is how a COMMAND prohibition - never run this exact thing - becomes a real refusal instead of prose: give the item a command target and a forbidden check whose literal is the dangerous fragment, and give it NO always binding unless writing those same words in a file is also forbidden, or the rule will refuse the documentation that describes it. A forbidden check on any other binding passes this gate, looks like the strongest form, and can never fire.")]
    async fn remember(&self, Parameters(args): Parameters<RememberArgs>) -> String {
        if let Some(queued) = self.capture("remember", &args) {
            return queued;
        }
        let root = self.root.clone();
        self.blocking(move |s| {
            let kind = Kind::from_str(&args.kind).map_err(|e| format!("invalid 'kind': {e}"))?;
            let severity = match args.severity.as_deref() {
                None => None,
                Some(v) => Some(Severity::from_str(v).map_err(|e| format!("invalid 'severity': {e}"))?),
            };
            let bindings =
                build_bindings(&args.moments, &args.targets, args.always).map_err(|e| format!("invalid binding: {e}"))?;
            let check = model::gate::build_check(
                args.check_kind.as_deref(),
                args.check_path.clone(),
                args.check_literal.clone(),
                args.check_literals.clone(),
            )
            .map_err(|e| format!("invalid check: {e}"))?;
            let item = Item {
                id: args.id.clone(),
                kind,
                text: args.text.clone(),
                bindings,
                severity,
                project: args.project.clone(),
                tags: args.tags.clone(),
                expires: args.expires.clone(),
                key: args.key.clone(),
                falsifier: args.falsifier.clone(),
                check,
            };
            if let Err(why) = refuse_an_unproven_check(&item) {
                return Err(why);
            }
            match model::store::declare_in(s, SESSION_ID, LINEAGE_ID, ACTOR, &item, root.as_deref().map(|p| p.as_path())) {
                Ok(event) => Ok(with_warnings(
                    format!("stored '{}' ({:?}, event seq {})", item.id, item.kind, event.seq),
                    &item,
                    root.as_deref().map(|p| p.as_path()),
                    Some(s),
                )),
                Err(e) => Err(format!("REFUSED: {e}")),
            }
        })
        .await
    }

    #[tool(description = "Correct an EXISTING item by id (model::store::revise): the same write gate as remember, plus its own rule that no field the item already carried may silently vanish - dropping a field you do not mention is REFUSED, not applied quietly. Omit a parameter to keep its current value; pass an empty string on severity/project/expires/key/falsifier to clear it. check_kind/check_path/check_literal/check_literals work together: omit all four to keep the existing check untouched, pass check_kind as \"\" to clear it, or give check_kind plus whichever of check_path/check_literal/check_literals the kind takes, together, to replace it wholesale - see remember's own note on what a check is for (the only kind of rule this memory ever uses to block a write outright, and only while it currently holds), on path_exists/contains/absent/absent_all needing check_path while forbidden needs none, and on when to prefer forbidden's path-less set form over absent_all's anchored one. Prefer this over remember for anything that changed.")]
    async fn revise(&self, Parameters(args): Parameters<ReviseArgs>) -> String {
        if let Some(queued) = self.capture("revise", &args) {
            return queued;
        }
        // The checkout, for the notes that need to read a file (see
        // `file_aware_warnings`). Cloned out here because the closure below
        // cannot borrow self.
        let revise_root = self.root.clone();
        self.blocking(move |s| {
            let existing = model::store::show(s, &args.id).map_err(|e| format!("cannot revise: {e}"))?;
            let bindings = if args.moments.is_some() || args.targets.is_some() || args.always.is_some() {
                build_bindings(
                    args.moments.as_deref().unwrap_or(&[]),
                    args.targets.as_deref().unwrap_or(&[]),
                    args.always.unwrap_or(false),
                )
                .map_err(|e| format!("invalid binding: {e}"))?
            } else {
                existing.bindings.clone()
            };
            // Each field below also records whether THIS call is what
            // cleared it - an explicit empty string, never merely omitted -
            // so `cleared` (built further down) can carry that intent to
            // the write gate. `model::gate::revise`'s field-preservation
            // check (ground 9) compares two finished `Item`s and cannot
            // tell "the caller forgot to mention this" from "the caller
            // asked to clear it": both end up as `existing.field.is_some()
            // && updated.field.is_none()`. The bool captured right here,
            // at the one place that still knows which it was, is what lets
            // `ClearedFields::baseline` (see `model::gate`) tell the gate
            // the difference without weakening it for anyone who never
            // asked for a clear at all - see that function's own doc
            // comment for the full reasoning, and this task's own report.
            // WHAT COUNTS AS A CLEAR, and why it is not just the empty string.
            // The documented convention is "pass an empty string to clear",
            // and it was unreachable: measured six times on 2026-08-06, an
            // empty string does not survive the MCP call layer - the value
            // disappears and the request never arrives as valid JSON. So the
            // one documented way to clear a field could not be used by the
            // very agent the documentation is written for, while the code
            // underneath supported it and had a test per field proving so.
            // A whitespace-only value now clears too. None of these five
            // fields has any use for a value made of spaces: a project named
            // " ", a key of a tab, a falsifier of nothing. Reading it as the
            // clear it obviously is costs no legitimate case and makes the
            // promise reachable through any transport.
            let is_clear = |v: Option<&str>| matches!(v, Some(s) if s.trim().is_empty());
            let severity = match args.severity.as_deref() {
                None => existing.severity,
                Some(v) if v.trim().is_empty() => None,
                Some(v) => Some(Severity::from_str(v).map_err(|e| format!("invalid 'severity': {e}"))?),
            };
            let severity_cleared = is_clear(args.severity.as_deref());
            let project = match args.project.as_deref() {
                None => existing.project.clone(),
                Some(p) if p.trim().is_empty() => None,
                Some(p) => Some(p.to_string()),
            };
            let project_cleared = is_clear(args.project.as_deref());
            let expires = match args.expires.as_deref() {
                None => existing.expires.clone(),
                Some(e) if e.trim().is_empty() => None,
                Some(e) => Some(e.to_string()),
            };
            let expires_cleared = is_clear(args.expires.as_deref());
            let key = match args.key.as_deref() {
                None => existing.key.clone(),
                Some(k) if k.trim().is_empty() => None,
                Some(k) => Some(k.to_string()),
            };
            let key_cleared = is_clear(args.key.as_deref());
            let falsifier = match args.falsifier.as_deref() {
                None => existing.falsifier.clone(),
                Some(f) if f.trim().is_empty() => None,
                Some(f) => Some(f.to_string()),
            };
            let falsifier_cleared = is_clear(args.falsifier.as_deref());
            // Same convention as every flat MCP check field: omitting
            // check_kind/check_path/check_literal/check_literals ALL FOUR
            // keeps the existing check untouched (mirrors `bindings` above -
            // nothing mentioned means nothing changes). check_literals is a
            // `Vec`, not an `Option`, so it has no way to say "given, but
            // deliberately empty" the way check_kind's "" does - an empty
            // check_literals is therefore treated exactly like an omitted
            // one here, consistent with how `gate::build_check` itself reads
            // an empty list for check_kind absent_all (see that function's
            // own doc comment). check_kind "" clears it, refused if
            // check_path/check_literal/check_literals are also given (a
            // self-contradictory request: clear AND set at once). Anything
            // else runs through the same gate::build_check remember uses,
            // replacing the check wholesale.
            let check = if args.check_kind.is_none()
                && args.check_path.is_none()
                && args.check_literal.is_none()
                && args.check_literals.is_empty()
            {
                existing.check.clone()
            } else if args.check_kind.as_deref() == Some("") {
                if args.check_path.is_some() || args.check_literal.is_some() || !args.check_literals.is_empty() {
                    return Err(
                        "invalid check: check_kind was cleared (\"\") but check_path, check_literal \
                         or check_literals was also given - clearing a check takes none of them; \
                         drop them to clear the check, or give a real check_kind to replace it \
                         instead"
                            .to_string(),
                    );
                }
                None
            } else {
                model::gate::build_check(
                    args.check_kind.as_deref(),
                    args.check_path.clone(),
                    args.check_literal.clone(),
                    args.check_literals.clone(),
                )
                .map_err(|e| format!("invalid check: {e}"))?
            };
            let check_cleared = args.check_kind.as_deref() == Some("");
            // Prefixed like every other refusal on this surface: a caller
            // that keys on "REFUSED" must not have to learn a second shape
            // just because the reason came from the edit modes.
            let text = edited_text(&existing.text, &args).map_err(|e| format!("REFUSED: {e}"))?;
            let updated = Item {
                id: existing.id.clone(),
                kind: existing.kind,
                text,
                bindings,
                severity,
                project,
                tags: args.tags.clone().unwrap_or_else(|| existing.tags.clone()),
                expires,
                key,
                falsifier,
                check,
            };
            // `existing` itself is used by `model::store::revise` for
            // exactly one thing: the ground-9 comparison against `updated`
            // (the persisted body always comes from `updated` - see
            // `store::revise`'s own body). Handing it the CLEARED baseline
            // instead of the raw `existing` is what turns a field named
            // here into a legitimate drop, without touching `revise`
            // itself or its own unconditional protection for every field
            // NOT named - see `model::gate::ClearedFields` for why this is
            // sound and `model/src/gate.rs`'s own `cleared_fields` tests.
            let cleared = model::gate::ClearedFields {
                severity: severity_cleared,
                project: project_cleared,
                expires: expires_cleared,
                key: key_cleared,
                falsifier: falsifier_cleared,
                check: check_cleared,
            };
            let gate_existing = cleared.baseline(&existing);
            // Same door as remember: a correction must not be able to swap a
            // working lock for one that fires nothing.
            if let Err(why) = refuse_an_unproven_check(&updated) {
                return Err(why);
            }
            match model::store::revise(s, SESSION_ID, LINEAGE_ID, ACTOR, &gate_existing, &updated) {
                Ok(event) => {
                    Ok(with_warnings(
                        format!("revised '{}' (event seq {})", updated.id, event.seq),
                        &updated,
                        revise_root.as_deref().map(|p| p.as_path()),
                        Some(s),
                    ))
                }
                Err(e) => Err(format!("REFUSED: {e}")),
            }
        })
        .await
    }

    /// THE DEFECT THIS CLOSES (GAP 2, found auditing this crate's own tool
    /// set, 2026-08-03): a standing rule in 2.0 is an item bound `Always`
    /// (served in full at every session start - see the crate-level doc
    /// comment above for which hook decides that, and why it has no tool of
    /// its own here) - but this crate had no way for an agent to add or
    /// remove that binding during a session at all. 1.0 had
    /// `pin`/`unpin`; 2.0 simply had no tool onto the same capability. Both
    /// go through `model::store::revise`, so the write gate still governs
    /// what may carry an `Always` binding (a Report or Chunk carrying any
    /// binding at all is refused there, exactly as it is for `remember`).
    #[tool(description = "Pin an item: add the Always binding, so it is served in full at every session start (model::store::revise), keeping every other binding and field untouched. Idempotent - pinning an item that is already pinned changes nothing and is not an error. Refused, loudly, by the same write gate as remember/revise when this kind may carry no binding at all (a Report or a Chunk).")]
    async fn pin(&self, Parameters(args): Parameters<PinArgs>) -> String {
        self.blocking(move |s| {
            let existing = model::store::show(s, &args.id).map_err(|e| format!("cannot pin: {e}"))?;
            if existing.bindings.contains(&Binding::Always) {
                return Ok(format!("already pinned: '{}' (Always binding already present)", existing.id));
            }
            let mut updated = existing.clone();
            updated.bindings.push(Binding::Always);
            match model::store::revise(s, SESSION_ID, LINEAGE_ID, ACTOR, &existing, &updated) {
                Ok(event) => Ok(format!("pinned '{}' (event seq {})", updated.id, event.seq)),
                Err(e) => Err(format!("REFUSED: {e}")),
            }
        })
        .await
    }

    #[tool(description = "Unpin an item: remove the Always binding (model::store::revise), keeping every other binding and field untouched. Idempotent - unpinning an item with no Always binding changes nothing and is not an error. Refused, loudly, when removing Always would leave the item with NO binding at all: a Rule or Orientation with no binding can never fire, so this never silently creates an unfireable item - give it another binding (a Moment or a Target) first, or leave it pinned.")]
    async fn unpin(&self, Parameters(args): Parameters<UnpinArgs>) -> String {
        self.blocking(move |s| {
            let existing = model::store::show(s, &args.id).map_err(|e| format!("cannot unpin: {e}"))?;
            if !existing.bindings.contains(&Binding::Always) {
                return Ok(format!("already unpinned: '{}' (no Always binding present)", existing.id));
            }
            let remaining: Vec<Binding> = existing.bindings.iter().filter(|b| **b != Binding::Always).cloned().collect();
            if remaining.is_empty() {
                return Err(format!(
                    "REFUSED: removing Always from '{}' would leave it with no binding at all - a Rule or \
                     Orientation with no binding can never fire, so this refuses rather than silently create an \
                     unfireable item. Give it another binding (a Moment or a Target) before unpinning, or leave it \
                     pinned.",
                    existing.id
                ));
            }
            let mut updated = existing.clone();
            updated.bindings = remaining;
            match model::store::revise(s, SESSION_ID, LINEAGE_ID, ACTOR, &existing, &updated) {
                Ok(event) => Ok(format!("unpinned '{}' (event seq {})", updated.id, event.seq)),
                Err(e) => Err(format!("REFUSED: {e}")),
            }
        })
        .await
    }

    #[tool(description = "Show one item whole, by id (model::store::show). Read-only. Reports a plain, honest error - never a blank reply that could look like success - when the id is unknown, DIVERGED (more than one current head; needs a resolve outside this tool set), or its stored body will not parse.")]
    async fn get(&self, Parameters(args): Parameters<GetArgs>) -> String {
        self.blocking(move |s| match model::store::show(s, &args.id) {
            Ok(item) => serde_json::to_string_pretty(&item).map_err(|e| format!("could not render item: {e}")),
            Err(e) => Err(e.to_string()),
        })
        .await
    }

    #[tool(description = "Search THOR's memory (surface 4, OPZOEKEN): every live item, every project, archive kinds (Report/Chunk) fully included - never scoped to 'the current project' the way session start is. Pass 'query' for a free-text search over an item's text and tags; pass 'key' for the exact key of one Lookup item (query is then ignored). Call this BEFORE remember, so you revise an existing item instead of storing a near-duplicate. Never an injection surface: nothing found here is ever pushed at you unprompted - you asked for it.")]
    async fn lookup(&self, Parameters(args): Parameters<LookupArgs>) -> String {
        let vectors = self.vectors.clone();
        #[cfg(feature = "semantic")]
        let embedder_cache = self.embedder_cache.clone();
        self.blocking(move |s| {
            if let Some(key) = args.key.as_deref().filter(|k| !k.is_empty()) {
                return match serve::lookup::by_key(s, key) {
                    Some(hit) => Ok(format!("{} ({:?}): {}", hit.id, hit.item.kind, hit.item.text)),
                    None => Ok(format!("no lookup found for key '{key}'")),
                };
            }
            let query = args
                .query
                .as_deref()
                .filter(|q| !q.is_empty())
                .ok_or_else(|| "pass 'query' (free-text search) or 'key' (an exact Lookup key)".to_string())?;
            // Letter AND meaning, exactly like the CLI's own `search` - see
            // `with_vectors` for the defect that came from these two doors
            // answering differently. Degrades to the literal search on its
            // own whenever no sidecar or model is present. The semantic arm
            // reuses this server's own cached embedder (GAP 1: this process
            // is long-lived, so the model is loaded at most once for its
            // whole life instead of once per call) rather than loading a
            // fresh one on every call the way the CLI's one-shot process can
            // afford to.
            let hits = match vectors.as_deref() {
                #[cfg(feature = "semantic")]
                Some(path) => {
                    let mut cache = embedder_cache.lock().unwrap_or_else(|p| p.into_inner());
                    serve::lookup::search_best_effort_cached(s, path, None, query, &mut cache)
                }
                #[cfg(not(feature = "semantic"))]
                Some(_) => serve::lookup::search(s, query),
                None => serve::lookup::search(s, query),
            };
            // How many the expiry rule held back, so a thin answer is never
            // mistaken for an empty memory (see lookup::search_with_expired).
            let withheld = serve::lookup::search_with_expired(s, query).1;
            let mut out = String::new();
            if hits.is_empty() {
                out.push_str(&format!("no matches for '{query}'\n"));
            }
            // A CAP, AND IT SAYS SO. Reported from a real session and
            // reproduced: a single common word returned 270,000 characters
            // across 797 items, which the caller's own transport then cut off
            // at an arbitrary point. An answer that has to be truncated by
            // something downstream is not an answer, and a silent truncation
            // reads as "that is all there is".
            //
            // The whole set is still reachable - narrow the query, or ask for
            // one by id with get - and the line below says exactly how much is
            // not shown, because a cap nobody is told about is the same defect
            // in a politer form.
            const MAX_HITS: usize = 25;
            for hit in hits.iter().take(MAX_HITS) {
                out.push_str(&format!("{} ({:?}): {}\n", hit.id, hit.item.kind, hit.item.text));
            }
            if hits.len() > MAX_HITS {
                out.push_str(&format!(
                    "({} more match(es) not shown, of {} in total. Add a word to narrow it, or ask for one by id with get.)\n",
                    hits.len() - MAX_HITS,
                    hits.len()
                ));
            }
            if withheld > 0 {
                out.push_str(&format!(
                    "({withheld} match(es) held back: their own expiry date has passed. Ask for one by id with get if you need it.)\n"
                ));
            }
            Ok(out)
        })
        .await
    }

    #[tool(description = "Judge an item you were served or looked up. Default records that it HELPED (serve::mark::record_useful), which clears the noise recorded BEFORE it - not the noise recorded after: the latest verdict is the one that counts, because a reader is allowed to change their mind about an item that has drifted. Pass noise:true for the opposite - this did not belong where it fired - and two of those SINCE the last mark of usefulness retire it from the injection surfaces while leaving it fully findable via lookup. The noise side is the ONLY thing that retires anything: a serving count decides nothing, because firing often is what relevance looks like (see serve::decay for the day that was measured). Judging as you go is where this is done best.")]
    async fn mark(&self, Parameters(args): Parameters<MarkArgs>) -> String {
        self.blocking(move |s| {
            let now = serve::time::now_iso8601();
            let written = if args.noise {
                serve::mark::record_noise(s, SESSION_ID, LINEAGE_ID, ACTOR, &now, &args.id)
            } else {
                serve::mark::record_useful(s, SESSION_ID, LINEAGE_ID, ACTOR, &now, &args.id)
            };
            match written {
                Ok(_) if args.noise => Ok(format!(
                    "marked noise: {} ({}+ of these, with no mark of usefulness, retires it from the injection surfaces; it stays findable via lookup)",
                    args.id,
                    serve::decay::NOISE_MARKS_BEFORE_STALE
                )),
                Ok(_) => Ok(format!("marked useful: {} (clears the noise recorded before it; a later noise mark still counts)", args.id)),
                Err(e) => Err(format!("could not record the mark: {e}")),
            }
        })
        .await
    }

    #[tool(description = "What this memory holds right now (serve::status::store_status): live item counts per kind, how many Rule/Orientation items exist in total, how many of those were declared but have never once fired, how many have been served repeatedly without ever being marked useful (a count to look at - since 2026-08-03 nothing is withheld for it, see serve::decay), and how many carry no falsifier. Read-only, takes no arguments.")]
    async fn status(&self) -> String {
        self.blocking(move |s| {
            let st = serve::status::store_status(s);
            let mut out = String::new();
            for row in &st.counts {
                out.push_str(&format!("{:?}: {}\n", row.kind, row.count));
            }
            out.push_str(&format!(
                "fireable total (Rule + Orientation): {}\n\
                 declared, never fired: {}\n\
                 served repeatedly, never marked useful: {} (a count, not a filter)\n\
                 retired as noise (called noise {}+ times, never marked useful): {}\n\
                 missing falsifier: {}\n",
                st.fireable_total,
                st.declared_never_fired,
                st.served_never_marked,
                serve::decay::NOISE_MARKS_BEFORE_STALE,
                st.decayed,
                st.missing_falsifier
            ));
            Ok(out)
        })
        .await
    }

    #[tool(description = "Walk one item's whole life by id (model::store::history), oldest first: every declare, revise and retraction, each with its sequence number, its revision hash and who wrote it. Read-only. Nothing is ever deleted from the log, so this still works for an item that has been retracted - use it to see WHAT a fact used to say and when it changed, before you revise or retract it. An unknown id comes back as an empty history, not an error.")]
    async fn history(&self, Parameters(args): Parameters<HistoryArgs>) -> String {
        self.blocking(move |s| match model::store::history(s, &args.id) {
            Ok(log) if log.is_empty() => Ok(format!("no history for id '{}'", args.id)),
            Ok(log) => {
                let mut out = String::new();
                for rev in &log {
                    let text = rev
                        .item
                        .as_ref()
                        .map(|i| i.text.clone())
                        .unwrap_or_else(|| "(no readable item body at this step)".to_string());
                    out.push_str(&format!(
                        "seq {} {} by {} [{}]: {}\n",
                        rev.seq,
                        rev.kind,
                        rev.actor,
                        &rev.rev_hash[..rev.rev_hash.len().min(12)],
                        text
                    ));
                }
                Ok(out)
            }
            Err(e) => Err(e.to_string()),
        })
        .await
    }

    #[tool(description = "Retract an item by id (model::store::retract): it stops being live everywhere - session start, the moment of action, lookup - but NOTHING is deleted, and history still walks it. A reason is REQUIRED and a blank one is refused, because six weeks later nobody can tell a deliberate removal from an accident. Use this when a fact is simply WRONG or gone; use revise when it merely changed. A retraction is a decision: bringing the fact back is a fresh remember, never a revise of the tombstone.")]
    async fn retract(&self, Parameters(args): Parameters<RetractArgs>) -> String {
        if let Some(queued) = self.capture("retract", &args) {
            return queued;
        }
        self.blocking(move |s| {
            match model::store::retract(s, SESSION_ID, LINEAGE_ID, ACTOR, &args.id, &args.reason) {
                Ok(_) => Ok(format!("retracted {} ({})", args.id, args.reason.trim())),
                Err(e) => Err(format!("refused: {e}")),
            }
        })
        .await
    }

    #[tool(description = "Settle an item that has more than one current head (model::store::resolve), which is what `get` reports as DIVERGED - two machines revised the same fact without seeing each other. Name the revision hash that survives in 'keep' and EVERY other current head in 'discard'. The head set is recomputed under the write lock, so a head that appeared while you were deciding makes this fail loudly instead of silently dropping someone else's revision. Read the item's history first; never guess which head is real.")]
    async fn resolve(&self, Parameters(args): Parameters<ResolveArgs>) -> String {
        self.blocking(move |s| {
            match model::store::resolve(
                s,
                SESSION_ID,
                LINEAGE_ID,
                ACTOR,
                &args.id,
                &args.keep,
                &args.discard,
            ) {
                Ok(_) => Ok(format!("resolved {} onto {}", args.id, args.keep)),
                Err(e) => Err(format!("could not resolve: {e}")),
            }
        })
        .await
    }

    #[tool(description = "Who defines and who uses one symbol name (serve::lookup::where_used) - the question to ask BEFORE changing a function, a struct or a variable, because the reference list IS the blast radius. Answers with every definition site and every use site as file and line, plus which commit the index was read at. Resolution is by bare name only: two unrelated things sharing a name come back together, so open the files rather than treating the list as a conclusion.")]
    async fn where_used(&self, Parameters(args): Parameters<WhereUsedArgs>) -> String {
        let Some(code) = self.code.clone() else {
            return NO_CODE_INDEX.to_string();
        };
        let limit = args.limit.unwrap_or(30).clamp(1, 200);
        match serve::lookup::where_used(&code.index_db, &code.repo, &args.name, limit) {
            Err(e) => format!("where_used failed: {e}"),
            Ok(usage) => {
                if usage.defined_at.is_empty() && usage.referenced_at.is_empty() {
                    return format!(
                        "the index knows no symbol named '{}'. {}",
                        args.name,
                        drift_line(&usage.provenance)
                    );
                }
                let mut out = format!("{}\n", drift_line(&usage.provenance));
                out.push_str(&format!("defined at ({}):\n", usage.defined_at.len()));
                for s in &usage.defined_at {
                    out.push_str(&format!("  {}:{}\n", s.path, s.line));
                }
                out.push_str(&format!("used at ({}):\n", usage.referenced_at.len()));
                for s in &usage.referenced_at {
                    out.push_str(&format!("  {}:{}\n", s.path, s.line));
                }
                out
            }
        }
    }

    #[tool(description = "What one FILE defines, in line order (serve::lookup::outline) - a file's shape without reading the whole thing. Takes a repository-relative path. Says plainly when the index has never seen that path, which is a different answer from 'this file defines nothing' - a file that was added since the last index falls in the first case, and the drift line tells you when that was.")]
    async fn outline(&self, Parameters(args): Parameters<OutlineArgs>) -> String {
        let Some(code) = self.code.clone() else {
            return NO_CODE_INDEX.to_string();
        };
        match serve::lookup::outline(&code.index_db, &args.path) {
            Err(e) => format!("outline failed: {e}"),
            Ok(None) => format!(
                "'{}' is not in the code index - check the path is repository-relative, or the \
                 file was added after the index was last built",
                args.path
            ),
            Ok(Some(names)) if names.is_empty() => {
                format!("'{}' is indexed but defines no named symbols", args.path)
            }
            Ok(Some(names)) => {
                let mut out = format!("{} ({} definitions):\n", args.path, names.len());
                for (name, line) in &names {
                    out.push_str(&format!("  {line}: {name}\n"));
                }
                out
            }
        }
    }

    #[tool(description = "Search the indexed SOURCE CODE of the current project (serve::lookup::search_code), not the memory: a case-insensitive substring search over the code index, and every answer says which commit the text was read at and whether the working copy has moved on since. Use it to find where something is written, then read the real file - the index is an archive with provenance, never a substitute for opening the file. Answers plainly when no index is configured rather than returning nothing.")]
    async fn search_code(&self, Parameters(args): Parameters<SearchCodeArgs>) -> String {
        let Some(code) = self.code.clone() else {
            return NO_CODE_INDEX.to_string();
        };
        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        match serve::lookup::search_code(&code.index_db, &code.repo, &args.query, limit) {
            Err(e) => format!("code search failed: {e}"),
            Ok(answer) => {
                let drift = drift_line(&answer.provenance);
                if answer.hits.is_empty() {
                    return format!("no code matches '{}'. {drift}", args.query);
                }
                let mut out = format!("{drift}\n");
                for hit in &answer.hits {
                    out.push_str(&format!(
                        "{}:{}-{}\n{}\n\n",
                        hit.path, hit.start_line, hit.end_line, hit.text
                    ));
                }
                out
            }
        }
    }
}

/// What replaying one captured call did at the authority.
///
/// Deliberately two cases and not a bool: the drain has to be able to PRINT
/// what happened, and "refused because the falsifier is missing" is the only
/// thing that will ever tell the owner why a thought he wrote on his phone is
/// not in his memory.
#[derive(Debug, Clone)]
pub enum CapturedOutcome {
    /// The write landed. Carries the tool's own success line.
    Applied(String),
    /// The write did not land, with the reason: the gate refused it, the
    /// arguments would not parse, or this build does not know that tool.
    NotApplied(String),
}

impl ThorMcpServer {
    /// Replay one captured call against THIS store, as if the caller had made
    /// it here. The write gate runs now, at the authority, which is the whole
    /// reason the capture kept the CALL instead of a finished item.
    ///
    /// WHY THE CLASSIFICATION MATCHES THE SUCCESS SHAPE AND NOT A FAILURE
    /// SHAPE: the tools answer in prose, and their refusals share no single
    /// prefix ("REFUSED: " from the gate on remember/revise, "refused: " from
    /// retract, "invalid 'kind': " from argument parsing). Matching the three
    /// success lines instead means an answer nobody anticipated counts as NOT
    /// applied. That is the safe direction: a capture wrongly reported failed
    /// gets looked at, while one wrongly reported applied is simply gone.
    pub async fn apply_captured(&self, op: &InboxOp) -> CapturedOutcome {
        if self.inbox.is_some() {
            return CapturedOutcome::NotApplied(
                "this server is itself in replica divert mode, so replaying here would only \
                 re-queue the call. Drain into the authority's store, never into a replica."
                    .to_string(),
            );
        }
        let text = match op.tool.as_str() {
            "remember" => match serde_json::from_value::<RememberArgs>(op.args.clone()) {
                Ok(args) => self.remember(Parameters(args)).await,
                Err(e) => format!("arguments do not parse as a remember call: {e}"),
            },
            "revise" => match serde_json::from_value::<ReviseArgs>(op.args.clone()) {
                Ok(args) => self.revise(Parameters(args)).await,
                Err(e) => format!("arguments do not parse as a revise call: {e}"),
            },
            "retract" => match serde_json::from_value::<RetractArgs>(op.args.clone()) {
                Ok(args) => self.retract(Parameters(args)).await,
                Err(e) => format!("arguments do not parse as a retract call: {e}"),
            },
            other => format!(
                "unknown captured tool '{other}' - the replica that captured this runs a newer \
                 build than this authority. Update the authority and drain again; the capture is \
                 kept until it applies."
            ),
        };
        let landed =
            text.starts_with("stored ") || text.starts_with("revised ") || text.starts_with("retracted ");
        if landed {
            CapturedOutcome::Applied(text)
        } else {
            CapturedOutcome::NotApplied(text)
        }
    }
}

#[tool_handler]
impl ServerHandler for ThorMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(INSTRUCTIONS)
    }
}

/// Entry point for the `mcp` binary: open the store (never creating one -
/// see `EventStore::open_existing`'s own doc comment: a typo'd path must
/// never quietly produce a brand-new, healthy-looking empty store) and serve
/// stdio, blocking until the client disconnects.
pub fn run(db: &Path, code: Option<CodeIndexPaths>) -> anyhow::Result<()> {
    run_in(db, code, None)
}

/// `run`, told which checkout it was started in so the write gate's
/// neighbourhood toll has somewhere to run its checks (see
/// `model::store::declare_in`). `None` means the toll does not exist for
/// this session, exactly like every other check with no anchor to resolve.
pub fn run_in(db: &Path, code: Option<CodeIndexPaths>, root: Option<PathBuf>) -> anyhow::Result<()> {
    let store = EventStore::open_existing(db)?;
    let server = match code {
        Some(paths) => ThorMcpServer::with_code_index(store, paths),
        None => ThorMcpServer::new(store),
    }
    // Same sidecar path the CLI derives, from the one place that derives it.
    .with_vectors(serve::semantic_paths::default_vectors_path(db));
    let server = match root {
        Some(root) => server.with_root(root),
        None => server,
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

/// True when `bind` can only be reached from this machine. Anything else -
/// including `0.0.0.0`, which is every interface - is a remote surface.
fn is_loopback_bind(bind: &str) -> bool {
    let host = bind.rsplit_once(':').map(|(h, _)| h).unwrap_or(bind).trim_matches(['[', ']']);
    host == "127.0.0.1" || host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// Serve the same tools over Streamable-HTTP, for ONE purpose: a connector
/// that a session away from the main machine can reach.
///
/// WHY THIS REFUSES TO START WITHOUT A CAPTURE INBOX. A machine that is not
/// the authority must never append to its own log - its chain has to stay a
/// pure prefix of the authority's, or the next shipment is refused as a fork
/// and the only way back is rebuilding the replica (see `thor_core::inbox`).
/// 1.0 made divert mode an environment variable, which meant forgetting to
/// set it did not fail: it quietly produced exactly that fork. Here the port
/// simply does not open without somewhere to queue writes, so the dangerous
/// configuration cannot be reached by forgetting something.
///
/// The transport carries no auth of its own. Run it on a LAN or a private
/// tunnel, behind whatever gate the connector already sits behind - the same
/// rule as `ops::transport`, and the same reason.
pub fn run_http(
    db: &Path,
    bind: &str,
    code: Option<CodeIndexPaths>,
    inbox: Option<PathBuf>,
    allowed_hosts: Vec<String>,
) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    // WHY A REMOTE BIND MUST NAME ITS HOSTS. The transport refuses any
    // request whose `Host` header is not on an allow-list, and that list
    // defaults to loopback only - protection against a browser being tricked
    // into reaching a server that is meant to be local. A connector bound to
    // 0.0.0.0 for a NAS is exactly the case that default does not fit: it
    // came up healthy, logged nothing, and answered every real request with
    // `403 Forbidden: Host header is not allowed`. Deployed on 2026-08-06 and
    // found only by calling it. So the mistake is moved to startup, where it
    // is one line instead of an hour.
    if !is_loopback_bind(bind) && allowed_hosts.is_empty() {
        anyhow::bail!(
            "binding {bind} reaches beyond this machine, so every request will arrive with a Host              header this transport refuses by default (403), and nothing in the log will say so.              Name the address clients will actually use, e.g. --allowed-host 10.0.0.50 (a bare              host matches any port). Bind 127.0.0.1 instead if this was only ever meant to be local."
        );
    }

    let Some(inbox) = inbox else {
        anyhow::bail!(
            "refusing to open an HTTP surface without --capture-inbox. A machine reachable over \
             HTTP is not the authority, and a write applied there forks the log away from the \
             main store: the next ship is refused and the replica has to be rebuilt. Give a file \
             to queue writes into, and drain it from the main machine with `sync drain`."
        );
    };

    let store = EventStore::open_existing(db)?;
    let server = match code {
        Some(paths) => ThorMcpServer::with_code_index(store, paths),
        None => ThorMcpServer::new(store),
    }
    .with_vectors(serve::semantic_paths::default_vectors_path(db))
    .with_capture_inbox(inbox.clone());

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            LocalSessionManager::default().into(),
            {
                // Non-exhaustive upstream, so it is built by mutation rather
                // than by a struct literal: a new field there must never
                // become a compile error here.
                let mut cfg = StreamableHttpServerConfig::default();
                if !allowed_hosts.is_empty() {
                    cfg.allowed_hosts = allowed_hosts.clone();
                }
                cfg
            },
        );
        let app = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(bind).await?;
        let actual = listener.local_addr()?;
        println!("THOR replica connector listening on http://{actual}/mcp");
        println!(
            "writes are QUEUED to {} and applied when the main machine drains them - reads answer \
             from this copy of the store",
            inbox.display()
        );
        axum::serve(listener, app).await?;
        Ok::<(), anyhow::Error>(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Check, Kind as ItemKind, Severity as ItemSeverity};

    fn server() -> ThorMcpServer {
        ThorMcpServer::new(EventStore::in_memory().unwrap())
    }

    fn base_remember(id: &str) -> RememberArgs {
        RememberArgs {
            id: id.to_string(),
            kind: "rule".to_string(),
            text: "never force-push to main".to_string(),
            severity: Some("irreversible".to_string()),
            project: None,
            // Gate ground 11 asks every heavy rule whether it can refuse. This
            // one is bound Always, so there is no literal to forbid: banning
            // "--force" everywhere would refuse the sentence that documents the
            // rule. The tag is the honest answer, not a way around the gate.
            tags: vec![format!("{}a test fixture with nothing literal to catch", model::store::NO_LITERAL_REASON_PREFIX)],
            expires: None,
            key: None,
            falsifier: Some("a force-push to main lands clean, nobody reverts it".to_string()),
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: vec![],
            targets: vec![],
            always: true,
        }
    }

    // ----------------------------------------------------- refusal is loud

    #[tokio::test]
    async fn a_refused_remember_is_loud_and_writes_nothing() {
        // Named after the defect it prevents: a Rule with no falsifier must
        // be refused with the reason visible in the reply, and the store
        // must stay completely empty - never a silent partial write.
        let srv = server();
        let mut args = base_remember("no-falsifier-1");
        args.falsifier = None;
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("REFUSED"), "expected a loud refusal, got: {reply}");
        assert!(reply.contains("falsifier"), "expected the reason to name the missing field: {reply}");

        let events = srv.store.lock().unwrap().get_all_events().unwrap();
        assert!(events.is_empty(), "a refused remember must write nothing at all, found: {events:?}");
    }

    #[tokio::test]
    async fn a_refused_revise_that_drops_a_field_is_loud_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("drop-tags-1");
        // Ground 9 is what this test is about, so keep ground 11 out of it: a
        // heavy rule that loses its no-literal tag is refused for the teeth
        // question first, and the tags message never gets a turn.
        args.severity = Some("house_style".to_string());
        args.tags = vec!["safety".to_string()];
        let stored = srv.remember(Parameters(args)).await;
        assert!(stored.starts_with("stored"), "fixture setup must succeed: {stored}");

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "drop-tags-1".to_string(),
            text: None,
            severity: None,
            project: None,
            tags: Some(vec![]), // ground 9: existing had tags, this drops them
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.contains("REFUSED"), "expected a loud refusal, got: {reply}");
        assert!(reply.contains("tags"), "expected the reason to name the dropped field: {reply}");

        let events = srv.store.lock().unwrap().get_all_events().unwrap();
        assert_eq!(events.len(), 1, "a refused revise must append nothing beyond the original declare");
    }

    // ------------------------------------------------- notes that read a file

    fn item_with_contains(path: &str, literal: &str) -> Item {
        let mut item = model::item::Item {
            id: "probe".to_string(),
            kind: model::item::Kind::Rule,
            text: "some rule about this file".to_string(),
            bindings: vec![model::item::Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("the file stops saying it".to_string()),
            check: None,
        };
        item.check =
            Some(model::item::Check::Contains { path: path.to_string(), literal: literal.to_string() });
        item
    }

    /// The measured defect: a check matched a passing remark in a comment
    /// while the behaviour it claimed to guard lived in another file, so the
    /// rule looked enforced and reached nobody editing the real code.
    #[test]
    fn a_literal_only_in_a_comment_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn real() {}\n// mentions handleCreated in passing\n").unwrap();
        let notes = file_aware_warnings(&item_with_contains("a.rs", "handleCreated"), dir.path());
        assert!(notes.iter().any(|n| n.contains("only inside comments")), "{notes:?}");
    }

    /// The blind spot that had to be corrected by hand: reading a leading
    /// hash as a comment marker turns a Markdown heading and a C `#define`
    /// into commentary. That over-reported 28 hollow checks where 6 were
    /// real.
    #[test]
    fn a_markdown_heading_and_a_define_are_content_not_commentary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# How it works\nplain text\n").unwrap();
        std::fs::write(dir.path().join("pins.h"), "#define PANEL_R0 8\n").unwrap();

        let heading = file_aware_warnings(&item_with_contains("README.md", "# How it works"), dir.path());
        assert!(heading.is_empty(), "a heading is content: {heading:?}");
        let define = file_aware_warnings(&item_with_contains("pins.h", "#define PANEL_R0"), dir.path());
        assert!(define.is_empty(), "a preprocessor directive is code: {define:?}");
    }

    #[test]
    fn a_literal_that_is_everywhere_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.js"), "setTimeout();\n".repeat(30)).unwrap();
        let notes = file_aware_warnings(&item_with_contains("a.js", "setTimeout"), dir.path());
        assert!(notes.iter().any(|n| n.contains("occurs 30 times")), "{notes:?}");
    }

    #[test]
    fn a_file_that_is_not_there_produces_no_note_rather_than_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        assert!(file_aware_warnings(&item_with_contains("gone.rs", "anything"), dir.path()).is_empty());
    }

    /// A rule with no check whose own falsifier names a real file: the check
    /// nearly writes itself, and nobody thought to ask for one.
    #[test]
    fn a_falsifier_naming_a_real_file_suggests_a_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/gate.rs"), "fn gate() {}\n").unwrap();

        let mut item = item_with_contains("x", "y");
        item.check = None;
        item.falsifier = Some("src/gate.rs stops refusing an unbound rule".to_string());
        let notes = suggested_check(&item, dir.path());
        assert!(notes.iter().any(|n| n.contains("src/gate.rs")), "{notes:?}");
    }

    #[test]
    fn nothing_is_suggested_when_a_check_is_already_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/gate.rs"), "fn gate() {}\n").unwrap();

        let mut item = item_with_contains("src/gate.rs", "fn gate");
        item.falsifier = Some("src/gate.rs stops refusing an unbound rule".to_string());
        assert!(suggested_check(&item, dir.path()).is_empty());
    }

    /// It never guesses: a falsifier naming a file that is not there says
    /// nothing at all, because a suggested proof nobody can verify is worse
    /// than no suggestion.
    #[test]
    fn a_falsifier_naming_a_missing_file_suggests_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut item = item_with_contains("x", "y");
        item.check = None;
        item.falsifier = Some("src/never-existed.rs changes".to_string());
        assert!(suggested_check(&item, dir.path()).is_empty());
    }

    // ------------------------------------------------------- small edits

    async fn stored(srv: &ThorMcpServer, id: &str, text: &str) {
        let mut args = base_remember(id);
        args.text = text.to_string();
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.starts_with("stored "), "{reply}");
    }

    async fn body(srv: &ThorMcpServer, id: &str) -> String {
        let raw = srv.get(Parameters(GetArgs { id: id.to_string() })).await;
        serde_json::from_str::<Item>(&raw).expect(&format!("get must return JSON: {raw}")).text
    }

    #[tokio::test]
    async fn append_adds_to_the_end_without_retyping_the_body() {
        let srv = server();
        stored(&srv, "app-1", "the changelog lives in CHANGELOG.md at the repo root").await;
        let reply = srv
            .revise(Parameters(ReviseArgs {
                id: "app-1".to_string(),
                append: Some("and nowhere else".to_string()),
                ..Default::default()
            }))
            .await;
        assert!(reply.starts_with("revised "), "{reply}");
        assert_eq!(body(&srv, "app-1").await, "the changelog lives in CHANGELOG.md at the repo root and nowhere else");
    }

    #[tokio::test]
    async fn replace_patches_one_place_and_leaves_the_rest() {
        let srv = server();
        stored(&srv, "rep-1", "the changelog lives in CHANGELOG.md at the repo root").await;
        srv.revise(Parameters(ReviseArgs {
            id: "rep-1".to_string(),
            replace_from: Some("the repo root".to_string()),
            replace_to: Some("docs/".to_string()),
            ..Default::default()
        }))
        .await;
        assert_eq!(body(&srv, "rep-1").await, "the changelog lives in CHANGELOG.md at docs/");
    }

    /// The defect this prevents, and it is the quiet one: a substring that is
    /// not in the body would replace nothing and still report success, so the
    /// caller walks away believing the correction landed.
    #[tokio::test]
    async fn replacing_something_that_is_not_there_is_refused_not_silently_ignored() {
        let srv = server();
        stored(&srv, "rep-2", "the changelog lives in CHANGELOG.md at the repo root").await;
        let reply = srv
            .revise(Parameters(ReviseArgs {
                id: "rep-2".to_string(),
                replace_from: Some("never written".to_string()),
                replace_to: Some("x".to_string()),
                ..Default::default()
            }))
            .await;
        assert!(reply.contains("REFUSED"), "{reply}");
        assert!(reply.contains("nothing to replace"), "the refusal must say why: {reply}");
        assert_eq!(
            body(&srv, "rep-2").await,
            "the changelog lives in CHANGELOG.md at the repo root",
            "a refused edit must change nothing"
        );
    }

    #[tokio::test]
    async fn two_edit_modes_at_once_are_refused() {
        let srv = server();
        stored(&srv, "rep-3", "the changelog lives in CHANGELOG.md at the repo root").await;
        let reply = srv
            .revise(Parameters(ReviseArgs {
                id: "rep-3".to_string(),
                text: Some("a whole new body that says something else entirely".to_string()),
                append: Some("and more".to_string()),
                ..Default::default()
            }))
            .await;
        assert!(reply.contains("REFUSED"), "{reply}");
    }

    #[tokio::test]
    async fn replace_from_without_replace_to_is_refused() {
        let srv = server();
        stored(&srv, "rep-4", "the changelog lives in CHANGELOG.md at the repo root").await;
        let reply = srv
            .revise(Parameters(ReviseArgs {
                id: "rep-4".to_string(),
                replace_from: Some("CHANGELOG.md".to_string()),
                ..Default::default()
            }))
            .await;
        assert!(reply.contains("REFUSED"), "{reply}");
    }

    /// The property that makes the small edits safe: they are a convenience
    /// for TYPING, never a way past the gate. An append whose result breaks a
    /// rule is refused exactly as if the whole body had been retyped.
    #[tokio::test]
    async fn a_small_edit_still_faces_the_whole_gate() {
        let srv = server();
        stored(&srv, "rep-5", "the changelog lives in CHANGELOG.md at the repo root").await;
        let reply = srv
            .revise(Parameters(ReviseArgs {
                id: "rep-5".to_string(),
                append: Some("x".repeat(400)),
                ..Default::default()
            }))
            .await;
        assert!(reply.contains("REFUSED"), "an over-long result must be refused: {reply}");
        assert_eq!(
            body(&srv, "rep-5").await,
            "the changelog lives in CHANGELOG.md at the repo root",
            "and nothing may have been written"
        );
    }

    // ------------------------------------------------------- write-time notes

    /// The invariant that makes appending safe, and the reason a warning is
    /// never prepended: `apply_captured` decides whether a drained write
    /// LANDED by looking at this string's leading "stored "/"revised ". Put a
    /// note in front of that and every warned write reads as not-applied, so
    /// the queue replays writes that already succeeded.
    #[tokio::test]
    async fn a_warning_is_appended_and_never_disturbs_the_success_prefix() {
        let srv = server();
        let mut args = base_remember("warned-item");
        // Six characters: past the length the gate refuses outright, still
        // short enough to be the smell `check_warnings` reports. This is the
        // measured example - a check on "INSERT" that claimed to guard admin
        // settings and matched seven times, including inside "INSERT OR
        // REPLACE".
        args.check_kind = Some("contains".to_string());
        args.check_path = Some("src/main.rs".to_string());
        args.check_literal = Some("INSERT".to_string());

        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.starts_with("stored "), "the success prefix must survive: {reply}");
        assert!(reply.contains("note:"), "the warning must actually reach the caller: {reply}");
        assert!(!reply.contains("REFUSED"), "a warning must never read as a refusal: {reply}");

        let events = srv.store.lock().unwrap().get_all_events().unwrap();
        assert_eq!(events.len(), 1, "a warned write is still a write: it must have landed");
    }

    /// A clean item must stay clean: no note, no trailing whitespace, nothing
    /// for a reader to wonder about.
    #[tokio::test]
    async fn a_clean_write_gets_no_note_at_all() {
        let srv = server();
        let reply = srv.remember(Parameters(base_remember("clean-item"))).await;
        assert!(reply.starts_with("stored "), "{reply}");
        assert!(!reply.contains("note:"), "nothing to warn about, so nothing may be said: {reply}");
    }

    // ------------------------------------------------- remember/get round trip

    #[tokio::test]
    async fn remember_then_get_round_trips() {
        let srv = server();
        let reply = srv.remember(Parameters(base_remember("round-trip-1"))).await;
        assert!(reply.contains("stored 'round-trip-1'"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "round-trip-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).expect(&format!("get must return valid JSON: {get_reply}"));
        assert_eq!(item.id, "round-trip-1");
        assert_eq!(item.kind, ItemKind::Rule);
        assert_eq!(item.severity, Some(ItemSeverity::Irreversible));
    }

    #[tokio::test]
    async fn get_on_an_unknown_id_is_a_plain_error_not_a_blank_success() {
        let srv = server();
        let reply = srv.get(Parameters(GetArgs { id: "does-not-exist".to_string() })).await;
        assert!(!reply.trim().is_empty(), "a missing id must produce an honest message, not a blank reply");
        assert!(reply.contains("does-not-exist"), "{reply}");
    }

    // ---------------------------------------------------------------- revise

    #[tokio::test]
    async fn revise_updates_text_and_keeps_untouched_fields() {
        let srv = server();
        srv.remember(Parameters(base_remember("revise-1"))).await;

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "revise-1".to_string(),
            text: Some("never force-push to main, ever".to_string()),
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "revise-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.text, "never force-push to main, ever");
        assert_eq!(item.severity, Some(ItemSeverity::Irreversible), "untouched field must be kept");
        assert_eq!(item.bindings, vec![Binding::Always], "untouched bindings must be kept");
    }

    // ------------------------------------------------------------- check
    //
    // THE DEFECT THESE PREVENT: `check_kind`/`check_path`/`check_literal`
    // used to have no write surface at all - every real construction site
    // hardcoded `check: None` - so the read side, gate validation and the
    // serve-side write guard all existed for a field nobody could ever set.
    // Each test below is named after the exact failure mode it pins shut.

    #[tokio::test]
    async fn remember_accepts_a_path_exists_check_and_it_round_trips() {
        let srv = server();
        let mut args = base_remember("check-pathexists-1");
        args.text = "the changelog lives in CHANGELOG.md at the repo root".to_string();
        args.check_kind = Some("path_exists".to_string());
        args.check_path = Some("CHANGELOG.md".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.starts_with("stored"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-pathexists-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.check, Some(Check::PathExists { path: "CHANGELOG.md".to_string() }));
    }

    #[tokio::test]
    async fn remember_accepts_a_contains_check_and_it_round_trips() {
        let srv = server();
        let mut args = base_remember("check-contains-1");
        args.text = "the license file must always mention GPLv3 somewhere in it".to_string();
        args.check_kind = Some("contains".to_string());
        args.check_path = Some("LICENSE".to_string());
        args.check_literal = Some("GPLv3".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.starts_with("stored"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-contains-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.check, Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }));
    }

    #[tokio::test]
    async fn remember_accepts_an_absent_check_and_it_round_trips() {
        let srv = server();
        let mut args = base_remember("check-absent-1");
        args.text = "the main config file must never contain a hardcoded api key".to_string();
        args.check_kind = Some("absent".to_string());
        args.check_path = Some("config/app.toml".to_string());
        args.check_literal = Some("api_key=".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.starts_with("stored"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-absent-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(
            item.check,
            Some(Check::Absent { path: "config/app.toml".to_string(), literal: "api_key=".to_string() })
        );
    }

    #[tokio::test]
    async fn remember_refuses_check_kind_without_check_path_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-1");
        args.text = "a rule that names a check kind but no path at all".to_string();
        args.check_kind = Some("path_exists".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_path"), "expected the reason to name the missing field: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_contains_without_check_literal_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-2");
        args.text = "a rule that names contains but gives no literal to find".to_string();
        args.check_kind = Some("contains".to_string());
        args.check_path = Some("README.md".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_literal"), "expected the reason to name the missing field: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_absent_without_check_literal_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-3");
        args.text = "a rule that names absent but gives no literal to forbid".to_string();
        args.check_kind = Some("absent".to_string());
        args.check_path = Some("README.md".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_literal"), "expected the reason to name the missing field: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_path_exists_with_a_check_literal_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-4");
        args.text = "a rule that names path_exists but also gives a literal".to_string();
        args.check_kind = Some("path_exists".to_string());
        args.check_path = Some("README.md".to_string());
        args.check_literal = Some("GPLv3".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_literal"), "expected the reason to name the conflicting field: {reply}");
        assert!(reply.contains("path_exists"), "{reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_check_path_with_no_check_kind_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-5");
        args.text = "a rule that names a check path but no check kind at all".to_string();
        args.check_path = Some("README.md".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_kind"), "expected the reason to name the missing field: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_an_unknown_check_kind_name_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-6");
        args.text = "a rule that misspells its own check kind".to_string();
        args.check_kind = Some("bogus".to_string());
        args.check_path = Some("README.md".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("bogus"), "expected the reason to name the bad value: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn revise_that_omits_all_three_check_fields_keeps_an_existing_check() {
        let srv = server();
        let mut args = base_remember("check-keep-1");
        args.text = "the roadmap lives in ROADMAP.md at the repo root".to_string();
        args.check_kind = Some("path_exists".to_string());
        args.check_path = Some("ROADMAP.md".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-keep-1".to_string(),
            text: None,
            severity: Some("costly".to_string()), // change an unrelated field
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-keep-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.severity, Some(ItemSeverity::Costly), "the field that WAS mentioned must change");
        assert_eq!(
            item.check,
            Some(Check::PathExists { path: "ROADMAP.md".to_string() }),
            "omitting all three check fields must keep the existing check untouched"
        );
    }

    #[tokio::test]
    async fn revise_changing_only_text_does_not_lose_an_existing_check() {
        let srv = server();
        let mut args = base_remember("check-keep-2");
        args.text = "the roadmap lives in ROADMAP.md at the repo root".to_string();
        args.check_kind = Some("path_exists".to_string());
        args.check_path = Some("ROADMAP.md".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-keep-2".to_string(),
            text: Some("the roadmap lives in ROADMAP.md, updated every quarter".to_string()),
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-keep-2".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.text, "the roadmap lives in ROADMAP.md, updated every quarter");
        assert_eq!(
            item.check,
            Some(Check::PathExists { path: "ROADMAP.md".to_string() }),
            "a text-only revise must not lose the existing check"
        );
    }

    #[tokio::test]
    async fn revise_with_an_empty_check_kind_on_an_item_with_no_check_stays_cleared() {
        // The one case clearing can succeed against the write gate's own
        // field-preservation rule (ground 9, model::gate::revise): when
        // there was never a check to preserve in the first place,
        // existing.check.is_some() is false, so ground 9 never fires. See
        // this file's own note (in build_check's caller, `revise`) and the
        // task report for the populated-check case, which ground 9 refuses
        // unconditionally today - the same way it already refuses clearing
        // ANY other populated optional field (severity/project/tags/
        // expires/key/falsifier all show the identical shape in gate.rs).
        let srv = server();
        assert!(srv.remember(Parameters(base_remember("check-clear-noop-1"))).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-clear-noop-1".to_string(),
            text: None,
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: Some("".to_string()),
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-clear-noop-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.check, None);
    }

    #[tokio::test]
    async fn revise_clearing_check_kind_while_also_giving_check_path_is_refused_and_changes_nothing() {
        let srv = server();
        let mut args = base_remember("check-clear-refuse-1");
        args.text = "the roadmap lives in ROADMAP.md at the repo root".to_string();
        args.check_kind = Some("path_exists".to_string());
        args.check_path = Some("ROADMAP.md".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-clear-refuse-1".to_string(),
            text: None,
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: Some("".to_string()),
            check_path: Some("OTHER.md".to_string()),
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.contains("invalid check"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-clear-refuse-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(
            item.check,
            Some(Check::PathExists { path: "ROADMAP.md".to_string() }),
            "a refused clear attempt must change nothing"
        );
    }

    /// THE PROPERTY THIS PINS, UPDATED (task report, 2026-08-06): this test
    /// used to pin the OPPOSITE - `model::gate::revise`'s ground 9 (field
    /// preservation) refusing a deliberate clear of a populated `check`
    /// unconditionally, with no exception for "the caller meant to clear
    /// it". That was real, and empirically the same for severity/project/
    /// expires/key/falsifier too (see the six tests below this one) - but it
    /// contradicted the documented "empty string clears it" convention this
    /// crate's own INSTRUCTIONS and every one of these fields' `ReviseArgs`
    /// doc comments promise. The fix is `model::gate::ClearedFields`: ground
    /// 9 itself is untouched (still unconditional, still proven by
    /// `gate.rs`'s own `a_revise_that_drops_the_check_is_refused` test,
    /// unchanged) - this crate's `revise` handler now tells it, per field,
    /// which drops were asked for, so a deliberate clear succeeds while an
    /// unnamed one is still refused exactly as before (see `clearing_one_
    /// field_does_not_excuse_an_unnamed_field_from_also_vanishing` in
    /// `gate.rs` for that other half, proven at the gate layer).
    #[tokio::test]
    async fn revise_clearing_a_populated_check_succeeds() {
        let srv = server();
        let mut args = base_remember("check-clear-populated-1");
        args.text = "the roadmap lives in ROADMAP.md at the repo root".to_string();
        args.check_kind = Some("path_exists".to_string());
        args.check_path = Some("ROADMAP.md".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-clear-populated-1".to_string(),
            text: None,
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: Some("".to_string()), // deliberate clear
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "expected a deliberate clear to succeed, got: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-clear-populated-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.check, None, "a deliberate clear must actually clear the field");
    }

    // --------------------------------------------------------- absent_all
    //
    // The SET form of Absent (check_kind "absent_all", paired with
    // check_literals instead of check_literal): one rule that forbids
    // SEVERAL literals together as ONE item, rather than one near-duplicate
    // item per literal - see `Check::AbsentAll`'s own doc comment and this
    // task's own report (the owner's typography rule forbidding six
    // punctuation characters is the concrete case this exists for). Each
    // test below is named after the exact defect it prevents, mirroring the
    // single-literal check section above.

    #[tokio::test]
    async fn remember_accepts_an_absent_all_check_and_it_round_trips() {
        let srv = server();
        let mut args = base_remember("check-absent-all-1");
        args.text = "docs/STYLE.md never carries an em dash, en dash or ellipsis".to_string();
        args.check_kind = Some("absent_all".to_string());
        args.check_path = Some("docs/STYLE.md".to_string());
        args.check_literals = vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()];
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.starts_with("stored"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-absent-all-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(
            item.check,
            Some(Check::AbsentAll {
                path: "docs/STYLE.md".to_string(),
                literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()],
            }),
            "the stored item must carry the full literal set, not just one entry of it"
        );
    }

    #[tokio::test]
    async fn remember_refuses_absent_all_with_no_check_literals_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-7");
        args.text = "a rule that names absent_all but gives no literals to forbid".to_string();
        args.check_kind = Some("absent_all".to_string());
        args.check_path = Some("docs/STYLE.md".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_literals"), "expected the reason to name the missing field: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_giving_both_check_literal_and_check_literals_and_writes_nothing() {
        // One of the two refusal grounds this task adds at the argument-shape
        // layer (model::gate::build_check): a single literal and a set are
        // mutually exclusive ways to describe a check, never given together.
        let srv = server();
        let mut args = base_remember("check-refuse-8");
        args.text = "a rule that confuses the single and set literal forms".to_string();
        args.check_kind = Some("absent_all".to_string());
        args.check_path = Some("docs/STYLE.md".to_string());
        args.check_literal = Some("TODO".to_string());
        args.check_literals = vec!["FIXME".to_string()];
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_literal"), "expected the reason to name both conflicting fields: {reply}");
        assert!(reply.contains("check_literals"), "expected the reason to name both conflicting fields: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_check_literals_on_contains_and_writes_nothing() {
        // The other refusal ground this task adds: a SET given to a
        // check_kind that only ever takes a single literal.
        let srv = server();
        let mut args = base_remember("check-refuse-9");
        args.text = "a rule that gives contains a whole set instead of one literal".to_string();
        args.check_kind = Some("contains".to_string());
        args.check_path = Some("LICENSE".to_string());
        args.check_literals = vec!["GPLv3".to_string()];
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_literal"), "expected the reason to name check_literal: {reply}");
        assert!(reply.contains("contains"), "expected the reason to name the offending kind: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn revise_that_omits_all_four_check_fields_keeps_an_existing_absent_all_set_untouched() {
        // THE DEFECT THIS PREVENTS: `check_literals` is a `Vec`, not an
        // `Option` - if the "omit everything keeps the existing check"
        // condition in this crate's own `revise` forgot to also require
        // `args.check_literals.is_empty()`, an unrelated revise (changing
        // only, say, severity) would fall into the "replace the check
        // wholesale" branch instead, calling `build_check` with `check_kind:
        // None` and refusing the whole revise outright.
        let srv = server();
        let mut args = base_remember("check-keep-absent-all-1");
        args.text = "docs/STYLE.md never carries an em dash, en dash or ellipsis".to_string();
        args.check_kind = Some("absent_all".to_string());
        args.check_path = Some("docs/STYLE.md".to_string());
        args.check_literals = vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()];
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-keep-absent-all-1".to_string(),
            text: None,
            severity: Some("costly".to_string()), // change an unrelated field
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-keep-absent-all-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.severity, Some(ItemSeverity::Costly), "the field that WAS mentioned must change");
        assert_eq!(
            item.check,
            Some(Check::AbsentAll {
                path: "docs/STYLE.md".to_string(),
                literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()],
            }),
            "omitting all four check fields must keep the existing set untouched, not just one entry of it"
        );
    }

    #[tokio::test]
    async fn revise_replaces_a_single_literal_check_with_an_absent_all_set() {
        // The "replace wholesale" branch, proven for the set form: swapping
        // a Check::Absent for a Check::AbsentAll is an ordinary correction,
        // not a drop (mirrors `a_revise_that_changes_the_check_form_is_
        // accepted` in gate.rs, one layer down).
        let srv = server();
        let mut args = base_remember("check-upgrade-to-set-1");
        args.text = "docs/STYLE.md never carries an em dash".to_string();
        args.check_kind = Some("absent".to_string());
        args.check_path = Some("docs/STYLE.md".to_string());
        args.check_literal = Some("\u{2014}".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-upgrade-to-set-1".to_string(),
            text: Some("docs/STYLE.md never carries an em dash, en dash or ellipsis".to_string()),
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: Some("absent_all".to_string()),
            check_path: Some("docs/STYLE.md".to_string()),
            check_literal: None,
            check_literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-upgrade-to-set-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(
            item.check,
            Some(Check::AbsentAll {
                path: "docs/STYLE.md".to_string(),
                literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()],
            })
        );
    }

    // -------------------------------------------------------------- forbidden
    //
    // The path-less SET form (check_kind "forbidden", paired with
    // check_literals and NO check_path): a standing prohibition with
    // nothing to anchor to, unlike absent_all's own file-anchored set - see
    // `Check::Forbidden`'s own doc comment and this task's own report (the
    // owner's typography rule, applying everywhere rather than to one
    // directory as a currency-proof formality, is the concrete case this
    // exists for). Each test below is named after the exact defect it
    // prevents, mirroring the absent_all section above.

    #[tokio::test]
    async fn remember_accepts_a_forbidden_check_and_it_round_trips() {
        let srv = server();
        let mut args = base_remember("check-forbidden-1");
        args.text = "never write an em dash, en dash or ellipsis, anywhere".to_string();
        args.check_kind = Some("forbidden".to_string());
        args.check_literals = vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()];
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.starts_with("stored"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-forbidden-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(
            item.check,
            Some(Check::Forbidden {
                literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()],
            }),
            "the stored item must carry the full literal set, and no check_path at all"
        );
    }

    #[tokio::test]
    async fn remember_refuses_forbidden_with_no_check_literals_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-10");
        args.text = "a rule that names forbidden but gives no literals to forbid".to_string();
        args.check_kind = Some("forbidden".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_literals"), "expected the reason to name the missing field: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_forbidden_given_a_check_path_and_writes_nothing() {
        // THE GROUND UNIQUE TO THIS KIND: forbidden carries no path at all -
        // a check_path given alongside check_kind "forbidden" must be
        // refused by name, never silently accepted and dropped, and never
        // silently accepted and misread as an anchor it will never be.
        let srv = server();
        let mut args = base_remember("check-refuse-11");
        args.text = "a rule that gives forbidden a check_path it has no use for".to_string();
        args.check_kind = Some("forbidden".to_string());
        args.check_path = Some("docs/STYLE.md".to_string());
        args.check_literals = vec!["TODO".to_string()];
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_path"), "expected the reason to name the offending field: {reply}");
        assert!(reply.contains("forbidden"), "expected the reason to name the offending kind: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn remember_refuses_forbidden_given_a_single_check_literal_instead_of_a_list_and_writes_nothing() {
        let srv = server();
        let mut args = base_remember("check-refuse-12");
        args.text = "a rule that gives forbidden one literal instead of a set".to_string();
        args.check_kind = Some("forbidden".to_string());
        args.check_literal = Some("TODO".to_string());
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("check_literals"), "expected the reason to name the field to use instead: {reply}");
        assert!(srv.store.lock().unwrap().get_all_events().unwrap().is_empty(), "a refused remember must write nothing");
    }

    #[tokio::test]
    async fn revise_that_omits_all_four_check_fields_keeps_an_existing_forbidden_set_untouched() {
        // The Forbidden counterpart to `revise_that_omits_all_four_check_
        // fields_keeps_an_existing_absent_all_set_untouched` above: proves
        // the same "check_literals is a Vec, not an Option" omit convention
        // holds for the path-less form too.
        let srv = server();
        let mut args = base_remember("check-keep-forbidden-1");
        args.text = "never write an em dash, en dash or ellipsis, anywhere".to_string();
        args.check_kind = Some("forbidden".to_string());
        args.check_literals = vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()];
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-keep-forbidden-1".to_string(),
            text: None,
            severity: Some("costly".to_string()), // change an unrelated field
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-keep-forbidden-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.severity, Some(ItemSeverity::Costly), "the field that WAS mentioned must change");
        assert_eq!(
            item.check,
            Some(Check::Forbidden {
                literals: vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "\u{2026}".to_string()],
            }),
            "omitting all four check fields must keep the existing set untouched, not just one entry of it"
        );
    }

    #[tokio::test]
    async fn revise_replaces_an_absent_all_check_with_a_forbidden_one() {
        // The "replace wholesale" branch, proven across the one boundary
        // that is new here: FROM a check that carries a path TO one that
        // carries none at all - mirrors `revise_replaces_a_single_literal_
        // check_with_an_absent_all_set` above, and the same swap
        // `gate.rs`'s own `a_revise_that_swaps_a_path_carrying_check_for_a_
        // path_less_forbidden_one_is_accepted` proves one layer down; this
        // is the same property proven through the real MCP tool surface.
        let srv = server();
        let mut args = base_remember("check-downgrade-to-forbidden-1");
        args.text = "docs/STYLE.md never carries an em dash".to_string();
        args.check_kind = Some("absent_all".to_string());
        args.check_path = Some("docs/STYLE.md".to_string());
        args.check_literals = vec!["\u{2014}".to_string()];
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-downgrade-to-forbidden-1".to_string(),
            text: Some("never write an em dash, anywhere".to_string()),
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: Some("forbidden".to_string()),
            check_path: None,
            check_literal: None,
            check_literals: vec!["\u{2014}".to_string()],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-downgrade-to-forbidden-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.check, Some(Check::Forbidden { literals: vec!["\u{2014}".to_string()] }));
    }

    #[tokio::test]
    async fn revise_clearing_a_populated_forbidden_check_succeeds() {
        // The Forbidden counterpart to `revise_clearing_a_populated_check_
        // succeeds` (which uses PathExists): ground 9's field-preservation
        // check must not confuse a DELIBERATE clear with a silent drop for
        // the path-less form either.
        let srv = server();
        let mut args = base_remember("check-clear-populated-forbidden-1");
        args.text = "never write an em dash, anywhere".to_string();
        args.check_kind = Some("forbidden".to_string());
        args.check_literals = vec!["\u{2014}".to_string()];
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "check-clear-populated-forbidden-1".to_string(),
            text: None,
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: Some("".to_string()), // deliberate clear
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "check-clear-populated-forbidden-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.check, None, "a deliberate clear must actually clear the field");
    }

    // ---------------------------------------- clearing the other five fields
    //
    // Task report, 2026-08-06, JOB 1 then JOB 2. Before JOB 2's fix, each of
    // these five tests proved (empirically, through this same real
    // `ThorMcpServer::revise` path, not `gate::revise` in isolation) that a
    // deliberate clear was refused exactly like `check`'s above - the crate's
    // own INSTRUCTIONS constant, and every one of these fields' own
    // `ReviseArgs` doc comments, promise "omit keeps, empty string clears",
    // but ground 9 in `model/src/gate.rs` compared `existing`/`updated`
    // unconditionally, with no channel for "the caller asked for this" to
    // reach it. `model::gate::ClearedFields` is the fix (see `check`'s test
    // above for the full explanation) - each test below now proves the
    // documented convention actually holds, per field.

    #[tokio::test]
    async fn revise_clearing_a_populated_severity_succeeds() {
        let srv = server();
        let mut args = base_remember("clear-severity-1");
        // A second field, left alone by this revise, to prove clearing one
        // field never disturbs a sibling that was never named.
        args.project = Some("thor2".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "clear-severity-1".to_string(),
            text: None,
            severity: Some("".to_string()), // deliberate clear, per the documented convention
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "expected a deliberate clear to succeed, got: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "clear-severity-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.severity, None, "a deliberate clear must actually clear the field");
        assert_eq!(item.project, Some("thor2".to_string()), "a field nobody asked to clear must survive untouched");
    }

    #[tokio::test]
    async fn revise_clearing_a_populated_project_succeeds() {
        let srv = server();
        let mut args = base_remember("clear-project-1");
        // A REAL project name on purpose, never "global"/"null"/"-" - those
        // spellings already normalise to None on both sides of the
        // comparison before ground 9 ever runs (see `model/tests/the_
        // global_tier_is_never_a_project.rs`'s `repairing_an_already_
        // stored_global_is_not_a_dropped_field`), which would prove that
        // pre-existing repair path, not the ClearedFields one this test
        // means to prove.
        args.project = Some("thor2".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "clear-project-1".to_string(),
            text: None,
            severity: None,
            project: Some("".to_string()), // deliberate clear
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "expected a deliberate clear to succeed, got: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "clear-project-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.project, None, "a deliberate clear must actually clear the field");
    }

    /// THE DEFECT THIS PREVENTS: the documented way to clear a field being
    /// unreachable by the very agent the documentation is written for.
    /// Measured six times on 2026-08-06: an empty string does not survive
    /// the MCP call layer - the value disappears and the request never
    /// arrives as valid JSON. A promise nobody can use is worse than no
    /// promise, so a whitespace-only value clears too. No legitimate case is
    /// lost: none of the clearable fields has any use for a value made of
    /// spaces.
    #[tokio::test]
    async fn a_whitespace_only_value_clears_a_field_just_like_an_empty_string() {
        let srv = server();
        let mut args = base_remember("clear-project-ws");
        args.project = Some("thor2".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "clear-project-ws".to_string(),
            text: None,
            severity: None,
            project: Some("   ".to_string()), // the reachable spelling of a clear
            tags: None,
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "a whitespace-only value must clear, got: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "clear-project-ws".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.project, None, "the field must actually be cleared, not set to whitespace");
    }

    #[tokio::test]
    async fn revise_clearing_a_populated_expires_succeeds() {
        let srv = server();
        // Only a Report may carry expires (gate::declare ground 2) -
        // blank_report already sets expires: Some("2027-01-01").
        assert!(srv.remember(Parameters(blank_report("clear-expires-1"))).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "clear-expires-1".to_string(),
            text: None,
            severity: None,
            project: None,
            tags: None,
            expires: Some("".to_string()), // deliberate clear
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "expected a deliberate clear to succeed, got: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "clear-expires-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.expires, None, "a deliberate clear must actually clear the field");
    }

    #[tokio::test]
    async fn revise_clearing_a_populated_key_succeeds() {
        let srv = server();
        // A Rule on purpose, not a Lookup: a Lookup REQUIRES a key
        // (gate::declare ground 6), so clearing it there would still be
        // refused by declare(updated) regardless of ground 9, confounding
        // what this test means to isolate. Nothing refuses a non-Lookup
        // carrying a key - it is simply meaningless there, per
        // RememberArgs' own doc comment.
        let mut args = base_remember("clear-key-1");
        args.key = Some("some-lookup-key".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "clear-key-1".to_string(),
            text: None,
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: Some("".to_string()), // deliberate clear
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "expected a deliberate clear to succeed, got: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "clear-key-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.key, None, "a deliberate clear must actually clear the field");
    }

    #[tokio::test]
    async fn revise_clearing_a_populated_falsifier_succeeds() {
        let srv = server();
        // A Report on purpose, not a Rule/Orientation: those two REQUIRE a
        // falsifier (gate::declare ground 10), so clearing it there would
        // still be refused by declare(updated) regardless of ground 9,
        // confounding what this test means to isolate - a Report may carry
        // one, but is never required to.
        let mut args = blank_report("clear-falsifier-1");
        args.falsifier = Some("this incident happens again within a week".to_string());
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "clear-falsifier-1".to_string(),
            text: None,
            severity: None,
            project: None,
            tags: None,
            expires: None,
            key: None,
            falsifier: Some("".to_string()), // deliberate clear
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.starts_with("revised"), "expected a deliberate clear to succeed, got: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "clear-falsifier-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.falsifier, None, "a deliberate clear must actually clear the field");
    }

    /// THE PROPERTY THE SIX TESTS ABOVE DO NOT COVER: none of them can, by
    /// construction, exercise an ACCIDENTAL drop - the raw MCP arguments
    /// already distinguish "omitted" from "explicit empty string" before an
    /// `Item` is ever built, so nothing reaches `revise` able to drop a
    /// field without also setting its `ClearedFields` flag. That other half
    /// (a field named cleared never excuses a DIFFERENT, unnamed field from
    /// also vanishing) is proved directly against `model::gate::revise` and
    /// `ClearedFields::baseline` in `gate.rs`'s own `cleared_fields` tests -
    /// see `clearing_one_field_does_not_excuse_an_unnamed_field_from_also_
    /// vanishing` there. This test instead pins the ordinary, most likely
    /// accidental-drop shape AT THIS CRATE'S OWN BOUNDARY: a tags-clearing
    /// attempt (which carries no "empty clears it" convention at all - an
    /// empty list is always refused, on purpose) must still be refused
    /// exactly as it was before ClearedFields existed, proving the fix
    /// widened the door for exactly six fields and no others.
    #[tokio::test]
    async fn clearing_severity_in_the_same_revise_that_drops_tags_still_refuses_the_tags_drop() {
        let srv = server();
        let mut args = base_remember("clear-severity-keep-tags-refusal-1");
        // Same reason as the sibling test above: a light rule isolates ground 9.
        args.severity = Some("house_style".to_string());
        args.tags = vec!["safety".to_string()];
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let revise_args = ReviseArgs {
            append: None,
            replace_from: None,
            replace_to: None,
            id: "clear-severity-keep-tags-refusal-1".to_string(),
            text: None,
            severity: Some("".to_string()), // deliberate, legitimate clear
            project: None,
            tags: Some(vec![]),             // never had a clear convention - still a silent drop
            expires: None,
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: None,
            targets: None,
            always: None,
        };
        let reply = srv.revise(Parameters(revise_args)).await;
        assert!(reply.contains("REFUSED"), "expected the tags drop to still be refused, got: {reply}");
        assert!(reply.contains("tags"), "expected the reason to name the dropped field: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "clear-severity-keep-tags-refusal-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.tags, vec!["safety".to_string()], "a refused revise must change nothing, severity included");
        assert_eq!(item.severity, Some(ItemSeverity::HouseStyle), "a refused revise must change nothing");
    }

    // ----------------------------------------------------------- pin/unpin

    #[tokio::test]
    async fn pin_adds_the_always_binding_and_it_shows_up() {
        let srv = server();
        let mut args = base_remember("pin-1");
        args.always = false;
        // A project, because a source-file anchor on a GLOBAL item is refused
        // by ground 19: it would fire in every repository that has a file by
        // that name. Real callers name the project; so does this fixture.
        args.project = Some("some-project".to_string());
        args.targets = vec![TargetArg { kind: "path".to_string(), value: "src/pin_test.rs".to_string() }];
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let reply = srv.pin(Parameters(PinArgs { id: "pin-1".to_string() })).await;
        assert!(reply.starts_with("pinned"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "pin-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert!(item.bindings.contains(&Binding::Always), "a pinned item must carry the Always binding: {item:?}");
        assert!(
            item.bindings.iter().any(|b| matches!(b, Binding::Target { .. })),
            "pin must keep the item's other bindings, not replace them: {item:?}"
        );
    }

    #[tokio::test]
    async fn unpin_removes_the_always_binding_but_keeps_the_rest() {
        let srv = server();
        let mut args = base_remember("unpin-1");
        args.always = true;
        args.project = Some("some-project".to_string()); // see the note in the pin test
        args.targets = vec![TargetArg { kind: "path".to_string(), value: "src/unpin_test.rs".to_string() }];
        assert!(srv.remember(Parameters(args)).await.starts_with("stored"));

        let reply = srv.unpin(Parameters(UnpinArgs { id: "unpin-1".to_string() })).await;
        assert!(reply.starts_with("unpinned"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "unpin-1".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert!(!item.bindings.contains(&Binding::Always), "unpin must remove the Always binding: {item:?}");
        assert!(
            item.bindings.iter().any(|b| matches!(b, Binding::Target { .. })),
            "unpin must keep the item's other bindings: {item:?}"
        );
    }

    /// THE DEFECT THIS PREVENTS: unpinning an item whose ONLY binding is
    /// Always would leave it with none at all - a Rule/Orientation with no
    /// binding can never fire (see `model::gate::declare`, ground 1), so this
    /// must refuse loudly rather than silently create a rule nothing can ever
    /// trigger again.
    #[tokio::test]
    async fn unpin_that_would_empty_the_bindings_is_refused() {
        let srv = server();
        // base_remember is Always-bound only: no moments, no targets.
        assert!(srv.remember(Parameters(base_remember("unpin-2"))).await.starts_with("stored"));

        let reply = srv.unpin(Parameters(UnpinArgs { id: "unpin-2".to_string() })).await;
        assert!(reply.contains("REFUSED"), "expected a loud refusal, got: {reply}");
        assert!(reply.contains("no binding"), "expected the reason to name the empty-binding problem: {reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "unpin-2".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.bindings, vec![Binding::Always], "a refused unpin must change nothing");
    }

    #[tokio::test]
    async fn pinning_an_already_pinned_item_is_idempotent() {
        let srv = server();
        assert!(srv.remember(Parameters(base_remember("pin-2"))).await.starts_with("stored")); // already always: true

        let reply = srv.pin(Parameters(PinArgs { id: "pin-2".to_string() })).await;
        assert!(reply.contains("already pinned"), "{reply}");

        let get_reply = srv.get(Parameters(GetArgs { id: "pin-2".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).unwrap();
        assert_eq!(item.bindings, vec![Binding::Always], "pinning twice must never duplicate the binding");

        let events = srv.store.lock().unwrap().get_all_events().unwrap();
        assert_eq!(events.len(), 1, "an idempotent pin must not append a redundant revision");
    }

    // --------------------------------------------------------------- lookup

    #[tokio::test]
    async fn lookup_finds_across_project_boundaries() {
        // Named after the defect it prevents: session start scopes to a
        // project on purpose, but lookup (surface 4) must not - an item
        // declared under a DIFFERENT project must still come back.
        let srv = server();
        let mut a = RememberArgs {
            kind: "report".to_string(),
            expires: Some("2027-01-01".to_string()),
            falsifier: None,
            always: false,
            project: Some("project-a".to_string()),
            ..blank_report("cross-proj-a")
        };
        // Distinct text on purpose (past the near-duplicate refusal in
        // model::store::declare, which is keyed on kind + text and does not
        // itself look at project) - "filament" is what the lookup query
        // below matches on, and it appears in both without the two texts
        // being the same fact reworded.
        a.text = "shared filament tuning notes for PLA nozzle temperature".to_string();
        let mut b = RememberArgs { project: Some("project-b".to_string()), ..blank_report("cross-proj-b") };
        b.text = "shared filament tuning notes for ABS bed adhesion settings".to_string();
        b.expires = Some("2027-01-01".to_string());

        assert!(srv.remember(Parameters(a)).await.starts_with("stored"));
        assert!(srv.remember(Parameters(b)).await.starts_with("stored"));

        let reply = srv.lookup(Parameters(LookupArgs { query: Some("filament".to_string()), key: None })).await;
        assert!(reply.contains("cross-proj-a"), "{reply}");
        assert!(reply.contains("cross-proj-b"), "expected an item from a different project too: {reply}");
    }

    #[tokio::test]
    async fn lookup_by_key_answers_only_the_exact_lookup() {
        let srv = server();
        let mut lookup_item = blank_report("release-checklist-item");
        lookup_item.kind = "lookup".to_string();
        lookup_item.text = "the release checklist lives in RELEASE.md".to_string();
        lookup_item.key = Some("release-checklist".to_string());
        lookup_item.expires = None;
        assert!(srv.remember(Parameters(lookup_item)).await.starts_with("stored"));

        let hit = srv.lookup(Parameters(LookupArgs { query: None, key: Some("release-checklist".to_string()) })).await;
        assert!(hit.contains("RELEASE.md"), "{hit}");

        let miss = srv.lookup(Parameters(LookupArgs { query: None, key: Some("no-such-key".to_string()) })).await;
        assert!(miss.contains("no lookup found"), "{miss}");
    }

    #[tokio::test]
    async fn lookup_with_neither_query_nor_key_is_a_named_error() {
        let srv = server();
        let reply = srv.lookup(Parameters(LookupArgs { query: None, key: None })).await;
        assert!(reply.contains("query") && reply.contains("key"), "{reply}");
    }

    /// THE DEFECT THIS PREVENTS (GAP 1): before this server cached a loaded
    /// embedder (see `ThorMcpServer`'s own `embedder_cache` field and
    /// `serve::lookup::search_best_effort_cached`), every `lookup` call
    /// reloaded the ONNX model from disk from scratch, even on a long-lived
    /// server. This wires a (fresh, empty) vectors sidecar so `lookup` takes
    /// the SAME code path production does (`Some(path) =>
    /// search_best_effort_cached`/`search`, see the `lookup` tool's own
    /// match), then calls it twice and asserts the two answers agree - the
    /// regression guard this workspace's own convention prefers when a
    /// direct load-count is not practical to assert (see
    /// `serve/src/lookup.rs`'s `search_best_effort_cached_reuses_the_same_
    /// cache_slot_across_two_calls` for why a genuine "loaded exactly once"
    /// counter needs a real ONNX model this test suite deliberately does not
    /// ship, feature build or not).
    #[tokio::test]
    async fn two_consecutive_lookups_on_the_same_server_answer_identically() {
        let vectors_dir = tempfile::tempdir().unwrap();
        let vectors_path = vectors_dir.path().join("v.db");
        let srv = ThorMcpServer::new(EventStore::in_memory().unwrap()).with_vectors(vectors_path);
        srv.remember(Parameters(base_remember("cache-regression-1"))).await;

        let first = srv.lookup(Parameters(LookupArgs { query: Some("force-push".to_string()), key: None })).await;
        let second = srv.lookup(Parameters(LookupArgs { query: Some("force-push".to_string()), key: None })).await;
        assert_eq!(first, second, "two consecutive lookups on the same server must answer identically");
        assert!(first.contains("cache-regression-1"), "{first}");
    }

    fn blank_report(id: &str) -> RememberArgs {
        RememberArgs {
            id: id.to_string(),
            kind: "report".to_string(),
            text: "placeholder".to_string(),
            severity: None,
            project: None,
            tags: vec![],
            expires: Some("2027-01-01".to_string()),
            key: None,
            falsifier: None,
            check_kind: None,
            check_path: None,
            check_literal: None,
            check_literals: vec![],
            moments: vec![],
            targets: vec![],
            always: false,
        }
    }

    // ----------------------------------------------------------------- mark

        #[tokio::test]
    async fn mark_is_queryable_immediately_after_the_write() {
        let srv = server();
        srv.remember(Parameters(base_remember("mark-1"))).await;
        let reply = srv.mark(Parameters(MarkArgs { id: "mark-1".to_string(), noise: false })).await;
        assert!(reply.contains("marked useful: mark-1"), "{reply}");

        let store = srv.store.lock().unwrap();
        assert!(serve::usefulness::ever_marked_useful(&store).contains("mark-1"));
    }


    /// The noise side, end to end through the tool: two judgements take an
    /// item off the injection surfaces while leaving it in lookup. This is the
    /// only signal that retires anything - see `serve::decay` for the day a
    /// serving count was measured to be the wrong one.
    #[tokio::test]
    async fn two_noise_judgements_retire_an_item_but_leave_it_findable() {
        let srv = server();
        // A TARGET-bound rule on purpose: an Always-bound one is exempt from
        // decay by design (a pin is the owner's own standing choice), so
        // using the pinned fixture here would assert against the exemption
        // rather than against the noise signal.
        let mut args = base_remember("noisy-1");
        args.always = false;
        args.project = Some("some-project".to_string()); // see the note in the pin test
        args.targets = vec![TargetArg { kind: "path".to_string(), value: "src/noisy.rs".to_string() }];
        srv.remember(Parameters(args)).await;
        for _ in 0..serve::decay::NOISE_MARKS_BEFORE_STALE {
            let reply = srv.mark(Parameters(MarkArgs { id: "noisy-1".to_string(), noise: true })).await;
            assert!(reply.contains("marked noise"), "{reply}");
        }
        {
            let store = srv.store.lock().unwrap();
            let decay = serve::decay::DecayContext::load(&store);
            let item = model::store::show(&store, "noisy-1").unwrap();
            assert!(decay.is_stale(&item), "two judgements must retire it from the injection surfaces");
        }
        let found = srv.lookup(Parameters(LookupArgs { query: Some("noisy-1".to_string()), key: None })).await;
        assert!(found.contains("noisy-1"), "and it must stay findable via lookup: {found}");
    }

    // --------------------------------------------------------------- status

    #[tokio::test]
    async fn status_counts_every_kind_including_zero() {
        let srv = server();
        srv.remember(Parameters(base_remember("status-1"))).await;
        let reply = srv.status().await;
        assert!(reply.contains("Rule: 1"), "{reply}");
        assert!(reply.contains("Report: 0"), "a kind with nothing declared must still show 0: {reply}");
        assert!(reply.contains("fireable total (Rule + Orientation): 1"), "{reply}");
        assert!(reply.contains("declared, never fired: 1"), "{reply}");
    }

    // ------------------------------------------------------- input validation

    #[tokio::test]
    async fn an_unknown_kind_is_refused_by_name_not_a_panic() {
        let srv = server();
        let mut args = base_remember("bad-kind-1");
        args.kind = "bogus".to_string();
        let reply = srv.remember(Parameters(args)).await;
        assert!(reply.contains("bogus"), "{reply}");
        assert!(reply.contains("rule"), "expects the valid list to be named: {reply}");
    }

    // ---- Replica capture and replay -------------------------------------
    //
    // The half of the round trip that lives in this crate: a write arriving
    // at a replica is captured instead of applied, and the authority replays
    // that capture into its own log. The HTTP hop between the two halves is
    // proven separately in ops::transport.

    fn replica(inbox: &std::path::Path) -> ThorMcpServer {
        ThorMcpServer::new(EventStore::in_memory().unwrap()).with_capture_inbox(inbox.to_path_buf())
    }

    /// THE DEFECT THIS PREVENTS: a replica appending a remembered fact to its
    /// own log. Its chain would stop being a prefix of the authority's, and
    /// the very next shipment would be refused as a fork - with no way back
    /// except rebuilding the replica from scratch.
    #[tokio::test]
    async fn a_write_at_a_replica_is_queued_and_never_touches_its_own_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        let srv = replica(&path);

        let reply = srv.remember(Parameters(base_remember("from-the-road"))).await;
        assert!(reply.starts_with("queued for the main machine"), "{reply}");

        let stored = srv.get(Parameters(GetArgs { id: "from-the-road".to_string() })).await;
        assert!(
            !stored.contains("\"id\""),
            "the replica must NOT hold the item - it was queued, not written: {stored}"
        );

        let queued = inbox::read_all(&path).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].tool, "remember");
        assert_eq!(queued[0].args["id"], "from-the-road");
    }

    /// The round trip that matters: what the replica captured becomes a real
    /// item at the authority, identical to one written there directly.
    #[tokio::test]
    async fn a_captured_write_replays_into_the_authority_as_a_normal_item() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        replica(&path).remember(Parameters(base_remember("captured-then-real"))).await;

        let authority = server();
        let op = &inbox::read_all(&path).unwrap()[0];
        match authority.apply_captured(op).await {
            CapturedOutcome::Applied(text) => assert!(text.contains("stored 'captured-then-real'"), "{text}"),
            CapturedOutcome::NotApplied(text) => panic!("a valid capture must apply: {text}"),
        }

        let get_reply = authority.get(Parameters(GetArgs { id: "captured-then-real".to_string() })).await;
        let item: Item = serde_json::from_str(&get_reply).expect(&get_reply);
        assert_eq!(item.id, "captured-then-real");
        assert_eq!(item.kind, ItemKind::Rule);
    }

    /// THE DEFECT THIS PREVENTS: the gate being skipped on the way in through
    /// the back door. A capture is a normal call arriving late, so a rule the
    /// gate would refuse in a local session has to be refused here too - and
    /// the reason has to survive, or nobody can tell why a thought written on
    /// a phone is missing.
    #[tokio::test]
    async fn a_capture_the_gate_refuses_is_reported_with_its_reason_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        let mut bad = base_remember("no-falsifier-here");
        bad.falsifier = None;
        replica(&path).remember(Parameters(bad)).await;

        let authority = server();
        let op = &inbox::read_all(&path).unwrap()[0];
        match authority.apply_captured(op).await {
            CapturedOutcome::Applied(t) => panic!("the gate must refuse a rule with no falsifier: {t}"),
            CapturedOutcome::NotApplied(text) => {
                assert!(
                    text.to_lowercase().contains("falsifier"),
                    "the refusal must name what was wrong: {text}"
                );
            }
        }
    }

    /// A capture from a newer replica must not be silently dropped, and must
    /// not look like it applied either.
    #[tokio::test]
    async fn a_tool_this_build_does_not_know_is_reported_not_ignored() {
        let op = InboxOp::new("teleport", serde_json::json!({ "id": "x" }));
        match server().apply_captured(&op).await {
            CapturedOutcome::Applied(t) => panic!("an unknown tool must never count as applied: {t}"),
            CapturedOutcome::NotApplied(text) => assert!(text.contains("teleport"), "{text}"),
        }
    }

    // ---- The neighbourhood toll, through the door the agent uses ---------
    //
    // `model::store` has nine tests for the toll itself. What none of those
    // can reach is the WIRING: a root resolved at startup, carried on the
    // server, handed to `declare_in` on every remember. That chain is
    // exactly where this project has been bitten before - a mechanism that
    // works perfectly while nothing calls it - so it gets its own test at
    // the surface an agent actually touches.

    fn anchored_args(id: &str) -> RememberArgs {
        let mut a = base_remember(id);
        a.kind = "orientation".to_string();
        a.severity = None;
        a.always = false;
        a.targets = vec![TargetArg { kind: "path".to_string(), value: "noted.md".to_string() }];
        a.text = format!("something worth knowing about {id}");
        a
    }

    /// THE DEFECT THIS PREVENTS: the toll existing in the model crate and
    /// never once firing for the agent, because the root never made it from
    /// the process to the write. Silent, and indistinguishable from a store
    /// that simply has no rot in it.
    #[tokio::test]
    async fn the_toll_reaches_the_agents_own_remember() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("noted.md"), "rewritten, and it says nothing of the sort
").unwrap();
        let srv = ThorMcpServer::new(EventStore::in_memory().unwrap()).with_root(dir.path().to_path_buf());

        // A fact on noted.md whose own proof will come out false.
        let mut stale = anchored_args("goes-stale");
        stale.check_kind = Some("contains".to_string());
        stale.check_path = Some("noted.md".to_string());
        stale.check_literal = Some("the old name".to_string());
        let stored = srv.remember(Parameters(stale)).await;
        assert!(stored.starts_with("stored "), "{stored}");

        // Now anything else on that same target is refused, by name.
        let refusal = srv.remember(Parameters(anchored_args("the-next-one"))).await;
        assert!(refusal.starts_with("REFUSED"), "the toll must fire through this door: {refusal}");
        assert!(refusal.contains("goes-stale"), "and name what to settle: {refusal}");

        let missing = srv.get(Parameters(GetArgs { id: "the-next-one".to_string() })).await;
        assert!(!missing.contains("\"id\""), "nothing may be written when the toll refuses: {missing}");
    }

    /// The same call from a server that was never told where the code is
    /// must go straight through. Every session outside a checkout depends on
    /// that, and a toll firing there would make this memory unwritable from
    /// anywhere but one directory.
    #[tokio::test]
    async fn a_server_with_no_root_never_tolls_the_agent() {
        let srv = server();
        let mut stale = anchored_args("goes-stale");
        stale.check_kind = Some("contains".to_string());
        stale.check_path = Some("noted.md".to_string());
        stale.check_literal = Some("the old name".to_string());
        assert!(srv.remember(Parameters(stale)).await.starts_with("stored "));

        let second = srv.remember(Parameters(anchored_args("the-next-one"))).await;
        assert!(second.starts_with("stored "), "with no root there is nothing to check: {second}");
    }

    /// THE DEFECT THIS PREVENTS: a connector that comes up healthy, logs
    /// nothing, and answers every real request with 403 because its Host
    /// allow-list defaults to loopback. Deployed to a NAS on 2026-08-06 and
    /// found only by calling it from another machine - which is exactly the
    /// class of failure this project keeps saying it will not ship.
    #[test]
    fn a_remote_bind_without_an_allowed_host_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        thor_core::event_store::EventStore::new(&db).unwrap();
        let err = run_http(&db, "0.0.0.0:5557", None, Some(dir.path().join("inbox.jsonl")), vec![])
            .expect_err("a bind beyond loopback with no allowed host must not start");
        let msg = err.to_string();
        assert!(msg.contains("403"), "the message must name what would go wrong: {msg}");
        assert!(msg.contains("--allowed-host"), "and how to fix it: {msg}");
    }

    #[test]
    fn loopback_binds_are_recognised_and_anything_else_is_remote() {
        for local in ["127.0.0.1:5557", "localhost:5557", "[::1]:5557", "127.0.0.1"] {
            assert!(is_loopback_bind(local), "{local} is local");
        }
        for remote in ["0.0.0.0:5557", "10.0.0.50:5557", "nas:5557"] {
            assert!(!is_loopback_bind(remote), "{remote} reaches beyond this machine");
        }
    }

    /// THE DEFECT THIS PREVENTS: pointing the drain at a replica by mistake.
    /// Replaying there would re-queue every capture into the file it came
    /// from, forever, while reporting progress.
    #[tokio::test]
    async fn replaying_into_a_replica_is_refused_rather_than_re_queued() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        let srv = replica(&path);
        let op = InboxOp::new("remember", serde_json::json!({ "id": "x", "kind": "rule", "text": "t" }));
        match srv.apply_captured(&op).await {
            CapturedOutcome::Applied(t) => panic!("a replica must not replay: {t}"),
            CapturedOutcome::NotApplied(text) => assert!(text.contains("divert mode"), "{text}"),
        }
        assert!(!path.exists(), "the refused replay must not have queued anything");
    }
}
