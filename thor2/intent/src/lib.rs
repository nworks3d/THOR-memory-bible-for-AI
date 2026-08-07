//! What is about to happen, derived from a command, a file path, or a draft
//! answer. One house for handeling-detectie (action detection): every entry
//! in the closed vocabulary below is deliberate and carries its own tests.
//!
//! The core rule this crate exists to enforce: a rule never carries a
//! pattern. Detection lives HERE and only here. A rule elsewhere declares the
//! MEANING (`Action::Publish`), never a trigger string - so three spellings
//! of the same meaning collapse to one action, and a rule bound to that
//! action already covers every surface form of it, present and future.
//!
//! This is new design (see the workspace-level design note), not a port of
//! `slices/rust/src/intent.rs`: the vocabulary and the regex groups are the
//! same idea, but `Action` is a real closed Rust enum (parsed with a
//! `FromStr` that FAILS on an unknown name) rather than a bare string, and
//! detection is split into exactly the three surfaces a caller actually has:
//! a command, a file path, and a draft answer.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The closed vocabulary. Each variant names something a person can regret.
/// Closed on purpose (R1 of the workspace design note): an open vocabulary is
/// how the anchor problem comes back wearing a new hat, with every rule
/// inventing its own trigger word and nothing firing twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Make something visible outside this machine or org.
    Publish,
    /// Record changes in version control.
    Commit,
    /// Send commits to a remote.
    Push,
    /// Change what is running somewhere.
    Deploy,
    /// Destroy data or history.
    Delete,
    /// Deliver a message to a human.
    Send,
    /// Move money or commit to a cost.
    Spend,
    /// Handle a secret, key, token or password.
    Credentials,
    /// Add a dependency or change the toolchain.
    Install,
    /// Change settings, hooks or automation.
    Configure,
    /// Touch live or customer data.
    ProdData,
    /// Assert that work is finished or verified.
    ClaimDone,
    /// Reply to the user at all.
    Answer,
    /// Write a new fact into memory, or correct one already there.
    Remember,
}

/// Every known action, in declaration order.
pub const ALL_ACTIONS: &[Action] = &[
    Action::Publish,
    Action::Commit,
    Action::Push,
    Action::Deploy,
    Action::Delete,
    Action::Send,
    Action::Spend,
    Action::Credentials,
    Action::Install,
    Action::Configure,
    Action::ProdData,
    Action::ClaimDone,
    Action::Answer,
    Action::Remember,
];

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Publish => "publish",
            Action::Commit => "commit",
            Action::Push => "push",
            Action::Deploy => "deploy",
            Action::Delete => "delete",
            Action::Send => "send",
            Action::Spend => "spend",
            Action::Credentials => "credentials",
            Action::Install => "install",
            Action::Configure => "configure",
            Action::ProdData => "prod_data",
            Action::ClaimDone => "claim_done",
            Action::Answer => "answer",
            Action::Remember => "remember",
        }
    }

    /// Every known action name, alphabetised - the list a refusal message
    /// hands back so "what do I write instead" always has an answer.
    pub fn known_names_sorted() -> Vec<&'static str> {
        let mut v: Vec<&'static str> = ALL_ACTIONS.iter().map(|a| a.as_str()).collect();
        v.sort_unstable();
        v
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The action name was not one of the closed vocabulary's entries. Carries
/// the name that failed and, via `Display`, the full list of valid ones -
/// so a caller sees what to write instead, not just that it was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAction(pub String);

impl fmt::Display for UnknownAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown action '{}'. The vocabulary is closed on purpose; known actions: {}",
            self.0,
            Action::known_names_sorted().join(", ")
        )
    }
}

impl std::error::Error for UnknownAction {}

impl FromStr for Action {
    type Err = UnknownAction;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ALL_ACTIONS
            .iter()
            .copied()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| UnknownAction(s.to_string()))
    }
}

/// One detected action plus the evidence that produced it.
///
/// The evidence is not decoration. When a rule fires and the user asks why,
/// "because you typed --visibility public" is an answer; "because the model
/// thought so" is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub action: Action,
    pub evidence: String,
}

impl Signal {
    pub fn new(action: Action, evidence: impl Into<String>) -> Self {
        Self { action, evidence: evidence.into() }
    }
}

fn re_i(pattern: &str) -> regex::Regex {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|e| panic!("bad pattern {pattern:?}: {e}"))
}

