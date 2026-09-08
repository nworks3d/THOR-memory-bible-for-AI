//! What was about to happen, in the shape `rank::select` needs: zero or more
//! detected moments, zero or more targets, and the raw context text the
//! closeness key is derived from. Every constructor here is the ONLY place a
//! caller (hook, check, why) turns a real command/path/target into this
//! shape, so all three channels build it identically.

use intent::Action;
use model::item::TargetKind;
use std::path::PathBuf;

/// One whitespace-separated argument, stripped of the shell punctuation that
/// wraps or follows it, and of a leading `--flag=`.
///
/// Deliberately not a shell parser. It never resolves a variable, a glob or a
/// pipeline; it only recovers the literal words a person typed, which is all
/// an anchor match needs.
fn shell_tokens(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|raw| {
            let t = raw.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | ';' | ',' | '`' | '|' | '<' | '>'));
            match t.split_once('=') {
                // `--db=path` and `FOO=path` both hand over the right half.
                Some((_, rhs)) if !rhs.is_empty() && t.starts_with(|c: char| c == '-' || c.is_ascii_uppercase()) => rhs,
                _ => t,
            }
            .trim_end_matches(['.', ':', ';'])
            .to_string()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// The last dot-suffix of a token, when it looks like a file extension:
/// 1 to 8 characters, letters and digits only.
fn extension_of(token: &str) -> Option<&str> {
    let (_, ext) = token.rsplit_once('.')?;
    let ok = !ext.is_empty()
        && ext.len() <= 8
        && ext.chars().all(|c| c.is_ascii_alphanumeric());
    ok.then_some(ext)
}

/// Every file the command NAMES, as an anchor value.
///
/// THE DEFECT THIS CLOSES, measured on 2026-08-07 in two places. A fact
/// anchored at a file fired when that file was opened with a file tool, and
/// stayed silent when a shell command read the very same file. Reproduced
/// here: three facts fired for `thor2/model/src/gate.rs` as a file and zero
/// for `grep -n Refusal thor2/model/src/gate.rs`. Shell is how deploy and
/// infrastructure work happens, so that is exactly where those facts were
/// needed and exactly where they said nothing. A warning about a grep trap in
/// a log file stayed quiet during that grep, and the mistake it named was
/// made.
///
/// Not a widened trigger: the anchor is unchanged, and it still matches only
/// the file it always named. What changes is that the file is now recognised
/// through a second, equally real route.
pub fn paths_in_command(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in shell_tokens(command) {
        if token.contains("://") || token.starts_with('-') || token.contains('*') || token.contains('?') {
            continue;
        }
        let normalised = token.replace('\\', "/");
        let has_separator = normalised.contains('/');
        if !has_separator && extension_of(&normalised).is_none() {
            continue;
        }
        if !out.contains(&normalised) {
            out.push(normalised);
        }
    }
    out
}

