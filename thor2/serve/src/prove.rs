//! Does this rule's check actually refuse anything? Answered by trying it,
//! not by reading it.
//!
//! THE DEFECT THIS CLOSES, named by the owner on 2026-08-09: "the gate must
//! not just store something as important - it has to be PROVEN working."
//!
//! Until now a rule counted as armed the moment somebody wrote a check on it.
//! Nothing ever ran that check against a write it claimed to stop, so a check
//! could be shaped wrong - a forbidden literal on a binding the guard's
//! command arm never reads, a literal with a typo in it - and the rule would
//! sit in the store looking like protection, be counted as protection by
//! `doctor`, and refuse nothing for the rest of its life. That is worse than
//! no check at all: it is cover you believe in.
//!
//! So a claim now has to survive being tried: the forbidden fragment goes
//! literally into a command or into file content and is handed to the same
//! guard arms the hook uses.
//!
//! WHAT THAT DOES AND DOES NOT SHOW, because the first version of this file
//! overclaimed and a test caught it. The attempt is BUILT from the literal, so
//! the literal is always present in it by construction. That makes this a
//! proof of REACH - the check is wired so that a real command carrying this
//! fragment would be refused - and never a proof that the fragment is the
//! right one. A literal with a typo in it is wired perfectly and guards
//! nothing, and no machine here can tell: only the person who knows the real
//! command can. Reach is still worth proving, because the shape that fails it
//! is the one that looks strongest and can never fire.
//!
//! Scope, stated plainly rather than implied: only `Forbidden` is provable
//! here, because it is the only check that refuses an INTRODUCTION with
//! nothing on disk to read. The file-reading kinds (`Absent`, `AbsentAll`,
//! `Contains`, `PathExists`) need a real checkout to mean anything, so they
//! are reported as unproven rather than quietly passed off as proven.

use crate::absent_guard;
use crate::live::LiveItem;
use model::item::{Binding, Check, Item, TargetKind};

/// What trying the check actually showed.
pub enum Proof {
    /// The guard refused the attempt. Carries the attempt that was refused,
    /// so a caller can say what was proven rather than assert that something
    /// was.
    Fires(String),
    /// The guard let the attempt through. Carries what was tried, because the
    /// attempt is the evidence and a reader needs to see it to fix the rule.
    Silent(String),
    /// Nothing to try: no check, or a kind that cannot be tried without a
    /// working copy. Never dressed up as either of the two above.
    NotProvable(&'static str),
}

/// Try the rule's own check against an attempt it claims to forbid.
pub fn prove(id: &str, item: &Item) -> Proof {
    let literals = match &item.check {
        Some(Check::Forbidden { literals }) => literals,
        Some(_) => {
            return Proof::NotProvable(
                "this check reads a file, so it can only be proven against a real working copy",
            )
        }
        None => return Proof::NotProvable("this rule carries no check"),
    };
    let Some(literal) = literals.first() else {
        return Proof::NotProvable("the forbidden set is empty");
    };
    let one = [LiveItem { id: id.to_string(), item: item.clone() }];

    // A command binding: the fragment inside the command it is anchored to.
    for binding in &item.bindings {
        if let Binding::Target { kind: TargetKind::Command, value } = binding {
            let attempt = format!("{value} {literal}");
            // The command arm gives up immediately without a root, even for a
            // Forbidden check that reads no file at all. So it gets one. The
            // current directory is never read on this path - if that ever
            // changes, this proof starts depending on where it was run from,
            // and `a_command_rule_whose_literal_really_appears_is_proven_by_being_refused`
            // is what will notice.
            let here = std::path::Path::new(".");
            if absent_guard::find_command_violation(&one, &attempt, Some(here)).is_some() {
                return Proof::Fires(attempt);
            }
            return Proof::Silent(attempt);
        }
    }
    // Always: the fragment written into a file.
    if item.bindings.iter().any(|b| matches!(b, Binding::Always)) {
        let attempt = literal.to_string();
        if absent_guard::find_forbidden_violation(&one, &attempt).is_some() {
            return Proof::Fires(format!("writing {attempt:?} into a file"));
        }
        return Proof::Silent(format!("writing {attempt:?} into a file"));
    }
    // Neither reach exists. This is the shape that looks strongest and can
    // never fire, so it gets its own answer rather than a vague failure.
    Proof::NotProvable(
        "a forbidden check reaches only through an always binding or a command target, and this rule has neither",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Kind, Severity};

    fn rule(text: &str, bindings: Vec<Binding>, check: Option<Check>) -> Item {
        Item {
            id: "r1".to_string(),
            kind: Kind::Rule,
            text: text.to_string(),
            bindings,
            severity: Some(Severity::Irreversible),
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it turns out to be safe".to_string()),
            check,
        }
    }

    fn cmd(value: &str) -> Binding {
        Binding::Target { kind: TargetKind::Command, value: value.to_string() }
    }

    #[test]
    fn a_command_rule_whose_literal_really_appears_is_proven_by_being_refused() {
        let item = rule(
            "never force-push",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        match prove("r1", &item) {
            Proof::Fires(attempt) => assert!(attempt.contains("--force"), "{attempt}"),
            _ => panic!("a literal that appears verbatim in the command must be refused"),
        }
    }

    /// THE LIMIT, kept as a test so nobody re-reads this module and believes
    /// it does more than it does. A literal with a typo in it is wired exactly
    /// as well as a correct one, so it passes here. It has to: the attempt is
    /// built from the literal. Only somebody who knows the real command can
    /// see that "--froce" guards nothing, and pretending otherwise would be
    /// the cover-you-believe-in defect this module exists to prevent, moved
    /// one level up.
    #[test]
    fn a_typo_in_the_literal_still_passes_and_that_is_the_honest_limit() {
        let item = rule(
            "never force-push",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--froce".to_string()] }),
        );
        assert!(
            matches!(prove("r1", &item), Proof::Fires(_)),
            "reach is all this proves; spelling is not something it can know"
        );
    }

    /// The shape that looks strongest and can never fire: a forbidden check on
    /// a binding the guard has no way to reach through.
    #[test]
    fn a_forbidden_check_on_a_moment_can_never_fire_and_says_so() {
        let item = rule(
            "never force-push",
            vec![Binding::Moment(intent::Action::Push)],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        assert!(matches!(prove("r1", &item), Proof::NotProvable(_)));
    }

    #[test]
    fn a_rule_with_no_check_is_reported_as_nothing_to_try_never_as_working() {
        let item = rule("ask first", vec![Binding::Always], None);
        assert!(matches!(prove("r1", &item), Proof::NotProvable(_)));
    }

    /// A file-reading check is not silently blessed. It needs a working copy,
    /// and saying so is the honest answer.
    #[test]
    fn a_file_reading_check_is_reported_unproven_rather_than_assumed_good() {
        let item = rule(
            "NOTES.md must keep its header",
            vec![Binding::Always],
            Some(Check::Absent { path: "NOTES.md".to_string(), literal: "TODO".to_string() }),
        );
        assert!(matches!(prove("r1", &item), Proof::NotProvable(_)));
    }
}
