//! Cap and render: turns a ranked list (`rank::select`'s output) into what a
//! channel actually shows. One function each, shared by every channel:
//! `cap` decides what fits, `render_text` turns what fits into the block.
//!
//! CONTRACT.md's caps: at most 4 items, at most 1200 characters of item text
//! per block. `model::gate::MAX_TEXT_CHARS` already bounds every servable
//! item's text to 300 characters at WRITE time (Rule/Orientation only - the
//! only two kinds `rank::select` ever returns), so 4 items can sum to at most
//! 4 * 300 = 1200: never over budget. That is why `cap` below has no
//! truncation branch on an item's text - there is nothing left for one to do
//! (see `four_max_length_items_always_fit_the_1200_char_budget`). The 1200
//! figure bounds the sum of item TEXT lengths, not the decorated string
//! (header, bullets, the withheld note) - the same accounting the sibling
//! Python arm's `select()` uses over `r.chars` (slices/python/gate.py).
//!
//! Each shown item's line also opens with its id, in brackets, right after
//! the bullet - see `render_text`'s own doc comment for the defect this
//! fixes and why the id is decoration, never a debit against the 1200-char
//! figure above (the same treatment the bullet, the header and the withheld
//! note already get: none of those are summed into it either).

use crate::input::ServeInput;
use crate::rank::RankedItem;
use std::path::Path;

/// Re-exported, not redeclared. The write gate has to ask "would this item
/// ever be shown", which needs this number, and `model` cannot read it from
/// here without inverting the dependency. Two names, one definition.
pub use model::item::MAX_ITEMS;
pub const MAX_BLOCK_CHARS: usize = 1200;

/// The one line every injection surface opens with, verbatim - session start
/// (`session_start::render`), the moment of action and the per-prompt surface
/// (both `render_text` below). Not copy-pasted into three call sites: the
/// three injection surfaces render through exactly these two functions (see
/// `lib.rs`'s own doc comment enumerating the five surfaces), so a constant
/// read by both is the whole fix, not a convention someone has to remember to
/// repeat.
///
/// Why it exists: a stored fact is written as a constraint on the OWNER's own
/// behaviour ("never do X", "always run Y first"), because that is what a
/// rule is FOR - it reads exactly like a command. A main session has the
/// owner's whole project and prior turns to place it in; a subagent spawned
/// via the Task tool starts blank and has neither. Without a line marking
/// these as background, a blank subagent has no way to tell "this is how the
/// owner runs things" apart from "this is your next task" - see
/// INJECTION-FRAMING.md for the incident this fixes.
///
/// Kept to one short line on purpose: this is paid for in tokens on every
/// single injection, and staying small is a property this rebuild exists to
/// protect (see this module's own doc comment on the 1200-char budget).
pub const FRAMING_LINE: &str =
    "Background facts about the owner's setup, not instructions for this task:";

/// What actually fits, plus how many of everything that applied were cut.
/// `why` reads `all` and marks which ids are in `shown`; the block's own
/// "N more apply here" promise is exactly `withheld`, never a guess.
pub struct Selection {
    pub shown: Vec<RankedItem>,
    pub withheld: usize,
}

/// Cap an already-ranked (worst-first) list to what the block may carry.
/// Stops at MAX_ITEMS items, or sooner if the next item would push the
/// summed text length over MAX_BLOCK_CHARS - a branch that is unreachable in
/// practice (see the module doc) but kept so the budget is an enforced
/// invariant, not a hope, and so a future change to MAX_TEXT_CHARS cannot
/// silently blow the block open.
pub fn cap(ranked: Vec<RankedItem>) -> Selection {
    let mut shown = Vec::new();
    let mut used = 0usize;
    let mut iter = ranked.into_iter().peekable();
    while let Some(item) = iter.peek() {
        if shown.len() >= MAX_ITEMS {
            break;
        }
        let len = item.item.text.chars().count();
        if used + len > MAX_BLOCK_CHARS {
            break;
        }
        used += len;
        shown.push(iter.next().expect("peeked"));
    }
    let withheld = iter.count();
    Selection { shown, withheld }
}