/// Every host the command NAMES, as an anchor value: the authority half of a
/// URL, or a bare dotted name.
///
/// A filename is the hard case, because `gate.rs` has exactly the shape of a
/// hostname. A bare token is therefore only read as a host when its last
/// label is NOT a known source extension (`model::gate::FILE_EXTENSIONS`, the
/// same list the write gate uses to tell `guard.rs:610` from `host:8080`).
/// That trades a miss for a false fire on purpose: a real TLD that is also a
/// source extension (`.md`, `.cc`) is skipped, which costs a hint, while the
/// opposite error would put a fact about a server in front of somebody
/// editing a file.
pub fn hosts_in_command(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in shell_tokens(command) {
        let candidate = match token.split_once("://") {
            // Strip scheme, then userinfo, then path, then port.
            Some((_, rest)) => {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                let authority = authority.rsplit('@').next().unwrap_or(authority);
                authority.split(':').next().unwrap_or(authority).to_string()
            }
            None => {
                if token.contains('/') || token.starts_with('-') {
                    continue;
                }
                // `ssh admin@host` names a host as plainly as a URL does.
                let token = token.rsplit('@').next().unwrap_or(&token).to_string();
                match extension_of(&token) {
                    // A dotted name whose tail is a source extension is a
                    // file, not a host.
                    Some(ext) if model::gate::FILE_EXTENSIONS.contains(&ext.to_lowercase().as_str()) => continue,
                    Some(ext) if ext.chars().all(|c| c.is_ascii_alphabetic()) && ext.len() >= 2 => {
                        token.split(':').next().unwrap_or(&token).to_string()
                    }
                    _ => continue,
                }
            }
        };
        if candidate.is_empty() || !candidate.contains('.') {
            continue;
        }
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

// ------------------------------------------------------- the shell write hole
//
// MEASURED: a rule anchored at a file with a `contains` or `absent` check
// refuses an Edit/Write of that file, but `rm file`, `truncate -s 0 file`,
// `sed -i ... file`, `echo x > file`, `tee file`, `mv other file` and
// `cp other file` from Bash walk straight past it - a shell write is a
// write, and until now only Edit/Write ever counted as one.
// `shell_write_targets` below is what `serve::absent_guard`'s command guard
// (`serve/src/bin/serve.rs`'s `command_guard_block`) feeds through the SAME
// file-based checks a real Edit/Write already goes through, so the same
// rule refuses both.

/// One shell-lite token: an ordinary WORD, or one of the OPERATORS this
/// function cares about (`;`, `&&`, `||`, `|`, `>`, `>>`) - a distinction
/// `shell_tokens` above has no need of (it only ever recovers words, and
/// trims `<`/`>` away as edge punctuation) and `shell_write_targets` cannot
/// do without: a filename that merely SITS next to a redirect must never be
/// read as the redirect's own target, and only keeping the operator as its
/// own token tells the two apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ShTok {
    Word(String),
    Op(&'static str),
}

fn flush_word(word: &mut String, out: &mut Vec<ShTok>) {
    if !word.is_empty() {
        out.push(ShTok::Word(std::mem::take(word)));
    }
}

/// Split `command` into shell-lite tokens: a quoted region (single or double
/// quotes) becomes ONE word token with its quotes stripped, even across
/// internal spaces - the one thing `shell_tokens` above cannot do, because it
/// starts from `str::split_whitespace`, which has already torn a quoted path
/// with a space into pieces before any quote logic could run. `;`, `&&`,
/// `||`, `|`, `>>` and `>` are recognised as their OWN tokens even with no
/// surrounding whitespace ("f>out" -> "f", ">", "out"), which is what lets a
/// redirect be told apart from a filename that merely touches one. `<` is a
/// silent word boundary (this function never reads input redirection, only
/// output), and a lone `&` (background) is dropped the same way. Not a shell
/// parser, the same declared scope `shell_tokens` above carries: no variable
/// expansion, no globbing, no nested quoting, no backslash escapes inside a
/// quote - only as much structure as telling an operator from a word needs.
fn sh_tokenize(command: &str) -> Vec<ShTok> {
    let mut out = Vec::new();
    let mut chars = command.chars().peekable();
    let mut word = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                let quote = c;
                for qc in chars.by_ref() {
                    if qc == quote {
                        break;
                    }
                    word.push(qc);
                }
            }
            c if c.is_whitespace() => flush_word(&mut word, &mut out),
            '<' => flush_word(&mut word, &mut out),
            ';' => {
                flush_word(&mut word, &mut out);
                out.push(ShTok::Op(";"));
            }
            '|' => {
                flush_word(&mut word, &mut out);
                if chars.peek() == Some(&'|') {
                    chars.next();
                    out.push(ShTok::Op("||"));
                } else {
                    out.push(ShTok::Op("|"));
                }
            }
            '&' => {
                flush_word(&mut word, &mut out);
                if chars.peek() == Some(&'&') {
                    chars.next();
                    out.push(ShTok::Op("&&"));
                }
                // A lone '&' backgrounds the command and names no path
                // either way, so it just ends whatever came before it.
            }
            '>' => {
                flush_word(&mut word, &mut out);
                if chars.peek() == Some(&'>') {
                    chars.next();
                    out.push(ShTok::Op(">>"));
                } else {
                    out.push(ShTok::Op(">"));
                }
            }
            _ => word.push(c),
        }
    }
    flush_word(&mut word, &mut out);
    out
}

