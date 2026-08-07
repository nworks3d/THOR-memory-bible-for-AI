//! Proves the two capabilities added to `respond.rs` - a positional escape
//! via `before`, and a `tier` that can resolve to WARN instead of BLOCK -
//! are genuinely ADDITIVE: a rulebook that carries neither field parses and
//! evaluates exactly as it did before either existed.
//!
//! WHY THIS USES A FIXTURE AND NOT THE OWNER'S OWN FILE. An earlier version
//! of this test read the live rulebook at `C:\Users\dev\thor2\` directly,
//! and asserted that none of its rules carried the new fields. That made the
//! suite depend on a configuration file the owner is free to edit, so the
//! first time he actually used the new capability the test went red - not
//! because anything broke, but because the test had frozen a moment in his
//! config and called it a property of the code. A test that fails when a
//! user changes his own settings is testing the wrong thing.
//!
//! What is still worth checking against the live file is the one thing that
//! survives him editing it: that it is readable and parses at all. That is
//! the last test below, and it deliberately asserts nothing about content.

use serve::respond;

/// A rulebook in the shape every deployment had before `before`/`tier`
/// existed: two rules, neither carrying either field. Verbatim in structure
/// to the real thing, small enough to read at a glance.
const RULEBOOK_WITHOUT_THE_NEW_FIELDS: &str = r#"[
  {
    "id": "ask-user-to-commit-push",
    "any_of": ["zal ik pushen", "zal ik committen"],
    "none_of": ["vals alarm", "any_of"],
    "reminder": "Do it rather than asking."
  },
  {
    "id": "no-plain-language-tldr",
    "any_of": ["md5", "gepusht"],
    "none_of": ["tldr", "any_of"],
    "min_chars": 600,
    "reminder": "Open with a plain summary."
  }
]"#;

/// THE DEFECT THIS PREVENTS: a change meant to be purely additive silently
/// changing how many rules a rulebook parses to.
#[test]
fn a_rulebook_without_the_new_fields_parses_to_the_same_count_through_both_paths() {
    let old = respond::parse_rules(RULEBOOK_WITHOUT_THE_NEW_FIELDS);
    let new = respond::parse_opt_in_rules(RULEBOOK_WITHOUT_THE_NEW_FIELDS);
    assert_eq!(old.len(), new.len(), "both parsers must see the same number of rules");
    assert_eq!(old.len(), 2, "the fixture must actually parse, or this test proves nothing");
}

/// A rule that asks for neither new field must get the defaults: no
/// positional constraint, and BLOCK. Anything else would change what an
/// existing deployment does the moment the code lands under it.
#[test]
fn a_rule_asking_for_neither_field_defaults_to_no_position_and_block() {
    let rules = respond::parse_opt_in_rules(RULEBOOK_WITHOUT_THE_NEW_FIELDS);
    assert!(!rules.is_empty());
    for rule in rules {
        assert!(rule.before.is_empty(), "rule '{}' must default to no positional constraint", rule.base.id);
        assert_eq!(rule.tier, respond::Tier::Block, "rule '{}' must default to Block", rule.base.id);
    }
}

/// THE DEFECT THIS PREVENTS: adding `before`/`tier` support changing what an
/// untouched rulebook actually blocks. For a battery built from the
/// fixture's own trigger vocabulary plus a compliant reply, an empty reply
/// and a below-the-floor reply, the old path and the new path must agree
/// exactly, and the new path must never produce a WARN, since nothing in
/// this fixture asks for one.
#[test]
fn an_untouched_rulebook_blocks_identically_through_the_new_path() {
    let text = RULEBOOK_WITHOUT_THE_NEW_FIELDS;
    let long = |s: &str| format!("{s} {}", "detail ".repeat(100));

    let messages = [
        String::new(),
        "gepusht".to_string(),
        "zal ik pushen naar main?".to_string(),
        long("the md5 came back clean."),
        long("TLDR: het werkt. the md5 came back clean."),
    ];

    let mut fired = 0usize;
    for msg in &messages {
        let old = respond::block_reason(Some(text), msg);
        let verdict = respond::guard_verdict(Some(text), msg);
        assert_eq!(old, verdict.block_reason, "block verdict diverged for: {msg:?}");
        assert!(verdict.warn_reason.is_none(), "nothing here asks for a warn: {msg:?}");
        if old.is_some() {
            fired += 1;
        }
    }
    assert!(fired >= 2, "the battery must exercise real fires, not just agree on silence; got {fired}");
}

/// The one thing worth asking of the owner's real file, and the only thing
/// that stays true however he edits it: it is readable and it parses. This
/// asserts NOTHING about which rules it holds, what tier they carry or what
/// they block - that is his configuration, not this suite's business.
/// Skipped rather than failed when the file is not there, so the suite still
/// runs on a machine that is not his.
#[test]
fn the_owners_own_rulebook_still_parses_if_it_is_there() {
    // The path comes from the environment, never from a constant: a real one
    // baked in here is the maintainer's own machine, and this repository is
    // public. Unset means skip, the same as absent.
    let Ok(live) = std::env::var("RESPONSE_GUARD_EVAL_RULEBOOK") else { return };
    let Ok(text) = std::fs::read_to_string(&live) else { return };
    let old = respond::parse_rules(&text);
    let new = respond::parse_opt_in_rules(&text);
    assert_eq!(old.len(), new.len(), "both parsers must agree on how many rules the live file holds");
    assert!(!old.is_empty(), "the live rulebook is present but parses to nothing - it is malformed");
}