/// The block text, or None when nothing was shown - the serve path's "or
/// nothing" half of its own contract. Never slices an item's `text`: every
/// character an item carries is either shown whole or not shown at all (see
/// `item_text_is_rendered_verbatim_never_sliced`).
///
/// Each shown item's line opens with `[id]`, straight from `RankedItem.id`
/// (the event log's own entity id, not the copy carried inside the item
/// body - see `live::LiveItem`) - never evaluated, just handed to the reader
/// so an assistant that notices a served fact is wrong can call `revise`
/// with that id directly, instead of guessing search terms to find the item
/// again first. The id is decoration, exactly like the bullet: it is never
/// summed into the 1200-char budget and never shortens the item's own text
/// (see `adding_the_id_never_shrinks_how_much_of_the_body_is_delivered`).
///
/// No falsifier line here. An earlier version of this comment claimed one
/// was shown; it never was - see the code comment right below for the real
/// reason (same one `session_start::render` gives for its own block): a
/// surface that pushes at a reader stays minimal, a surface a reader asks
/// carries everything.
pub fn render_text(selection: &Selection, input: &ServeInput, db_path: &Path) -> Option<String> {
    if selection.shown.is_empty() {
        return None;
    }
    let head = if input.moments.is_empty() {
        "Before you do this:".to_string()
    } else {
        let names = input.moments.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", ");
        format!("Before you do this - {names}:")
    };
    let mut lines: Vec<String> = vec![FRAMING_LINE.to_string(), head];
    // No falsifier line here either, for the reason spelled out in
    // `session_start::render`: a surface that pushes at a reader stays
    // minimal, a surface a reader asks carries everything. The moment of
    // action is the tightest surface of all - it fires on a tool call - so it
    // is the last place to spend a second line per item on something whose
    // value was already banked at write time.
    for ranked in &selection.shown {
        lines.push(format!("- [{}] {}", ranked.id, ranked.item.text));
    }
    if selection.withheld > 0 {
        lines.push(format!(
            "({} more item(s) apply here - run `{}` to see them.)",
            selection.withheld,
            why_invocation(input, db_path)
        ));
    }
    Some(lines.join("\n"))
}