/// `toks`, cut at every top-level `;`, `&&`, `||` and `|` - a compound or
/// piped command is one write per stage, never one write for the whole line
/// (`cat a.txt | tee b.txt` writes `b.txt`, not `a.txt`).
fn split_segments(toks: &[ShTok]) -> Vec<Vec<ShTok>> {
    let mut segments = Vec::new();
    let mut current = Vec::new();
    for t in toks {
        match t {
            ShTok::Op(";") | ShTok::Op("&&") | ShTok::Op("||") | ShTok::Op("|") => {
                segments.push(std::mem::take(&mut current));
            }
            _ => current.push(t.clone()),
        }
    }
    segments.push(current);
    segments
}

/// The wrapper-shells whose `-c`/`-lc`/`-Command` argument is itself a real
/// command to analyse, never the wrapper's own literal words. Matched
/// case-insensitively against the SAME normalised leading word
/// `absent_guard::strip_invocation_wrapper` already computes (directory and
/// `.exe` suffix stripped), so `/bin/sh`, `bash.exe` and a bare `pwsh` are
/// all recognised the same way that function already recognises any other
/// command's own name.
const SHELL_WRAPPER_NAMES: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "powershell", "pwsh"];

/// If `command` invokes one of the known wrapper shells with a `-c`/`-lc`/
/// `-Command` flag, the quoted argument that follows - the REAL command a
/// caller meant to run, which is what `shell_write_targets` must actually
/// see. `None` for anything else, including a wrapper invoked WITHOUT that
/// flag (an interactive-shell shape this project never receives from a tool
/// call) or one where the flag carries no following word.
fn unwrap_shell_c(command: &str) -> Option<String> {
    let normalised = crate::absent_guard::strip_invocation_wrapper(command);
    let head = normalised.split_whitespace().next()?;
    if !SHELL_WRAPPER_NAMES.contains(&head.to_lowercase().as_str()) {
        return None;
    }
    let toks = sh_tokenize(command);
    let flag_idx = toks.iter().position(|t| match t {
        ShTok::Word(w) => w == "-c" || w == "-lc" || w.eq_ignore_ascii_case("-command"),
        ShTok::Op(_) => false,
    })?;
    match toks.get(flag_idx + 1) {
        Some(ShTok::Word(inner)) => Some(inner.clone()),
        _ => None,
    }
}

/// A candidate argument names a real path, never a flag or a glob - the two
/// filters every verb-specific reader below shares. GLOBS ARE IGNORED
/// deliberately: a glob names nothing until the shell expands it, and this
/// function never runs a shell, so there is no way to know which real files
/// a pattern like `*.log` would touch - guessing and refusing on that guess
/// would refuse honest work the glob may not even reach the guarded file at
/// all.
fn is_path_arg(w: &str) -> bool {
    !w.starts_with('-') && !w.contains('*') && !w.contains('?')
}

/// What actually happens to the file a shell write touches - never a bare
/// "written", because a rule anchored there needs to know WHICH proof still
/// applies: a `Contains` check has nothing left to hold once the file is
/// gone or emptied, while a location prohibition cares only that the path
/// was touched at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellWriteEffect {
    /// `rm`/`git rm`: the file stops existing.
    Removed,
    /// `truncate -s 0`, or a bare `> f` / `: > f` with nothing feeding it:
    /// the file still exists, but every byte it held is gone.
    Emptied,
    /// `sed -i`, `tee`, a `>`/`>>` redirection with a real producer in front
    /// of it, or a `mv`/`cp` destination: the file's content changes, but
    /// it is not necessarily reduced to nothing.
    Rewritten,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellWrite {
    pub path: String,
    pub effect: ShellWriteEffect,
}

const MAX_UNWRAP_DEPTH: usize = 8;