/// Ordered, deliberately narrow patterns over a COMMAND string. Anything
/// requiring cleverness belongs in a test first.
fn command_rules() -> Vec<(regex::Regex, Action, &'static str)> {
    [
        // publish
        (r"\bgh\s+repo\s+(edit|create)\b.*--visibility\s+public", Action::Publish, "repo made public"),
        (r"\bgh\s+(release|gist)\s+create\b", Action::Publish, "release or gist created"),
        (r"\bnpm\s+publish\b|\bcargo\s+publish\b|\btwine\s+upload\b", Action::Publish, "package published"),
        // push (checked before the generic git rules so it wins)
        (r"\bgit\s+push\b", Action::Push, "git push"),
        (r"\bgit\s+push\b.*(-f|--force)", Action::Push, "force push"),
        // commit
        (r"\bgit\s+commit\b", Action::Commit, "git commit"),
        (r"\bgit\s+(tag|merge|rebase)\b", Action::Commit, "history rewritten or tagged"),
        // A version bump is a release action wearing a build command's
        // clothes: anchor on what gets typed BEFORE the mistake.
        (r"\bnpm\s+version\b|\bnpm\s+run\s+(bump|release|set-version)\b", Action::Commit, "version bump"),
        // deploy
        (r"\bdocker\s+compose\b.*\b(up|restart|down)\b", Action::Deploy, "compose up/restart"),
        (r"\bsystemctl\s+(restart|start|stop)\b", Action::Deploy, "service restarted"),
        (r"\bkubectl\s+apply\b|\bterraform\s+apply\b", Action::Deploy, "infrastructure applied"),
        (r"\bscp\b|\brobocopy\b|\brsync\b", Action::Deploy, "files copied to another host"),
        (r"\bdocker\s+cp\b", Action::Deploy, "copied into a running container"),
        // delete
        (r"\brm\s+(-\w*[rf]\w*\s+)+", Action::Delete, "recursive or forced delete"),
        (r"\bRemove-Item\b.*-Recurse", Action::Delete, "recursive delete"),
        (r"\bdrop\s+(table|database)\b", Action::Delete, "schema dropped"),
        (r"\bgit\s+(reset\s+--hard|clean\s+-\w*f)", Action::Delete, "working tree discarded"),
        (r"\bdocker\s+(volume\s+rm|system\s+prune)", Action::Delete, "volume or image data removed"),
        // install
        (r"\b(npm|pnpm|yarn)\s+(i|install|add)\s+\S", Action::Install, "dependency added"),
        (r"\bpip\s+install\b|\bcargo\s+add\b|\bapt(-get)?\s+install\b", Action::Install, "dependency added"),
        // credentials
        (r"\b(export|set)\s+\w*(TOKEN|SECRET|PASSWORD|API_KEY)\w*=", Action::Credentials, "secret in a command"),
        (r"\bssh-keygen\b|\bgpg\s+--(gen-key|import)", Action::Credentials, "key material"),
        // send: the command itself delivers a message (a webhook post, a mail
        // relay), as distinct from a path or a tool identity - a command
        // surface is all this crate is given here, so this is where a
        // send-shaped command is caught.
        (r"\bsendmail\b|\bnotify-send\b|\bmail\s+-s\b", Action::Send, "message delivered from a command"),
    ]
    .into_iter()
    .map(|(pat, action, label)| (re_i(pat), action, label))
    .collect()
}

/// Ordered, deliberately narrow patterns over a FILE PATH (already seen with
/// backslashes normalized to forward slashes by the caller of `from_path`,
/// but normalized again here defensively).
fn path_rules() -> Vec<(regex::Regex, Action, &'static str)> {
    [
        (r"(^|/)\.env(\.|$)", Action::Credentials, "env file"),
        (r"(^|/)(id_rsa|id_ed25519|authorized_keys)$", Action::Credentials, "ssh key material"),
        (r"(^|/)\.claude/settings(\.local)?\.json$", Action::Configure, "agent settings"),
        (r"(^|/)\.github/workflows/", Action::Configure, "CI workflow"),
        (r"(^|/)(docker-compose|compose)\.ya?ml$", Action::Deploy, "compose file"),
        (r"(^|/)Dockerfile$", Action::Deploy, "container image"),
        (r"(^|/)\.git(ignore|attributes)$", Action::Configure, "repo hygiene file"),
        (r"(^|/)package\.json$", Action::Install, "dependency manifest"),
        (r"(^|/)(Cargo|pyproject)\.toml$", Action::Install, "dependency manifest"),
    ]
    .into_iter()
    .map(|(pat, action, label)| (re_i(pat), action, label))
    .collect()
}