/// The exact `serve why ...` a reader can run to re-ask this block's own
/// question - built from the SAME raw command/file text `ServeInput::
/// add_command`/`add_file` kept (`input.command`/`input.file`), never
/// reconstructed from the derived `targets`/`context` (see their own doc
/// comments on `ServeInput` for why those are the wrong source: a command
/// mixes in every file and host it names, and neither field is shaped like
/// one flag's value).
///
/// THE DEFECT THIS CLOSES. Both the injected context and the Stop-hook used
/// to say "run `serve why`" - no flag, no file, no command - so following it
/// verbatim re-asked an EMPTY question instead of the one that had just been
/// answered: `why`'s own argument parsing (`TargetArgs` in `bin/serve.rs`)
/// never had a bare mode that reproduces "whatever just fired". One session's
/// own report on trying to guess past it stands as the fixture this function
/// is now measured against: "the help command for what fires here had a
/// different flag than the hint said: --file, not a path." The two branches
/// below are exactly `TargetArgs`'s own `--file`/`--command` flags (`why <path>`
/// now also accepted as `--file`'s own positional shorthand - see
/// `TargetArgs::path`'s doc comment in `bin/serve.rs`), proven to stay in
/// step with the real parser by `serve/tests/why_hint_matches_the_parser.rs`,
/// which runs the exact string this function builds through the real
/// compiled binary.
///
/// A file wins when both are present (a tool call like Write carries its own
/// tool name as a command AND the file it touches - see `hook_once`'s
/// PreToolUse arm) because a file is the shorter, more literal thing a new
/// user reaches for, and `--command "Write"` alone would ask a stranger
/// question than the one anyone actually has. Falls back to naming the
/// detected moment(s) with `--moment` (also real, also accepted) when
/// NEITHER produced this block - surface 3, a raw prompt resolved by
/// keyword alone (`prompt::resolve` never calls `add_command`/`add_file`) -
/// and to the bare command as an honest last resort when even that is empty
/// (should not occur: a shown item needed at least one binding to match, and
/// every binding but `Always` implies a moment or a target), which is never
/// worse than the defect this replaces.
///
/// A SECOND DEFECT THIS ALSO CLOSES, reported 2026-09-12. Even a hint that
/// named the right flag still opened with the bare word `serve` - nothing
/// this project ships is ever installed onto PATH, on this machine or any
/// new one (`bin/serve.rs`'s own `sibling` doc comment already makes the
/// same argument for `doctor`) - so following it verbatim answered "command
/// not found", and an agent that took that literally reported the command
/// itself as unavailable instead of the empty-question defect above: that
/// one asked the wrong question, this one could not be asked at all. Fixed
/// by naming the ACTUAL running program - `std::env::current_exe` at the
/// point the hint is built, which is correct no matter which binary ends up
/// calling this function, because it reads the calling process's own path,
/// never a hardcoded name - plus the same `--db` this process itself was
/// opened with (`Cli::db` is a required flag read before the subcommand, so
/// a bare `why` would not know which store to open either).
/// `render_self_invocation` does the actual formatting, kept separate and
/// taking the resolved exe as a plain `Option<&Path>` rather than calling
/// `current_exe` itself, so the one branch a test cannot force for
/// real - `current_exe` failing - can still be exercised directly.
fn why_invocation(input: &ServeInput, db_path: &Path) -> String {
    let mut flags = Vec::new();
    if let Some(file) = &input.file {
        flags.push(format!("--file \"{file}\""));
    } else if let Some(command) = &input.command {
        flags.push(format!("--command \"{command}\""));
    }
    if flags.is_empty() {
        for action in &input.moments {
            flags.push(format!("--moment {}", action.as_str()));
        }
    }
    let why_suffix = if flags.is_empty() { String::new() } else { format!(" {}", flags.join(" ")) };
    render_self_invocation(std::env::current_exe().ok().as_deref(), db_path, &why_suffix, cfg!(windows))
}