/// Every real WRITE a shell command performs directly, bypassing the
/// Edit/Write tool entirely, paired with what actually happens to the file -
/// see this module's own "the shell write hole" section above for the
/// measurement this closes, and `ShellWriteEffect`'s own doc comment for why
/// the effect is never collapsed to a bare "written".
///
/// FLAGS ARE SKIPPED (a token starting with `-` is never read as a path -
/// `rm -rf` names one flag and zero files on its own). GLOBS ARE IGNORED
/// (see `is_path_arg`'s own doc comment for why guessing would cost more
/// than it catches). A QUOTED PATH WITH A SPACE is read whole
/// (`sh_tokenize`).
///
/// `sh -c "..."`, `bash -lc "..."` and `powershell -Command "..."` are
/// unwrapped FIRST (`unwrap_shell_c`): the write this function must see is
/// whatever the wrapped string actually runs, never the wrapper's own
/// words - a Bash tool call routed through one of these would otherwise
/// name no file at all. Capped at `MAX_UNWRAP_DEPTH`, the same defensive
/// stance `serve/src/bin/serve.rs`'s own `MAX_TOOL_INPUT_WALK_DEPTH` takes
/// for a payload this project does not control the shape of: a missed case
/// past the cap is acceptable, an unbounded recursive walk is not.
pub fn shell_write_targets(command: &str) -> Vec<ShellWrite> {
    shell_write_targets_at_depth(command, 0)
}

fn shell_write_targets_at_depth(command: &str, depth: usize) -> Vec<ShellWrite> {
    if depth < MAX_UNWRAP_DEPTH {
        if let Some(inner) = unwrap_shell_c(command) {
            return shell_write_targets_at_depth(&inner, depth + 1);
        }
    }
    let toks = sh_tokenize(command);
    let mut out = Vec::new();
    for segment in split_segments(&toks) {
        collect_segment_writes(&segment, &mut out);
    }
    out
}

/// The size `truncate -s <n>` was given, and the file it names, if any -
/// `-s 0` and `-s0` both spell the same flag. `-s 0` (and only `-s 0`)
/// empties the file; any other size still rewrites it (it may grow, or
/// shrink to something other than nothing), so only the zero case is ever
/// reported as `Emptied`.
fn collect_truncate(rest: &[&str], out: &mut Vec<ShellWrite>) {
    let mut size: Option<&str> = None;
    let mut file: Option<&str> = None;
    let mut i = 0;
    while i < rest.len() {
        let w = rest[i];
        if w == "-s" {
            size = rest.get(i + 1).copied();
            i += 2;
            continue;
        }
        if let Some(v) = w.strip_prefix("-s") {
            if !v.is_empty() {
                size = Some(v);
            }
            i += 1;
            continue;
        }
        if is_path_arg(w) {
            file = Some(w);
        }
        i += 1;
    }
    if let Some(f) = file {
        let effect = if size == Some("0") { ShellWriteEffect::Emptied } else { ShellWriteEffect::Rewritten };
        out.push(ShellWrite { path: f.to_string(), effect });
    }
}

/// `sed -i`'s own target files: the first non-flag argument is the SCRIPT
/// ('s/a/b/'), never a file - a glob character there is the script's own
/// regex syntax, not a filesystem glob, so only the arguments AFTER it are
/// ever read as paths. With fewer than two non-flag arguments (a script with
/// no file named, reading stdin, or a shape this cannot tell apart from
/// that), nothing is reported rather than guessed at.
fn collect_sed_i(rest: &[&str], out: &mut Vec<ShellWrite>) {
    let non_flags: Vec<&str> = rest.iter().copied().filter(|w| !w.starts_with('-')).collect();
    if non_flags.len() < 2 {
        return;
    }
    for w in &non_flags[1..] {
        if is_path_arg(w) {
            out.push(ShellWrite { path: (*w).to_string(), effect: ShellWriteEffect::Rewritten });
        }
    }
}

/// The `>`/`>>` redirection in `segment`, if any: the LAST one (a
/// double-redirect is vanishingly unlikely, and the last is also the one a
/// real shell would actually leave the stream pointed at). Everything before
/// it is the "producer"; a bare `:` there is the shell no-op idiom for
/// "truncate this file, run nothing", so it counts as no producer at all -
/// `: > f` and a bare `> f` both EMPTY the file, while any real command in
/// front of the `>` (`echo x`, `printf ...`) REWRITES it, and `>>` always
/// rewrites regardless, since append can never simply empty a file.
fn collect_redirect(segment: &[ShTok], out: &mut Vec<ShellWrite>) {
    let mut redirect: Option<(bool, usize)> = None;
    for (i, t) in segment.iter().enumerate() {
        match t {
            ShTok::Op(">") => redirect = Some((false, i)),
            ShTok::Op(">>") => redirect = Some((true, i)),
            _ => {}
        }
    }
    let Some((is_append, idx)) = redirect else { return };
    let target = segment[idx + 1..].iter().find_map(|t| match t {
        ShTok::Word(w) => Some(w.as_str()),
        ShTok::Op(_) => None,
    });
    let Some(target) = target else { return };
    if !is_path_arg(target) {
        return;
    }
    let has_producer = segment[..idx].iter().any(|t| matches!(t, ShTok::Word(w) if w != ":"));
    let effect = if is_append || has_producer { ShellWriteEffect::Rewritten } else { ShellWriteEffect::Emptied };
    out.push(ShellWrite { path: target.to_string(), effect });
}

