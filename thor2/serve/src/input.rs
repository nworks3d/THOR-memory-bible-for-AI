//! What was about to happen, in the shape `rank::select` needs: zero or more
//! detected moments, zero or more targets, and the raw context text the
//! closeness key is derived from. Every constructor here is the ONLY place a
//! caller (hook, check, why) turns a real command/path/target into this
//! shape, so all three channels build it identically.

use intent::Action;
use model::item::TargetKind;

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
    }

    #[test]
    fn a_file_derives_its_own_moments_and_becomes_a_path_target() {
        let mut input = ServeInput::default();
        input.add_file("app/.env");
        assert!(input.moments.contains(&Action::Credentials));
        assert!(input.targets.contains(&(TargetKind::Path, "app/.env".to_string())));
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
}