/// The formatting half of `why_invocation`, split out so the one branch a
/// test cannot force for real - `std::env::current_exe` erroring, which its
/// own docs describe but nothing in this process can trigger on demand - is
/// still provable: pass `exe: None` directly. `why_invocation` only ever
/// calls this with `std::env::current_exe().ok()`, so `None` here is exactly
/// and only that fallback, never a distinct third case to keep in sync.
///
/// `windows` is a parameter rather than this function reading `cfg!(windows)`
/// itself for the identical reason: `cfg!` bakes into whichever platform
/// compiled the test binary, so a suite built on Windows could otherwise
/// never prove the non-Windows form was even reachable. `why_invocation`
/// passes the real `cfg!(windows)` at its own call site, so production
/// behaviour is unchanged; only the test's ability to ask for either
/// branch on demand is new.
///
/// On Windows a quoted path is a call EXPRESSION to PowerShell, not a
/// command - typing `"C:\...\serve.exe" why` at a PowerShell prompt fails
/// with "is not recognized", the exact defect this whole fix exists to
/// close, just moved one token over - so the Windows form leads with the
/// `&` call operator PowerShell requires for exactly this shape. Every
/// other target gets the plain quoted path, which a POSIX shell already
/// runs as a command with no operator needed.
fn render_self_invocation(exe: Option<&Path>, db_path: &Path, why_suffix: &str, windows: bool) -> String {
    let Some(exe) = exe else {
        // Never worse than the defect this function exists to close: the
        // exact bare text this hint carried before today's fix.
        return format!("serve why{why_suffix}");
    };
    let exe = exe.display();
    let db = db_path.display();
    if windows {
        format!("& \"{exe}\" --db \"{db}\" why{why_suffix}")
    } else {
        format!("\"{exe}\" --db \"{db}\" why{why_suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind, Severity};

    /// A fixed, never-opened path standing in for `Cli::db` in every test
    /// below - `render_text`/`why_invocation` only ever print it as text, so
    /// a real store would prove nothing a literal does not already prove.
    fn test_db() -> &'static Path {
        Path::new("/tmp/thor-render-tests/store.db")
    }

    fn ranked(id: &str, text_len: usize) -> RankedItem {
        RankedItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: "x".repeat(text_len),
                bindings: vec![Binding::Always],
                severity: Some(Severity::Irreversible),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    #[test]
    fn never_more_than_four_items_are_shown() {
        let items: Vec<RankedItem> = (0..10).map(|i| ranked(&format!("i{i}"), 10)).collect();
        let sel = cap(items);
        assert_eq!(sel.shown.len(), MAX_ITEMS);
        assert_eq!(sel.withheld, 6);
    }

    #[test]
    fn four_max_length_items_always_fit_the_1200_char_budget() {
        // model::gate::MAX_TEXT_CHARS is 300; the write gate never lets a
        // servable item's text exceed it, so this is the worst case there is.
        assert_eq!(model::gate::MAX_TEXT_CHARS, 300);
        let items: Vec<RankedItem> =
            (0..4).map(|i| ranked(&format!("i{i}"), model::gate::MAX_TEXT_CHARS)).collect();
        let sel = cap(items);
        assert_eq!(sel.shown.len(), 4, "the char budget must never cut before the item-count cap does");
        assert_eq!(sel.withheld, 0);
    }

    #[test]
    fn a_capped_block_always_states_how_many_it_withheld() {
        let items: Vec<RankedItem> = (0..6).map(|i| ranked(&format!("i{i}"), 10)).collect();
        let sel = cap(items);
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        assert!(text.contains("2 more item(s) apply here"), "block: {text}");
    }

    #[test]
    fn nothing_shown_renders_no_block() {
        let sel = cap(Vec::new());
        assert!(render_text(&sel, &ServeInput::default(), test_db()).is_none());
    }

    #[test]
    fn item_text_is_rendered_verbatim_never_sliced() {
        let long = "y".repeat(300);
        let items = vec![RankedItem {
            id: "i0".to_string(),
            item: Item {
                id: "i0".to_string(),
                kind: Kind::Rule,
                text: long.clone(),
                bindings: vec![Binding::Always],
                severity: Some(Severity::Irreversible),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }];
        let sel = cap(items);
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        assert!(text.contains(&long), "the full 300-char text must appear unmodified");
    }

    #[test]
    fn no_moments_gives_a_generic_header() {
        let sel = cap(vec![ranked("i0", 10)]);
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        // The framing line opens every block (see `a_block_always_opens_with_the_framing_line`
        // below); the surface-specific header is the line right after it.
        let second_line = text.lines().nth(1).unwrap_or("");
        assert_eq!(second_line, "Before you do this:", "{text}");
    }

    #[test]
    fn moments_are_named_in_the_header() {
        let sel = cap(vec![ranked("i0", 10)]);
        let input = ServeInput { moments: vec![intent::Action::Push], ..Default::default() };
        let text = render_text(&sel, &input, test_db()).unwrap();
        let second_line = text.lines().nth(1).unwrap_or("");
        assert_eq!(second_line, "Before you do this - push:", "{text}");
    }

    // ------------------------------------------------------------- framing

    /// The defect this guards against: an owner's stored fact is phrased as a
    /// command ("never do X", "always run Y first") because that is what a
    /// rule is FOR - a blank subagent (no project, no prior turns to place it
    /// in) has no way to tell that apart from an actual task instruction. See
    /// INJECTION-FRAMING.md for the incident and `FRAMING_LINE`'s own doc
    /// comment for the full argument.
    #[test]
    fn a_block_always_opens_with_the_framing_line() {
        let sel = cap(vec![ranked("i0", 10)]);
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        assert!(
            text.starts_with(FRAMING_LINE),
            "the moment/prompt block must open with the framing line: {text}"
        );
    }

    // ---------------------------------------------------------- falsifier

    /// The defect this guards against: a second line per item on the surface
    /// that fires at every TOOL CALL - the tightest surface there is. Same
    /// argument as `session_start::render`'s own doc comment: the falsifier's
    /// value was banked at write time, and a pushed surface stays minimal.
    #[test]
    fn a_falsifier_never_rides_along_on_the_moment_block() {
        let mut item = ranked("i0", 10);
        item.item.falsifier = Some("this stops holding once the store is retired".to_string());
        let sel = cap(vec![item]);
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        assert!(!text.contains("falsified by"), "a pushed surface must not carry it: {text}");
        assert!(
            !text.contains("this stops holding once the store is retired"),
            "and not its text under any other wording either: {text}"
        );
    }

    /// One item is one line, falsifier or not - the field never changes the
    /// shape of this block.
    #[test]
    fn one_item_is_one_line_either_way() {
        let mut with = ranked("i0", 10);
        with.item.falsifier = Some("some observation".to_string());
        let without = ranked("i1", 10);
        assert_eq!(without.item.falsifier, None, "fixture sanity");

        let a = render_text(&cap(vec![with]), &ServeInput::default(), test_db()).unwrap();
        let b = render_text(&cap(vec![without]), &ServeInput::default(), test_db()).unwrap();
        assert_eq!(a.lines().count(), b.lines().count(), "same shape either way\nA:{a}\nB:{b}");
    }

    // -------------------------------------------------------------------- id

    /// THE DEFECT THIS PREVENTS: a served fact carried no id at all, so an
    /// assistant that noticed the fact was wrong had no way to correct it
    /// directly - it had to guess search terms to find the item again first,
    /// turning a one-call `revise` into three steps. Every shown item now
    /// carries its id.
    #[test]
    fn a_served_item_shows_its_id() {
        let sel = cap(vec![ranked("i0", 10)]);
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        assert!(text.contains("i0"), "the block must carry the item's id: {text}");
    }

    /// Showing the id somewhere is not enough - it has to come out whole and
    /// cleanly delimited, so an assistant can lift it out and pass it
    /// straight to `revise`'s own `id` argument with no editing. Uses an id
    /// shaped like a real one (a project prefix plus a colon, per
    /// `bin/restore_attribution.rs`'s own doc comment on the `<project>:<id>`
    /// shape) to prove the bracket, not the id's own content, is what an
    /// assistant would split on.
    #[test]
    fn the_shown_id_is_in_a_form_that_can_be_passed_straight_to_revise() {
        let sel = cap(vec![ranked("thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV", 10)]);
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        assert!(
            text.contains("[thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV]"),
            "the id must appear whole, inside brackets, exactly as `revise` expects it: {text}"
        );
    }

    /// THE CRITICAL DEFECT THIS PREVENTS: the id riding in on the item's own
    /// text budget, so adding it silently truncates more of the fact's body
    /// than before. Proven at the exact worst case the module doc comment
    /// already reasons about
    /// (`four_max_length_items_always_fit_the_1200_char_budget`) - four items
    /// at the write gate's own `MAX_TEXT_CHARS` ceiling - now each also
    /// carrying a realistically long id, distinct from the body's own filler
    /// character so a dropped body character could never hide inside it.
    #[test]
    fn adding_the_id_never_shrinks_how_much_of_the_body_is_delivered() {
        let long_id = format!("{}:{}", "p".repeat(20), "9".repeat(26)); // project + ulid shape
        let items: Vec<RankedItem> =
            (0..4).map(|i| ranked(&format!("{long_id}-{i}"), model::gate::MAX_TEXT_CHARS)).collect();
        let sel = cap(items);
        assert_eq!(sel.shown.len(), 4, "a long id must not change how many items the same budget selects");
        assert_eq!(sel.withheld, 0);
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        for shown in &sel.shown {
            assert!(
                text.contains(&shown.item.text),
                "every character of the {}-char body must still appear, id or no id",
                shown.item.text.chars().count()
            );
        }
    }

    // ------------------------------------------------- the withheld hint
    //
    // THE DEFECT THESE PREVENT: the hint used to read "run `serve why`" with
    // nothing after it, on every surface, regardless of what had actually
    // fired - not the flag `why`'s own parser (`TargetArgs` in
    // `bin/serve.rs`) requires, and not even the file or command that made
    // the block fire, so following it verbatim re-asked an EMPTY question.
    // Measured against a session's own report after trying to guess past it:
    // "the help command for what fires here had a different flag than the
    // hint said: --file, not a path." `serve/tests/
    // why_hint_matches_the_parser.rs` proves the flags named here are the
    // ones the real compiled parser accepts; these are the unit-level half -
    // the hint text carries the flag at all.

    fn withheld_selection() -> Selection {
        let items: Vec<RankedItem> = (0..6).map(|i| ranked(&format!("i{i}"), 10)).collect();
        cap(items)
    }

    #[test]
    fn a_file_backed_block_hints_the_file_flag_the_parser_accepts() {
        let sel = withheld_selection();
        let input = ServeInput { file: Some("src/main.rs".to_string()), ..Default::default() };
        let text = render_text(&sel, &input, test_db()).unwrap();
        assert!(
            text.contains("why --file \"src/main.rs\"` to see them"),
            "the hint must carry the exact --file invocation: {text}"
        );
    }

    #[test]
    fn a_command_backed_block_hints_the_command_flag_the_parser_accepts() {
        let sel = withheld_selection();
        let input = ServeInput { command: Some("git push --force origin main".to_string()), ..Default::default() };
        let text = render_text(&sel, &input, test_db()).unwrap();
        assert!(
            text.contains("why --command \"git push --force origin main\"` to see them"),
            "the hint must carry the exact --command invocation: {text}"
        );
    }

    /// A file wins when both are present (the shape a real Write/Edit tool
    /// call takes: its own tool name as a command, plus the file it
    /// touches - see `hook_once`'s PreToolUse arm) - the shorter, more
    /// literal thing a new user reaches for.
    #[test]
    fn a_file_wins_over_a_command_when_both_are_present() {
        let sel = withheld_selection();
        let input = ServeInput {
            command: Some("Write".to_string()),
            file: Some("src/main.rs".to_string()),
            ..Default::default()
        };
        let text = render_text(&sel, &input, test_db()).unwrap();
        assert!(text.contains("--file \"src/main.rs\""), "{text}");
        assert!(!text.contains("--command"), "a file hint must not also name the command: {text}");
    }

    /// Surface 3 (a raw prompt) never calls `add_command`/`add_file` -
    /// `prompt::resolve` derives moments and targets directly - so neither
    /// field is ever set there. The hint still names a real, accepted flag
    /// (`--moment`) instead of falling back to the empty "serve why" that
    /// caused this whole defect.
    #[test]
    fn a_prompt_only_block_hints_the_moment_flag_when_neither_file_nor_command_apply() {
        let sel = withheld_selection();
        let input = ServeInput { moments: vec![intent::Action::Push], ..Default::default() };
        let text = render_text(&sel, &input, test_db()).unwrap();
        assert!(
            text.contains("why --moment push` to see them"),
            "the hint must fall back to a real, accepted flag: {text}"
        );
    }

    /// The last-resort case (should not occur on a real shown item, which
    /// needs at least one binding to have matched) must still be the bare
    /// command this replaces - never a panic, never a malformed flag.
    #[test]
    fn the_bare_fallback_never_panics_with_nothing_to_name() {
        let sel = withheld_selection();
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        assert!(text.contains("why` to see them"), "{text}");
    }

    // ------------------------------------------ the self-invocation path
    //
    // THE DEFECT THESE PREVENT, reported 2026-09-12: even a hint that named
    // the right flag (the block above) still opened with the bare word
    // `serve`. Nothing this project ships is ever installed onto PATH, on
    // this machine or a fresh one, so following it verbatim answered
    // "command not found" - an agent that took that literally reported the
    // command itself as unavailable, a worse failure than the empty-question
    // defect the tests above guard: that one could be answered wrong, this
    // one could not be run at all.

    /// THE CORE PROOF, at the level `why_invocation` itself is wired: the
    /// real `std::env::current_exe` (this very test binary, since that is
    /// the process actually running) and the real `db_path` argument both
    /// reach the final rendered block, not just `render_self_invocation` in
    /// isolation below.
    #[test]
    fn the_rendered_hint_names_the_real_running_binary_and_the_real_db_path() {
        let sel = withheld_selection();
        let db = test_db();
        let text = render_text(&sel, &ServeInput::default(), db).unwrap();
        let exe = std::env::current_exe().expect("this process must have a real exe path while running");
        assert!(
            text.contains(&exe.display().to_string()),
            "the hint must name the real running binary's own absolute path, never the bare word 'serve': {text}"
        );
        assert!(
            text.contains(&format!("--db \"{}\"", db.display())),
            "the hint must carry the exact --db this process itself was opened with: {text}"
        );
    }

    /// Proves the real wiring on THIS host: `why_invocation` passes the
    /// real `cfg!(windows)` (not a caller-chosen value) into
    /// `render_self_invocation`, so on a Windows build the rendered hint
    /// actually takes the `&`-prefixed form - not merely that the pure
    /// formatter can produce it when asked (see the two tests below).
    #[cfg(windows)]
    #[test]
    fn on_windows_the_real_hint_leads_with_the_ampersand_call_operator() {
        let sel = withheld_selection();
        let text = render_text(&sel, &ServeInput::default(), test_db()).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert!(
            text.contains(&format!("& \"{}\"", exe.display())),
            "a quoted path is a call EXPRESSION to PowerShell, not a command, without the leading '&': {text}"
        );
    }

    /// `render_self_invocation`'s own two platform forms, proven directly
    /// with fixed inputs rather than through `cfg!(windows)` (which bakes
    /// into whichever platform compiled this test binary and so could never
    /// let a Windows-built suite prove the non-Windows form was reachable at
    /// all) - see the function's own doc comment for why `windows` is a
    /// parameter rather than read from `cfg!` internally.
    #[test]
    fn the_windows_form_leads_with_the_call_operator_and_quotes_both_paths() {
        let text = render_self_invocation(
            Some(Path::new("C:\\thor2\\bin\\serve.exe")),
            Path::new("C:\\thor2\\store.db"),
            " --file \"src/main.rs\"",
            true,
        );
        assert_eq!(
            text,
            "& \"C:\\thor2\\bin\\serve.exe\" --db \"C:\\thor2\\store.db\" why --file \"src/main.rs\"",
            "PowerShell needs the '&' call operator before a quoted path"
        );
    }

    /// The non-Windows counterpart: no call operator, since a POSIX shell
    /// already runs a quoted path as a command.
    #[test]
    fn the_non_windows_form_has_no_call_operator() {
        let text = render_self_invocation(
            Some(Path::new("/opt/thor2/bin/serve")),
            Path::new("/opt/thor2/store.db"),
            "",
            false,
        );
        assert_eq!(text, "\"/opt/thor2/bin/serve\" --db \"/opt/thor2/store.db\" why");
    }

    /// THE DEFECT THIS PREVENTS: `std::env::current_exe` failing (its own
    /// docs describe this as possible, e.g. the running binary was since
    /// deleted or replaced) crashing the hint, or the hint silently
    /// disappearing, instead of degrading to the one thing that is always
    /// true - the bare command name. Exercised directly via
    /// `render_self_invocation(None, ..)` because a real failure cannot be
    /// forced from inside a running test. Never worse than the defect this
    /// whole fix exists to close: exactly the text every hint carried before
    /// today, on either platform - the fallback does not vary by `windows`.
    #[test]
    fn a_current_exe_that_cannot_be_resolved_falls_back_to_the_bare_name() {
        let with_flag = render_self_invocation(None, test_db(), " --file \"src/main.rs\"", true);
        assert_eq!(with_flag, "serve why --file \"src/main.rs\"");
        let bare = render_self_invocation(None, test_db(), "", false);
        assert_eq!(bare, "serve why");
    }
}