/// One segment's own writes: the verb-shaped ones (`rm`, `git rm`,
/// `truncate -s 0`, `sed -i`, `tee`, `mv`/`cp`'s own destination) PLUS
/// whatever `>`/`>>` redirection the segment carries - checked independently
/// of each other (never `else`), so a segment like `tee f > log` (unusual,
/// but not impossible) reports BOTH targets rather than one silently
/// winning.
fn collect_segment_writes(segment: &[ShTok], out: &mut Vec<ShellWrite>) {
    let words: Vec<&str> = segment
        .iter()
        .filter_map(|t| match t {
            ShTok::Word(w) => Some(w.as_str()),
            ShTok::Op(_) => None,
        })
        .collect();

    match words.as_slice() {
        ["rm", rest @ ..] => {
            for w in rest {
                if is_path_arg(w) {
                    out.push(ShellWrite { path: (*w).to_string(), effect: ShellWriteEffect::Removed });
                }
            }
        }
        ["git", "rm", rest @ ..] => {
            for w in rest {
                if is_path_arg(w) {
                    out.push(ShellWrite { path: (*w).to_string(), effect: ShellWriteEffect::Removed });
                }
            }
        }
        ["truncate", rest @ ..] => collect_truncate(rest, out),
        ["sed", rest @ ..] if rest.iter().any(|w| w.starts_with("-i") || w.starts_with("--in-place")) => {
            collect_sed_i(rest, out)
        }
        ["tee", rest @ ..] => {
            for w in rest {
                if is_path_arg(w) {
                    out.push(ShellWrite { path: (*w).to_string(), effect: ShellWriteEffect::Rewritten });
                }
            }
        }
        ["mv", rest @ ..] | ["cp", rest @ ..] => {
            if let Some(dest) = rest.iter().rev().find(|w| is_path_arg(w)) {
                out.push(ShellWrite { path: (*dest).to_string(), effect: ShellWriteEffect::Rewritten });
            }
        }
        _ => {}
    }

    collect_redirect(segment, out);
}

#[derive(Debug, Clone, Default)]
pub struct ServeInput {
    pub moments: Vec<Action>,
    pub targets: Vec<(TargetKind, String)>,
    /// Raw text of what actually happened (command, file path, or both) -
    /// never anything an item declared about itself. Source of the closeness
    /// key in `rank::closeness`.
    pub context: String,
    /// Which project this session is standing in, resolved once by
    /// `project::resolve_project` and carried here so `rank::select` can apply
    /// the same scoping session start already applies. `None` means the
    /// project could not be resolved, and then only global items are served -
    /// see `project::applies_to` for why that is the safe direction.
    pub project: Option<String>,
    /// The verbatim string last handed to `add_command`, kept ONLY so
    /// `render::render_text` can echo back a `serve why --command "..."`
    /// that re-asks this exact question - `targets`/`context` already hold
    /// this same text, derived and mixed with other signals, which is right
    /// for ranking but wrong for a hint that must be copy-pasteable as one
    /// flag's value. See `render::why_invocation`'s own doc comment for the
    /// defect this closes.
    pub command: Option<String>,
    /// The verbatim string last handed to `add_file`, for the same reason
    /// `command` above exists.
    pub file: Option<String>,
    /// The project ROOT this session is standing in - the same filesystem
    /// root `serve/src/bin/serve.rs` already resolves once per hook call and
    /// threads into `absent_guard`'s own `root: Option<&Path>` parameters,
    /// carried here too so `rank::select`'s own anchor matching
    /// (`absent_guard::scoped_target_matches`) can tell a touched file
    /// INSIDE this project from one outside it. `None` exactly when no root
    /// was resolved (or none was ever supplied, e.g. a hand-built input) -
    /// `scoped_target_matches` then falls back to the old, root-blind
    /// comparison rather than guessing. See that function's own doc comment
    /// for the defect this exists to close.
    pub root: Option<PathBuf>,
}