/// Phrases in the assistant's own draft that assert completion. Deliberately
/// narrow: a claim is a statement about the world, not enthusiasm.
fn claim_rules() -> Vec<(regex::Regex, &'static str)> {
    [
        (r"\b(all\s+)?tests?\s+(pass|passing|are\s+green)\b", "tests reported green"),
        (r"\b(alle\s+)?tests?\s+(slagen|groen|zijn\s+groen)\b", "tests reported green"),
        (r"\b(it|this|that)\s+(now\s+)?works\b", "asserted working"),
        (r"\b(het\s+)?werkt\s+(nu|weer)?\b", "asserted working"),
        (r"\b(verified|confirmed|geverifieerd|bevestigd)\b", "asserted verified"),
        (r"\b(done|finished|complete|klaar|afgewerkt)\b", "asserted finished"),
        (r"\b(deployed|shipped|live|uitgerold)\b", "asserted shipped"),
    ]
    .into_iter()
    .map(|(pat, label)| (re_i(pat), label))
    .collect()
}

fn prod_hint() -> regex::Regex {
    re_i(r"\b(prod|production|live)\b")
}
fn money_hint() -> regex::Regex {
    re_i(r"\b(price|pricing|invoice|vat|btw|payment|refund|prijs)\b")
}

fn scan(text: &str, rules: &[(regex::Regex, Action, &'static str)]) -> Vec<Signal> {
    let mut out = Vec::new();
    for (re, action, label) in rules {
        if re.is_match(text) {
            out.push(Signal::new(*action, *label));
        }
    }
    out
}

/// Actions implied by a COMMAND that has not run yet.
///
/// Returns every action that applies - a `git push` to a public remote is
/// both a push and a publish, and collapsing that to one would lose whichever
/// rule the caller happened not to guess.
pub fn from_command(command: &str) -> Vec<Signal> {
    if command.is_empty() {
        return Vec::new();
    }
    let mut signals = scan(command, &command_rules());
    if prod_hint().is_match(command) {
        signals.push(Signal::new(Action::ProdData, "command names prod"));
    }
    dedup(signals)
}

/// Actions implied by a FILE PATH about to be read or written. Windows
/// backslashes are not a different world: normalized to `/` before matching.
pub fn from_path(path: &str) -> Vec<Signal> {
    if path.is_empty() {
        return Vec::new();
    }
    let norm = path.replace('\\', "/");
    let mut signals = scan(&norm, &path_rules());
    if money_hint().is_match(&norm) {
        signals.push(Signal::new(Action::Spend, "path names money"));
    }
    dedup(signals)
}

/// Actions implied by what the assistant is about to SAY.
///
/// This is the surface a file or command gate cannot reach at all, and it is
/// where the heaviest rules in the measured store live: do not claim done
/// without the test gate, always open with a plain-language summary, never
/// pad with disclaimers. Every draft is at minimum `Answer` - replying is
/// itself the action a rule can govern (e.g. "always open with a summary").
pub fn from_draft(text: &str) -> Vec<Signal> {
    let mut signals = vec![Signal::new(Action::Answer, "a reply is being made")];
    for (re, label) in &claim_rules() {
        if re.is_match(text) {
            signals.push(Signal::new(Action::ClaimDone, *label));
            break;
        }
    }
    dedup(signals)
}

/// One signal per action, keeping the first evidence seen.
fn dedup(signals: Vec<Signal>) -> Vec<Signal> {
    let mut seen: std::collections::HashSet<Action> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for s in signals {
        if seen.insert(s.action) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn actions_of(signals: &[Signal]) -> HashSet<Action> {
        signals.iter().map(|s| s.action).collect()
    }

    // ------------------------------------------------------------ Action::FromStr

    #[test]
    fn a_known_action_name_parses() {
        assert_eq!(Action::from_str("publish").unwrap(), Action::Publish);
        assert_eq!(Action::from_str("prod_data").unwrap(), Action::ProdData);
    }

    #[test]
    fn an_unknown_action_name_fails_to_parse() {
        let err = Action::from_str("yolo").unwrap_err();
        assert!(err.to_string().contains("yolo"));
        assert!(err.to_string().contains("publish"), "lists a known name to use instead");
    }

    #[test]
    fn as_str_and_from_str_round_trip_for_every_action() {
        for action in ALL_ACTIONS {
            assert_eq!(Action::from_str(action.as_str()).unwrap(), *action);
        }
    }

    #[test]
    fn remember_parses_from_its_wire_spelling() {
        // The defect this prevents: the variant exists on the Rust enum but
        // its wire spelling was never added to ALL_ACTIONS, so the string a
        // caller actually writes ("remember") would silently fail to parse
        // even though the variant itself compiles fine.
        assert_eq!(Action::from_str("remember").unwrap(), Action::Remember);
    }

    #[test]
    fn remember_round_trips_through_display() {
        // The defect this prevents: Display drifting from as_str for this
        // one variant (a typo'd match arm, a forgotten one), which would show
        // a person a name that does not parse back to the same action.
        assert_eq!(Action::Remember.to_string(), "remember");
        assert_eq!(Action::from_str(&Action::Remember.to_string()).unwrap(), Action::Remember);
    }

    #[test]
    fn the_unknown_action_message_lists_remember() {
        // The defect this prevents: a variant added to the enum and to
        // as_str but never landing in ALL_ACTIONS, so it silently never
        // appears in the "known actions" list a caller is told to use
        // instead - the refusal would look complete while quietly lying.
        let err = Action::from_str("yolo").unwrap_err();
        assert!(err.to_string().contains("remember"), "{err}");
    }

    // ------------------------------------------------------------ from_command

    #[test]
    fn one_meaning_survives_three_spellings() {
        // THE point of the design: three ways of saying "publish" all land on
        // the one Action::Publish, so a rule bound to it covers all three.
        for command in [
            "gh repo edit nworks3d/foo --visibility public",
            "npm publish --access public",
            "gh release create v1.2.3",
        ] {
            assert!(actions_of(&from_command(command)).contains(&Action::Publish), "{command}");
        }
    }

    #[test]
    fn a_push_is_a_push_and_not_a_commit() {
        let actions = actions_of(&from_command("git push origin main"));
        assert!(actions.contains(&Action::Push));
        assert!(!actions.contains(&Action::Commit));
    }

    #[test]
    fn a_destructive_command_reads_as_delete() {
        for command in ["rm -rf ./build", "git reset --hard origin/main", "drop table orders"] {
            assert!(actions_of(&from_command(command)).contains(&Action::Delete), "{command}");
        }
    }

    #[test]
    fn an_ordinary_command_triggers_nothing() {
        // A gate that fires on everything is wallpaper.
        for command in ["ls -la", "git status", "npm test", "cat README.md"] {
            assert!(from_command(command).is_empty(), "{command}");
        }
    }

    #[test]
    fn an_empty_command_triggers_nothing() {
        assert!(from_command("").is_empty());
    }

    #[test]
    fn evidence_is_always_carried() {
        let signals = from_command("git push --force origin main");
        assert!(!signals.is_empty());
        assert!(signals.iter().all(|s| !s.evidence.is_empty()));
    }

    // ------------------------------------------------------------ from_path

    #[test]
    fn a_path_can_carry_its_own_action() {
        assert!(actions_of(&from_path("app/.env")).contains(&Action::Credentials));
    }

    #[test]
    fn windows_paths_are_not_a_different_world() {
        let actions = actions_of(&from_path(r"C:\repo\.github\workflows\ci.yml"));
        assert!(actions.contains(&Action::Configure));
    }

    #[test]
    fn an_empty_path_triggers_nothing() {
        assert!(from_path("").is_empty());
    }

    // ------------------------------------------------------------ from_draft

    #[test]
    fn a_draft_that_claims_success_is_detected() {
        let actions = actions_of(&from_draft("Done - all tests pass and it works now."));
        assert!(actions.contains(&Action::ClaimDone));
    }

    #[test]
    fn a_draft_in_dutch_is_detected_too() {
        let actions = actions_of(&from_draft("Klaar, alle tests slagen."));
        assert!(actions.contains(&Action::ClaimDone));
    }

    #[test]
    fn an_ordinary_reply_claims_nothing() {
        let actions = actions_of(&from_draft("Here are the three options you asked about."));
        let expected: HashSet<Action> = [Action::Answer].into_iter().collect();
        assert_eq!(actions, expected);
    }
}