impl ServeInput {
    pub fn is_empty(&self) -> bool {
        self.moments.is_empty() && self.targets.is_empty()
    }

    fn push_context(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.context.is_empty() {
            self.context.push(' ');
        }
        self.context.push_str(text);
    }

    /// A command about to run: derives moments via `intent::from_command`,
    /// treats the command text itself as a possible `Target::Command` doel,
    /// and - see `paths_in_command`/`hosts_in_command` - every file and host
    /// the command NAMES as a doel of its own.
    pub fn add_command(&mut self, command: &str) {
        if command.is_empty() {
            return;
        }
        self.command = Some(command.to_string());
        self.moments.extend(intent::from_command(command).into_iter().map(|s| s.action));
        self.targets.push((TargetKind::Command, command.to_string()));
        for path in paths_in_command(command) {
            self.targets.push((TargetKind::Path, path));
        }
        for host in hosts_in_command(command) {
            self.targets.push((TargetKind::Host, host));
        }
        self.push_context(command);
    }

    /// A file about to be read or written: derives moments via
    /// `intent::from_path` and treats the path itself as a `Target::Path` doel.
    pub fn add_file(&mut self, path: &str) {
        if path.is_empty() {
            return;
        }
        self.file = Some(path.to_string());
        self.moments.extend(intent::from_path(path).into_iter().map(|s| s.action));
        self.targets.push((TargetKind::Path, path.to_string()));
        self.push_context(path);
    }

    /// An explicit target of any kind (symbol/route/host/project/...), for a
    /// human-driven `check`/`why` call that is not shaped like a real
    /// Claude Code hook payload.
    pub fn add_target(&mut self, kind: TargetKind, value: &str) {
        if value.is_empty() {
            return;
        }
        self.targets.push((kind, value.to_string()));
        self.push_context(value);
    }

    /// An explicit moment, named directly rather than derived from a command
    /// or path - lets a human ask "what fires on `publish`" without having to
    /// spell a command that would trigger it.
    pub fn add_moment(&mut self, action: Action) {
        if !self.moments.contains(&action) {
            self.moments.push(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_derives_its_own_moments_and_becomes_a_command_target() {
        let mut input = ServeInput::default();
        input.add_command("git push --force origin main");
        assert!(input.moments.contains(&Action::Push));
        assert!(input.targets.contains(&(TargetKind::Command, "git push --force origin main".to_string())));
        assert!(input.context.contains("push"));
        assert_eq!(
            input.command.as_deref(),
            Some("git push --force origin main"),
            "the verbatim command must be kept for render::why_invocation's own hint"
        );
    }

    #[test]
    fn a_file_derives_its_own_moments_and_becomes_a_path_target() {
        let mut input = ServeInput::default();
        input.add_file("app/.env");
        assert!(input.moments.contains(&Action::Credentials));
        assert!(input.targets.contains(&(TargetKind::Path, "app/.env".to_string())));
        assert_eq!(
            input.file.as_deref(),
            Some("app/.env"),
            "the verbatim path must be kept for render::why_invocation's own hint"
        );
    }

    #[test]
    fn an_empty_input_is_empty() {
        assert!(ServeInput::default().is_empty());
    }

    #[test]
    fn adding_the_same_moment_twice_is_not_duplicated() {
        let mut input = ServeInput::default();
        input.add_moment(Action::Publish);
        input.add_moment(Action::Publish);
        assert_eq!(input.moments.len(), 1);
    }

    // ------------------------------------------ files and hosts in a command

    /// The measured defect, in one line: the same file, once as a file and
    /// once inside a command, has to reach the same anchor.
    #[test]
    fn a_command_that_reads_a_file_names_that_file_as_a_target() {
        let paths = paths_in_command("grep -n Refusal thor2/model/src/gate.rs");
        assert_eq!(paths, vec!["thor2/model/src/gate.rs"]);

        let mut input = ServeInput::default();
        input.add_command("grep -n Refusal thor2/model/src/gate.rs");
        assert!(
            input.targets.contains(&(TargetKind::Path, "thor2/model/src/gate.rs".to_string())),
            "the file the command reads must be a Path doel: {:?}",
            input.targets
        );
        assert!(
            input.targets.iter().any(|(k, _)| *k == TargetKind::Command),
            "and the command itself must still be one"
        );
    }

    /// The exact case from the field report: a bare log filename, greped.
    #[test]
    fn a_bare_filename_with_an_extension_counts() {
        assert_eq!(paths_in_command("Select-String UP_DONE rebuild-prod.log"), vec!["rebuild-prod.log"]);
    }

    #[test]
    fn windows_and_unc_paths_are_normalised_not_dropped() {
        assert_eq!(paths_in_command("type C:\\Users\\dev\\thor2\\thor.db"), vec!["C:/Users/dev/thor2/thor.db"]);
        assert_eq!(paths_in_command("cat \\\\server\\share\\deploy.log"), vec!["//server/share/deploy.log"]);
    }

    #[test]
    fn a_flag_hands_over_its_value() {
        assert_eq!(paths_in_command("serve --db=/srv/thor/thor.db hook"), vec!["/srv/thor/thor.db"]);
    }

    #[test]
    fn quotes_and_trailing_punctuation_come_off() {
        assert_eq!(paths_in_command("cat 'docs/1.0/SETUP.md';"), vec!["docs/1.0/SETUP.md"]);
    }

    /// Not a widened trigger: a glob names no one file, and a flag is not a
    /// path. Either one turned into a doel would fire facts on commands that
    /// never touched the file they are about.
    #[test]
    fn globs_and_flags_are_not_paths() {
        assert!(paths_in_command("rm -rf target/*.rs").iter().all(|p| !p.contains('*')));
        assert_eq!(paths_in_command("cargo test --all-targets"), Vec::<String>::new());
    }

    #[test]
    fn a_url_yields_its_host() {
        assert_eq!(hosts_in_command("curl https://quote.example.com/orders?id=3"), vec!["quote.example.com"]);
        // `ssh admin@host` names a host too; the user half is not part of it.
        assert_eq!(hosts_in_command("ssh admin@files.example.com"), vec!["files.example.com"]);
    }

    #[test]
    fn a_bare_hostname_yields_a_host() {
        assert_eq!(hosts_in_command("nslookup shop.example.com"), vec!["shop.example.com"]);
    }

    /// The hard case, and the reason the source-extension list is consulted:
    /// `gate.rs` has exactly the shape of a hostname. Reading it as one would
    /// put facts about a server in front of somebody editing a file.
    #[test]
    fn a_filename_is_never_read_as_a_host() {
        for cmd in ["grep foo model/src/gate.rs", "cat notes.md", "sh deploy.sh", "node build.js"] {
            assert_eq!(hosts_in_command(cmd), Vec::<String>::new(), "{cmd} must name no host");
        }
    }

    #[test]
    fn a_command_naming_a_host_reaches_a_host_anchor() {
        let mut input = ServeInput::default();
        input.add_command("curl -I https://shop.example.com/");
        assert!(
            input.targets.contains(&(TargetKind::Host, "shop.example.com".to_string())),
            "a host anchor must be reachable from a command: {:?}",
            input.targets
        );
    }

    // ------------------------------------------------- shell_write_targets
    //
    // THE MEASURED HOLE: `rm file`, `truncate -s 0 file`, `sed -i ... file`,
    // `echo x > file`, `tee file`, `mv other file` and `cp other file` from
    // Bash walk straight past a rule anchored at that exact file, because
    // only Edit/Write ever counted as a write. Each case below is the one
    // real shape a Bash tool call takes for it.

    fn removed(path: &str) -> ShellWrite {
        ShellWrite { path: path.to_string(), effect: ShellWriteEffect::Removed }
    }
    fn emptied(path: &str) -> ShellWrite {
        ShellWrite { path: path.to_string(), effect: ShellWriteEffect::Emptied }
    }
    fn rewritten(path: &str) -> ShellWrite {
        ShellWrite { path: path.to_string(), effect: ShellWriteEffect::Rewritten }
    }

    #[test]
    fn rm_names_every_argument_as_removed() {
        assert_eq!(shell_write_targets("rm a b"), vec![removed("a"), removed("b")]);
    }

    #[test]
    fn rm_skips_its_own_flags() {
        assert_eq!(shell_write_targets("rm -rf dir"), vec![removed("dir")]);
    }

    #[test]
    fn truncate_s_zero_empties_the_file() {
        assert_eq!(shell_write_targets("truncate -s 0 f"), vec![emptied("f")]);
    }

    #[test]
    fn truncate_a_nonzero_size_rewrites_rather_than_empties() {
        assert_eq!(shell_write_targets("truncate -s 100 f"), vec![rewritten("f")]);
    }

    #[test]
    fn sed_in_place_rewrites_the_file_and_never_its_own_script() {
        assert_eq!(shell_write_targets("sed -i 's/a/b/' f"), vec![rewritten("f")]);
    }

    #[test]
    fn a_bare_redirect_with_a_producer_rewrites_the_file() {
        assert_eq!(shell_write_targets("echo x > f"), vec![rewritten("f")]);
    }

    #[test]
    fn an_append_redirect_always_rewrites_even_with_a_producer() {
        assert_eq!(shell_write_targets("cat a >> f"), vec![rewritten("f")]);
    }

    #[test]
    fn a_redirect_with_no_producer_empties_the_file() {
        assert_eq!(shell_write_targets("> f"), vec![emptied("f")]);
        assert_eq!(shell_write_targets(": > f"), vec![emptied("f")], "the shell no-op idiom names no producer either");
    }

    #[test]
    fn tee_rewrites_the_file_it_names() {
        assert_eq!(shell_write_targets("tee f"), vec![rewritten("f")]);
    }

    #[test]
    fn a_piped_tee_is_still_found_in_its_own_segment() {
        assert_eq!(shell_write_targets("cat notes.md | tee f"), vec![rewritten("f")]);
    }

    #[test]
    fn mv_names_only_the_destination_as_rewritten() {
        assert_eq!(shell_write_targets("mv a b"), vec![rewritten("b")]);
    }

    #[test]
    fn cp_names_only_the_destination_as_rewritten() {
        assert_eq!(shell_write_targets("cp a b"), vec![rewritten("b")]);
    }

    #[test]
    fn git_rm_is_removed_the_same_as_a_bare_rm() {
        assert_eq!(shell_write_targets("git rm f"), vec![removed("f")]);
    }

    #[test]
    fn a_quoted_path_with_a_space_is_read_whole() {
        assert_eq!(shell_write_targets("rm \"my file.txt\""), vec![removed("my file.txt")]);
    }

    #[test]
    fn a_glob_is_ignored_rather_than_guessed_at() {
        assert_eq!(
            shell_write_targets("rm *.log"),
            Vec::<ShellWrite>::new(),
            "a glob names nothing until the shell expands it"
        );
    }

    #[test]
    fn a_sh_c_wrapper_is_unwrapped_to_the_real_command() {
        assert_eq!(shell_write_targets("sh -c \"rm file.txt\""), shell_write_targets("rm file.txt"));
        assert_eq!(shell_write_targets("sh -c \"rm file.txt\""), vec![removed("file.txt")]);
    }

    #[test]
    fn a_bash_lc_wrapper_is_also_unwrapped() {
        assert_eq!(shell_write_targets("bash -lc \"rm file.txt\""), vec![removed("file.txt")]);
    }

    #[test]
    fn a_powershell_command_wrapper_is_unwrapped_even_if_the_inner_shape_is_unrecognised() {
        // Unwrapping succeeds; the inner text just is not one of the POSIX
        // verbs this function knows, which is an acceptable missed case, not
        // a wrong answer - see `shell_write_targets`'s own doc comment.
        assert_eq!(shell_write_targets("powershell -Command \"Remove-Item file.txt\""), Vec::<ShellWrite>::new());
    }

    #[test]
    fn a_plain_read_yields_nothing() {
        assert_eq!(shell_write_targets("cat f"), Vec::<ShellWrite>::new());
    }

    #[test]
    fn a_status_check_yields_nothing() {
        assert_eq!(shell_write_targets("git status"), Vec::<ShellWrite>::new());
    }
}
