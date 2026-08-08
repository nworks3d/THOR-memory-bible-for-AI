//! The four item kinds, their typed bindings, and the item itself.
//!
//! Every field below is a real, typed value serialised as canonical JSON -
//! never a formatted string parsed back out of prose. See `FINDINGS-1.0.md`
//! F1 for the defect class this replaces: 1.0 carried type, anchors, tags,
//! project and expiry as a `[memory/gotcha | tags: ... | anchors: ...]`
//! footer glued to the end of the body, so a content-only revise had to
//! *carry it over* by parsing - and "metadata silently dropped on a revise"
//! was a real defect class, not a hypothetical one, because there was
//! nothing in the schema to protect it.

use intent::Action;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The five item kinds. Each does exactly one job (see the workspace design
/// note's table): a `Rule` never expires and is served at a gate; an
/// `Orientation` lives until revised and is also served at a gate; a
/// `Report` expires and is found only by searching, never served at a gate;
/// a `Lookup` lives until revised and is served only on an explicit request
/// for its `key`; a `Chunk` is a piece of source code or documentation
/// pulled from a repo - archive exactly like `Report`: lives until revised,
/// fully searchable, never served at a gate. See `Kind::can_fire` for the
/// one place that decides which of these five may ever reach an injection
/// surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Rule,
    Orientation,
    Report,
    Lookup,
    Chunk,
}

impl Kind {
    /// Whether an item of this kind may ever reach an injection surface - a
    /// gate that fires at a moment of action, unprompted. True only for
    /// `Rule` and `Orientation`; `Report`, `Lookup` and `Chunk` are archive:
    /// fully searchable, never served up unprompted.
    ///
    /// This is the ONE place in the workspace that decides this boolean.
    /// Every serve-path channel (`serve::rank::eligible`'s selection filter,
    /// `serve::audit::audit_rows`'s reporting filter) calls this method
    /// rather than re-deciding the same thing locally - `model/tests/
    /// single_can_fire_definition.rs` greps the whole workspace's source for
    /// a second inline decision of this exact boolean and fails if one
    /// appears anywhere outside this function's own body.
    pub fn can_fire(self) -> bool {
        matches!(self, Kind::Rule | Kind::Orientation)
    }
}

/// Ordered from heaviest to lightest, exactly as measured in the real data
/// (71 irreversible / 208 costly / 22 house_style, of 301). Derived `Ord`
/// follows declaration order, so `Severity::Irreversible < Severity::Costly
/// < Severity::HouseStyle` - a severity that prevents data loss outranks one
/// about tone, and when something has to be cut it is cut from the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Irreversible,
    Costly,
    HouseStyle,
}

/// What kind of thing a `Target` binding's value names.
///
/// `Route` and `Host` were added for the thor2 migration (see RESULTS.md P3):
/// the mechanical conversion of already-extracted items parked 108 of them
/// because their target was an HTTP route or a bare hostname, neither of
/// which had a shape here. A UNC network share (`\\server\share\path`) is
/// NOT a third new shape - it is a path, so `reuse/target_shapes.py`'s
/// classifier maps it onto `Path` directly rather than getting its own kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Path,
    Dir,
    Symbol,
    Command,
    Project,
    Route,
    Host,
}

/// A binding is a typed value, never a substring lifted out of the text at
/// serve time. `Moment` reuses the closed action vocabulary from crate
/// `intent` (so a rule declares the MEANING, never a trigger string);
/// `Target` names an exact path/dir/symbol/command/project; `Always` is the
/// pinned layer, spelled out explicitly rather than implied by an empty list
/// (an item with no bindings at all is simply unbound, which the write gate
/// refuses for a `Rule` - see `gate::declare`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    Moment(Action),
    Target { kind: TargetKind, value: String },
    Always,
}

/// A machine-runnable check an item may optionally carry, alongside its
/// prose `falsifier`. Unlike `falsifier` (free text, read and judged by a
/// person, never evaluated by anything), a `Check` is executed by
/// `crate::check::run`, which reports one of three outcomes - `Holds`,
/// `Fails`, `CannotRun` - never a boolean, because "this proves nothing
/// either way" is a real, distinct answer from both "true" and "false".
///
/// Five forms, no globs: four name one exact file, relative to a root the
/// caller supplies at run time (never an absolute path baked into the item
/// itself); the fifth, `Forbidden`, names no file at all - see its own doc
/// comment below. See `crate::check` for the runner and `gate::declare` for
/// the write-time rules on when an item may carry one at all.
///
/// `AbsentAll` was added after the first three (see this module's own test
/// section "check field - AbsentAll variant" for the backward-compatibility
/// proof this requires): a single rule in the owner's head - his typography
/// rule forbids the em dash, the en dash, the minus glyph, two kinds of
/// curly quote and the ellipsis character, six literals, one rule - used to
/// need one `Absent` item PER literal, which is the exact same-topic-cluster
/// shape the rest of this workspace spent effort learning to treat as one
/// lead rather than six near-duplicates. `AbsentAll` carries the whole set
/// on one item with one falsifier instead.
///
/// `Forbidden` was added after `AbsentAll` (see this module's own test
/// section "check field - Forbidden variant" for its own backward-
/// compatibility proof), for the SAME typography rule: that rule is not
/// actually about any one file. "Never write an em dash" binds the writer,
/// not a directory - anchoring it to a path (as `AbsentAll` requires) was a
/// currency-proof formality with nothing real behind it, and the side
/// effect of that formality was that the rule only ever fired for writing
/// under whichever one directory happened to hold the anchor, never
/// anywhere else it was meant to apply too. `Forbidden` names the literals
/// and nothing else: no path, because there is no file this fact depends
/// on, and so nothing about it can ever go stale. See `crate::check::run`'s
/// own doc comment for what that means for the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// The named path exists.
    PathExists { path: String },
    /// The named file still contains this exact literal string.
    Contains { path: String, literal: String },
    /// The named file does not contain this exact literal string.
    Absent { path: String, literal: String },
    /// The named file contains NONE of these exact literal strings - holds
    /// only when every one of them is absent, fails as soon as any single
    /// one of them is present. The set form of `Absent`, for a rule that
    /// forbids several literals together as one fact. Never empty - see
    /// `gate::check_check`'s own refusal for an empty (or empty-containing)
    /// list.
    AbsentAll { path: String, literals: Vec<String> },
    /// NONE of these exact literal strings may ever be written, full stop -
    /// no path, because unlike the four forms above, there is no file whose
    /// current content decides the answer: the rule names nothing outside
    /// itself, so there is nothing that can rot and nothing left to prove.
    /// The set-only, path-less sibling of `AbsentAll` - prefer this one when
    /// the rule forbids something self-contained (a banned character, a
    /// banned word, a banned phrase, wherever it might be written), and
    /// `AbsentAll` when the rule is really about one specific file's current
    /// content (a file that could move, or content that could change).
    /// Never empty, for the same reason `AbsentAll`'s list is never empty -
    /// see `gate::check_check`.
    Forbidden { literals: Vec<String> },
}

/// A typed memory item. Field order below IS the canonical JSON field order:
/// `serde_json::to_string` on this struct (never on a `serde_json::Value`
/// map) serialises fields in declaration order, so canonicalisation needs no
/// `preserve_order` feature and no manual key sorting - it falls out of the
/// struct definition by construction. See `store::canonical_body` and its
/// round-trip test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: Kind,
    pub text: String,
    pub bindings: Vec<Binding>,
    pub severity: Option<Severity>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    /// ISO-8601 date. Only a `Report` may carry one (see `gate::declare`,
    /// refusal ground 2): a `Rule` never expires.
    pub expires: Option<String>,
    /// Required for a `Lookup` (see `gate::declare`, refusal ground 6);
    /// meaningless for the other three kinds.
    pub key: Option<String>,
    /// Free text naming what observation would prove this fact wrong - never
    /// evaluated by anything mechanical, never a gate of its own. It exists
    /// because a detector that decides at READ time whether a fact has gone
    /// stale depends on a model choosing to look, and that was measured at 0
    /// of 20 independently. A field filled in at WRITE time instead simply
    /// travels with the fact and is shown wherever the fact is shown, so the
    /// reader - never a machine - judges whether it still holds.
    ///
    /// Required at write time for `Rule` and `Orientation` (see
    /// `gate::declare`): those two kinds never expire by design, which is
    /// exactly where staleness would otherwise hide with nothing to name it.
    /// Optional for `Report`, `Lookup` and `Chunk` - a `Report` already has an
    /// expiry date, and a `Lookup`/`Chunk` is an archive fact answered by an
    /// explicit request, not a standing claim.
    ///
    /// `#[serde(default)]` so a body written before this field existed (every
    /// item migrated from 1.0) still parses as `None`, rather than failing to
    /// deserialise at all - the requirement above binds only NEW writes
    /// through `gate::declare`, never old bodies already on disk.
    #[serde(default)]
    pub falsifier: Option<String>,
    /// A machine-runnable check (see `Check`, and `crate::check::run` for the
    /// runner), alongside the prose `falsifier` above. Optional even for a
    /// `Rule` or `Orientation` - unlike `falsifier`, which those two kinds
    /// require - because most facts still have no mechanical way to check
    /// themselves; a prose falsifier is required either way. Refused for a
    /// `Report`, `Lookup` or `Chunk` (see `gate::declare`), for the same
    /// reason a binding is: only a kind that can ever reach a gate is a
    /// candidate to be proven current by a check that passes right now.
    ///
    /// `#[serde(default)]` for the same reason `falsifier` carries it: a
    /// body written before this field existed has no `"check"` key at all,
    /// and must still parse as `None` rather than fail to deserialise.
    ///
    /// `skip_serializing_if = "Option::is_none"` goes one step further than
    /// `falsifier`'s own attribute, and is the reason the hash stays stable
    /// for every item already in a real store. With only `#[serde(default)]`
    /// (`falsifier`'s current attribute), an item with `check: None` would
    /// still serialise a `"check":null` key that never existed in a body
    /// written before this field did - changing the canonical body, and so
    /// the hash chained into the event log, for every already-migrated item.
    /// Skipping the key entirely when `None` makes an item with no check
    /// serialise BYTE-FOR-BYTE identically to how it serialised before this
    /// field existed. See `store::canonical_body` and this module's own
    /// `an_item_without_a_check_serialises_with_no_check_key_at_all` test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
}

/// A string did not name one of a closed enum's variants. Carries the value
/// that failed and the exact list of valid ones, so a caller (a CLI, a
/// config loader) can report what to write instead rather than just that it
/// was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownVariant {
    pub value: String,
    pub valid: &'static [&'static str],
}

impl fmt::Display for UnknownVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown value '{}'; valid: {}", self.value, self.valid.join(", "))
    }
}

impl std::error::Error for UnknownVariant {}

const KIND_NAMES: &[&str] = &["rule", "orientation", "report", "lookup", "chunk"];
const SEVERITY_NAMES: &[&str] = &["irreversible", "costly", "house_style"];
const TARGET_KIND_NAMES: &[&str] = &["path", "dir", "symbol", "command", "project", "route", "host"];

impl FromStr for Kind {
    type Err = UnknownVariant;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "rule" => Ok(Kind::Rule),
            "orientation" => Ok(Kind::Orientation),
            "report" => Ok(Kind::Report),
            "lookup" => Ok(Kind::Lookup),
            "chunk" => Ok(Kind::Chunk),
            _ => Err(UnknownVariant { value: s.to_string(), valid: KIND_NAMES }),
        }
    }
}

impl FromStr for Severity {
    type Err = UnknownVariant;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "irreversible" => Ok(Severity::Irreversible),
            "costly" => Ok(Severity::Costly),
            "house_style" => Ok(Severity::HouseStyle),
            _ => Err(UnknownVariant { value: s.to_string(), valid: SEVERITY_NAMES }),
        }
    }
}

impl FromStr for TargetKind {
    type Err = UnknownVariant;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "path" => Ok(TargetKind::Path),
            "dir" => Ok(TargetKind::Dir),
            "symbol" => Ok(TargetKind::Symbol),
            "command" => Ok(TargetKind::Command),
            "project" => Ok(TargetKind::Project),
            "route" => Ok(TargetKind::Route),
            "host" => Ok(TargetKind::Host),
            _ => Err(UnknownVariant { value: s.to_string(), valid: TARGET_KIND_NAMES }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rule() -> Item {
        Item {
            id: "test-rule-1".to_string(),
            kind: Kind::Rule,
            text: "never force-push to main".to_string(),
            bindings: vec![Binding::Moment(Action::Push), Binding::Always],
            severity: Some(Severity::Irreversible),
            project: Some("thor2".to_string()),
            tags: vec!["git".to_string(), "safety".to_string()],
            expires: None,
            key: None,
            falsifier: Some("a force-push to main lands with no incident and no revert needed".to_string()),
            check: None,
        }
    }

    fn sample_lookup() -> Item {
        Item {
            id: "test-lookup-1".to_string(),
            kind: Kind::Lookup,
            text: "the release checklist lives in RELEASE.md".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "RELEASE.md".to_string() }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: Some("release-checklist".to_string()),
            falsifier: None,
            check: None,
        }
    }

    fn sample_chunk() -> Item {
        Item {
            id: "test-chunk-1".to_string(),
            kind: Kind::Chunk,
            text: "fn add(a: i32, b: i32) -> i32 { a + b }".to_string(),
            bindings: vec![],
            severity: None,
            project: Some("example-repo".to_string()),
            tags: vec!["rust".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    #[test]
    fn canonical_json_round_trips_a_rule() {
        let item = sample_rule();
        let json = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn canonical_json_round_trips_a_lookup() {
        let item = sample_lookup();
        let json = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn canonical_json_round_trips_a_chunk() {
        let item = sample_chunk();
        let json = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn canonical_json_field_order_is_deterministic() {
        // The struct's declared field order IS the wire order: id, kind, text,
        // bindings, severity, project, tags, expires, key. Assert the keys
        // appear in exactly that order in the serialised string, so a future
        // field reorder in this file is caught here rather than silently
        // changing what gets hashed into the log.
        let json = serde_json::to_string(&sample_rule()).unwrap();
        let positions: Vec<usize> = [
            "\"id\"", "\"kind\"", "\"text\"", "\"bindings\"", "\"severity\"", "\"project\"",
            "\"tags\"", "\"expires\"", "\"key\"", "\"falsifier\"",
        ]
        .iter()
        .map(|key| json.find(key).unwrap_or_else(|| panic!("{key} missing from {json}")))
        .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "field order drifted from id/kind/text/bindings/severity/project/tags/expires/key/falsifier: {json}");
    }

    #[test]
    fn serializing_twice_gives_byte_identical_output() {
        let item = sample_rule();
        let a = serde_json::to_string(&item).unwrap();
        let b = serde_json::to_string(&item).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn kind_name_round_trips() {
        for (name, kind) in [
            ("rule", Kind::Rule),
            ("orientation", Kind::Orientation),
            ("report", Kind::Report),
            ("lookup", Kind::Lookup),
            ("chunk", Kind::Chunk),
        ] {
            assert_eq!(Kind::from_str(name).unwrap(), kind);
        }
        assert!(Kind::from_str("bogus").is_err());
    }

    // ------------------------------------------------------------ can_fire

    #[test]
    fn only_rule_and_orientation_can_fire() {
        assert!(Kind::Rule.can_fire());
        assert!(Kind::Orientation.can_fire());
        assert!(!Kind::Report.can_fire());
        assert!(!Kind::Lookup.can_fire());
        assert!(!Kind::Chunk.can_fire());
    }

    #[test]
    fn severity_orders_heaviest_first() {
        assert!(Severity::Irreversible < Severity::Costly);
        assert!(Severity::Costly < Severity::HouseStyle);
    }

    // ------------------------------------------------------- falsifier field

    #[test]
    fn a_body_written_before_the_falsifier_field_existed_still_parses() {
        // Every one of the 2955 already-migrated Rule/Orientation items in the
        // real store was serialised before this field existed, so its JSON
        // body has no "falsifier" key at all - not even null. This is the
        // exact shape of a pre-migration body: constructed here by hand
        // (never read from a real store) with the key omitted entirely.
        let old_body = r#"{"id":"pre-existing-1","kind":"rule","text":"never force-push to main","bindings":[],"severity":"irreversible","project":null,"tags":[],"expires":null,"key":null}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body with no falsifier key must still parse");
        assert_eq!(item.falsifier, None, "a body written before this field existed must default to None, not fail to parse");
    }

    #[test]
    fn a_falsifier_round_trips_through_canonical_json() {
        let mut item = sample_rule();
        item.falsifier = Some("the store starts reporting force-pushes to main with no revert".to_string());
        let json = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }

    // ----------------------------------------------------------- check field
    //
    // THE DEFECT THESE PREVENT: every item already in a real store was
    // written before this field existed, so its stored body has no "check"
    // key at all. The canonical body is exactly what gets hashed into the
    // event log's chain (see `store::canonical_body`), and a hash is a pure
    // function of its input bytes - so if adding this field changed so much
    // as one byte of how an already-migrated item (which never sets it)
    // serialises, every one of those items' hashes would change too. Each
    // test below is named after the exact failure mode it pins shut.

    #[test]
    fn a_body_written_before_the_check_field_existed_still_parses() {
        // The realistic shape of a body on disk today: falsifier already
        // exists (it predates this field), "check" does not - constructed
        // here by hand, never read from a real store, with the key omitted
        // entirely.
        let old_body = r#"{"id":"pre-existing-1","kind":"rule","text":"never force-push to main","bindings":[],"severity":"irreversible","project":null,"tags":[],"expires":null,"key":null,"falsifier":"a force-push to main lands with no incident"}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body with no check key must still parse");
        assert_eq!(item.check, None, "a body written before this field existed must default to None, not fail to parse");
    }

    #[test]
    fn an_item_without_a_check_serialises_with_no_check_key_at_all() {
        // Not `"check":null` - the key must be fully absent, exactly as it
        // was before this field existed. `canonical_json_field_order_is_
        // deterministic` (above) already pins the other nine fields' exact
        // order using this same `sample_rule()` fixture (which leaves
        // `check` at its default `None`); this test is the other half - the
        // one new field contributes nothing at all to the serialised bytes
        // when unset, which is what keeps the hash stable for every item
        // already in a real store.
        let item = sample_rule();
        let json = serde_json::to_string(&item).unwrap();
        assert!(
            !json.contains("check"),
            "an item with no check must not mention the key at all, or the hash changes for every already-migrated item: {json}"
        );
    }

    #[test]
    fn a_fully_populated_item_without_a_check_serialises_byte_for_byte_as_a_golden_literal() {
        // THE GAP THIS CLOSES: the two tests above prove the "check" key is
        // absent when unset, and that an old (pre-`check`, pre-`falsifier`)
        // body still parses - but neither one proves END TO END that a
        // COMPLETE item, every other field populated, still serialises to
        // exactly the bytes it always did. `canonical_json_field_order_is_
        // deterministic` (in the falsifier section above) only checks the
        // POSITION of each key via `.find()`, not the full body, so a value
        // inside one of the other nine fields could still drift - a
        // `Binding` shape change, a `Severity` spelling change, a stray
        // space from a serde attribute edit - without either existing test
        // noticing. This pins the WHOLE canonical body, byte for byte,
        // against a hand-written literal: the golden value. It is written
        // out in full here, not derived by calling `serde_json` a second
        // time inside the test, so the test actually fails the moment the
        // shape drifts in any way, rather than trivially agreeing with
        // itself.
        //
        // Kind::Lookup, not Kind::Rule, is the fixture here on purpose: it
        // is the one kind that can carry a real, non-empty `bindings` AND a
        // real `key` at the same time (`gate::declare` ground 6 requires
        // the key; nothing forbids the binding), which populates more of
        // the "every other field" surface than any other kind can. `expires`
        // is the one field that stays `None` regardless - by the schema's
        // own design (`gate::declare` ground 2) only a `Report` may ever
        // carry it, and a `Report` cannot carry a binding (ground 3) or a
        // key, so no single item can have both a populated `bindings`/`key`
        // and a populated `expires` - this is not an oversight, it is the
        // schema.
        let item = Item {
            id: "golden-lookup-1".to_string(),
            kind: Kind::Lookup,
            text: "the release checklist lives in RELEASE.md".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "RELEASE.md".to_string() }],
            severity: Some(Severity::HouseStyle),
            project: Some("thor2".to_string()),
            tags: vec!["release".to_string(), "process".to_string()],
            expires: None,
            key: Some("release-checklist".to_string()),
            falsifier: Some("the release checklist moves out of RELEASE.md".to_string()),
            check: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        let golden = r#"{"id":"golden-lookup-1","kind":"lookup","text":"the release checklist lives in RELEASE.md","bindings":[{"target":{"kind":"path","value":"RELEASE.md"}}],"severity":"house_style","project":"thor2","tags":["release","process"],"expires":null,"key":"release-checklist","falsifier":"the release checklist moves out of RELEASE.md"}"#;
        assert_eq!(
            json, golden,
            "the canonical body drifted from the golden literal - this exact byte shape must stay \
             stable for every item already in a real store, check field or no"
        );
    }

    #[test]
    fn every_check_form_round_trips_through_json() {
        for check in [
            Check::PathExists { path: "README.md".to_string() },
            Check::Contains { path: "README.md".to_string(), literal: "GPLv3".to_string() },
            Check::Absent { path: "README.md".to_string(), literal: "TODO".to_string() },
            Check::AbsentAll { path: "README.md".to_string(), literals: vec!["TODO".to_string(), "FIXME".to_string()] },
            Check::Forbidden { literals: vec!["TODO".to_string(), "FIXME".to_string()] },
        ] {
            let json = serde_json::to_string(&check).unwrap();
            let back: Check = serde_json::from_str(&json).unwrap();
            assert_eq!(check, back, "round trip failed for {json}");
        }
    }

    #[test]
    fn a_check_round_trips_through_a_full_item() {
        let mut item = sample_rule();
        item.check = Some(Check::Contains { path: "README.md".to_string(), literal: "GPLv3".to_string() });
        let json = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn a_check_appears_after_falsifier_in_field_order_when_present() {
        // The struct's declared field order is the wire order (see this
        // file's own module doc comment and `canonical_json_field_order_is_
        // deterministic` above): `check` is declared last, right after
        // `falsifier`, so it must serialise last too, whenever it is set.
        let mut item = sample_rule();
        item.check = Some(Check::PathExists { path: "README.md".to_string() });
        let json = serde_json::to_string(&item).unwrap();
        let falsifier_pos = json.find("\"falsifier\"").unwrap_or_else(|| panic!("falsifier missing from {json}"));
        let check_pos = json.find("\"check\"").unwrap_or_else(|| panic!("check missing from {json}"));
        assert!(falsifier_pos < check_pos, "check must serialise after falsifier, the struct's declared field order: {json}");
    }

    // ------------------------------------------- check field - AbsentAll variant
    //
    // THE DEFECT THESE PREVENT: every item already in a real store that
    // carries a `Check::PathExists`/`Contains`/`Absent` was serialised
    // before `AbsentAll` existed. `Check` has no `#[serde(tag = ...)]` of
    // its own, so it uses serde's default externally-tagged representation
    // - each variant's own wire shape depends only on ITS OWN name and
    // fields, never on how many OTHER variants the enum happens to declare.
    // Adding a fourth variant must therefore change nothing about how the
    // three existing ones serialise or parse. Proven two ways, mirroring
    // the falsifier/check-field sections above: a hand-written OLD-style
    // body (never mentioning "absent_all") still parses, and a `Check::
    // Absent` value still serialises to the exact same golden literal it
    // always did.

    #[test]
    fn a_stored_body_carrying_the_old_absent_form_still_parses_after_absent_all_exists() {
        let old_body = r#"{"id":"pre-existing-2","kind":"orientation","text":"keep main.rs free of TODOs","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null,"falsifier":"a TODO lands in main.rs and stays there a week","check":{"absent":{"path":"src/main.rs","literal":"TODO"}}}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body carrying the old Absent form must still parse");
        assert_eq!(item.check, Some(Check::Absent { path: "src/main.rs".to_string(), literal: "TODO".to_string() }));
    }

    #[test]
    fn a_stored_body_carrying_path_exists_still_parses_after_absent_all_exists() {
        let old_body = r#"{"id":"pre-existing-3","kind":"rule","text":"the changelog lives in CHANGELOG.md","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null,"falsifier":"CHANGELOG.md is removed or renamed","check":{"path_exists":{"path":"CHANGELOG.md"}}}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body carrying the old PathExists form must still parse");
        assert_eq!(item.check, Some(Check::PathExists { path: "CHANGELOG.md".to_string() }));
    }

    #[test]
    fn a_stored_body_carrying_contains_still_parses_after_absent_all_exists() {
        let old_body = r#"{"id":"pre-existing-4","kind":"rule","text":"LICENSE keeps the GPLv3 grant","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null,"falsifier":"LICENSE stops mentioning GPLv3","check":{"contains":{"path":"LICENSE","literal":"GPLv3"}}}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body carrying the old Contains form must still parse");
        assert_eq!(item.check, Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }));
    }

    #[test]
    fn adding_absent_all_does_not_change_how_path_exists_serialises() {
        let json = serde_json::to_string(&Check::PathExists { path: "CHANGELOG.md".to_string() }).unwrap();
        assert_eq!(json, r#"{"path_exists":{"path":"CHANGELOG.md"}}"#);
    }

    #[test]
    fn adding_absent_all_does_not_change_how_contains_serialises() {
        let json =
            serde_json::to_string(&Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }).unwrap();
        assert_eq!(json, r#"{"contains":{"path":"LICENSE","literal":"GPLv3"}}"#);
    }

    #[test]
    fn adding_absent_all_does_not_change_how_absent_serialises() {
        let json =
            serde_json::to_string(&Check::Absent { path: "src/main.rs".to_string(), literal: "TODO".to_string() }).unwrap();
        assert_eq!(json, r#"{"absent":{"path":"src/main.rs","literal":"TODO"}}"#);
    }

    #[test]
    fn absent_all_serialises_its_literal_list_as_a_json_array_in_declared_order() {
        let check = Check::AbsentAll {
            path: "docs/STYLE.md".to_string(),
            literals: vec!["TODO".to_string(), "FIXME".to_string()],
        };
        let json = serde_json::to_string(&check).unwrap();
        assert_eq!(json, r#"{"absent_all":{"path":"docs/STYLE.md","literals":["TODO","FIXME"]}}"#);
    }

    #[test]
    fn a_fully_populated_item_carrying_absent_all_serialises_byte_for_byte_as_a_golden_literal() {
        // The AbsentAll counterpart to `a_fully_populated_item_without_a_
        // check_serialises_byte_for_byte_as_a_golden_literal` above: pins
        // the WHOLE canonical body, byte for byte, against a hand-written
        // literal, so a future change to `AbsentAll`'s own field order or
        // shape is caught here rather than silently changing what gets
        // hashed into the log for every item that comes to carry one.
        let mut item = Item {
            id: "golden-lookup-1".to_string(),
            kind: Kind::Lookup,
            text: "the release checklist lives in RELEASE.md".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "RELEASE.md".to_string() }],
            severity: Some(Severity::HouseStyle),
            project: Some("thor2".to_string()),
            tags: vec!["release".to_string(), "process".to_string()],
            expires: None,
            key: Some("release-checklist".to_string()),
            falsifier: Some("the release checklist moves out of RELEASE.md".to_string()),
            check: None,
        };
        item.check = Some(Check::AbsentAll {
            path: "RELEASE.md".to_string(),
            literals: vec!["TODO".to_string(), "FIXME".to_string()],
        });
        let json = serde_json::to_string(&item).unwrap();
        let golden = r#"{"id":"golden-lookup-1","kind":"lookup","text":"the release checklist lives in RELEASE.md","bindings":[{"target":{"kind":"path","value":"RELEASE.md"}}],"severity":"house_style","project":"thor2","tags":["release","process"],"expires":null,"key":"release-checklist","falsifier":"the release checklist moves out of RELEASE.md","check":{"absent_all":{"path":"RELEASE.md","literals":["TODO","FIXME"]}}}"#;
        assert_eq!(json, golden, "the canonical body for an item carrying AbsentAll drifted from the golden literal: {json}");
    }

    // -------------------------------------------- check field - Forbidden variant
    //
    // THE DEFECT THESE PREVENT: every item already in a real store that
    // carries a `Check::PathExists`/`Contains`/`Absent`/`AbsentAll` was
    // serialised before `Forbidden` existed. `Check` has no `#[serde(tag =
    // ...)]` of its own, so - exactly as the "AbsentAll variant" section
    // above already proved once for the first four - each variant's own
    // wire shape depends only on ITS OWN name and fields, never on how many
    // OTHER variants the enum happens to declare. Adding a fifth variant
    // must therefore change nothing about how the four existing ones
    // serialise or parse. Proven the same two ways as before: a
    // hand-written OLD-style body (never mentioning "forbidden") still
    // parses for each of the four, and each of the four still serialises to
    // the exact same golden literal it always did.

    #[test]
    fn a_stored_body_carrying_the_old_absent_form_still_parses_after_forbidden_exists() {
        let old_body = r#"{"id":"pre-existing-5","kind":"orientation","text":"keep main.rs free of TODOs","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null,"falsifier":"a TODO lands in main.rs and stays there a week","check":{"absent":{"path":"src/main.rs","literal":"TODO"}}}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body carrying the old Absent form must still parse");
        assert_eq!(item.check, Some(Check::Absent { path: "src/main.rs".to_string(), literal: "TODO".to_string() }));
    }

    #[test]
    fn a_stored_body_carrying_path_exists_still_parses_after_forbidden_exists() {
        let old_body = r#"{"id":"pre-existing-6","kind":"rule","text":"the changelog lives in CHANGELOG.md","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null,"falsifier":"CHANGELOG.md is removed or renamed","check":{"path_exists":{"path":"CHANGELOG.md"}}}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body carrying the old PathExists form must still parse");
        assert_eq!(item.check, Some(Check::PathExists { path: "CHANGELOG.md".to_string() }));
    }

    #[test]
    fn a_stored_body_carrying_contains_still_parses_after_forbidden_exists() {
        let old_body = r#"{"id":"pre-existing-7","kind":"rule","text":"LICENSE keeps the GPLv3 grant","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null,"falsifier":"LICENSE stops mentioning GPLv3","check":{"contains":{"path":"LICENSE","literal":"GPLv3"}}}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body carrying the old Contains form must still parse");
        assert_eq!(item.check, Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }));
    }

    #[test]
    fn a_stored_body_carrying_absent_all_still_parses_after_forbidden_exists() {
        // Unlike the three above, AbsentAll itself did not predate THIS
        // variant's own predecessor test section - it is now also an "old"
        // form in exactly the sense this whole section exists to protect.
        let old_body = r#"{"id":"pre-existing-8","kind":"orientation","text":"docs/STYLE.md keeps no stray TODO or FIXME markers","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null,"falsifier":"a TODO or FIXME marker lands in docs/STYLE.md and stays a week","check":{"absent_all":{"path":"docs/STYLE.md","literals":["TODO","FIXME"]}}}"#;
        let item: Item = serde_json::from_str(old_body).expect("a body carrying the old AbsentAll form must still parse");
        assert_eq!(
            item.check,
            Some(Check::AbsentAll { path: "docs/STYLE.md".to_string(), literals: vec!["TODO".to_string(), "FIXME".to_string()] })
        );
    }

    #[test]
    fn adding_forbidden_does_not_change_how_path_exists_serialises() {
        let json = serde_json::to_string(&Check::PathExists { path: "CHANGELOG.md".to_string() }).unwrap();
        assert_eq!(json, r#"{"path_exists":{"path":"CHANGELOG.md"}}"#);
    }

    #[test]
    fn adding_forbidden_does_not_change_how_contains_serialises() {
        let json =
            serde_json::to_string(&Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }).unwrap();
        assert_eq!(json, r#"{"contains":{"path":"LICENSE","literal":"GPLv3"}}"#);
    }

    #[test]
    fn adding_forbidden_does_not_change_how_absent_serialises() {
        let json =
            serde_json::to_string(&Check::Absent { path: "src/main.rs".to_string(), literal: "TODO".to_string() }).unwrap();
        assert_eq!(json, r#"{"absent":{"path":"src/main.rs","literal":"TODO"}}"#);
    }

    #[test]
    fn adding_forbidden_does_not_change_how_absent_all_serialises() {
        let json = serde_json::to_string(&Check::AbsentAll {
            path: "docs/STYLE.md".to_string(),
            literals: vec!["TODO".to_string(), "FIXME".to_string()],
        })
        .unwrap();
        assert_eq!(json, r#"{"absent_all":{"path":"docs/STYLE.md","literals":["TODO","FIXME"]}}"#);
    }

    #[test]
    fn forbidden_serialises_its_literal_list_as_a_json_array_in_declared_order() {
        let check = Check::Forbidden { literals: vec!["TODO".to_string(), "FIXME".to_string()] };
        let json = serde_json::to_string(&check).unwrap();
        assert_eq!(json, r#"{"forbidden":{"literals":["TODO","FIXME"]}}"#);
    }

    #[test]
    fn forbidden_serialises_with_no_path_key_at_all() {
        // THE DEFECT THIS PREVENTS: Forbidden is deliberately the one form
        // with nothing to anchor - if a future edit ever gave it a path
        // field (even one nothing reads), this catches it directly, rather
        // than relying on the golden-literal test below alone to notice a
        // stray key.
        let json = serde_json::to_string(&Check::Forbidden { literals: vec!["TODO".to_string()] }).unwrap();
        assert!(!json.contains("\"path\""), "Forbidden must carry no path key at all: {json}");
    }

    #[test]
    fn a_fully_populated_item_carrying_forbidden_serialises_byte_for_byte_as_a_golden_literal() {
        // The Forbidden counterpart to `a_fully_populated_item_carrying_
        // absent_all_serialises_byte_for_byte_as_a_golden_literal` above:
        // pins the WHOLE canonical body, byte for byte, against a
        // hand-written literal, so a future change to `Forbidden`'s own
        // field shape is caught here rather than silently changing what
        // gets hashed into the log for every item that comes to carry one.
        let mut item = Item {
            id: "golden-lookup-1".to_string(),
            kind: Kind::Lookup,
            text: "the release checklist lives in RELEASE.md".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "RELEASE.md".to_string() }],
            severity: Some(Severity::HouseStyle),
            project: Some("thor2".to_string()),
            tags: vec!["release".to_string(), "process".to_string()],
            expires: None,
            key: Some("release-checklist".to_string()),
            falsifier: Some("the release checklist moves out of RELEASE.md".to_string()),
            check: None,
        };
        item.check = Some(Check::Forbidden { literals: vec!["TODO".to_string(), "FIXME".to_string()] });
        let json = serde_json::to_string(&item).unwrap();
        let golden = r#"{"id":"golden-lookup-1","kind":"lookup","text":"the release checklist lives in RELEASE.md","bindings":[{"target":{"kind":"path","value":"RELEASE.md"}}],"severity":"house_style","project":"thor2","tags":["release","process"],"expires":null,"key":"release-checklist","falsifier":"the release checklist moves out of RELEASE.md","check":{"forbidden":{"literals":["TODO","FIXME"]}}}"#;
        assert_eq!(json, golden, "the canonical body for an item carrying Forbidden drifted from the golden literal: {json}");
    }

    #[test]
    fn target_kind_name_round_trips() {
        for (name, kind) in [
            ("path", TargetKind::Path),
            ("dir", TargetKind::Dir),
            ("symbol", TargetKind::Symbol),
            ("command", TargetKind::Command),
            ("project", TargetKind::Project),
            ("route", TargetKind::Route),
            ("host", TargetKind::Host),
        ] {
            assert_eq!(TargetKind::from_str(name).unwrap(), kind);
        }
        assert!(TargetKind::from_str("bogus").is_err());
    }
}

/// How many items one served block ever holds.
///
/// WHY A DELIVERY NUMBER LIVES IN THE ITEM MODEL. The write gate has to be
/// able to ask "would this item ever be shown", and the answer depends on how
/// many places a block has. `serve` already depends on `model`, so the
/// constant cannot live there and be read here without inverting that. It is
/// re-exported as `serve::render::MAX_ITEMS`, which stays the name every
/// existing caller uses: one definition, two doors.
pub const MAX_ITEMS: usize = 4;

/// Severity, heaviest first, with a missing severity sinking below every
/// declared one - never landing between costly and irreversible, and never
/// silently acting as if it were between irreversible and costly either.
/// (Ord's own default for `Option<Severity>` puts `None` FIRST, which would
/// rank a severity-less item as if it outranked `Irreversible` - exactly the
/// silent-middle-value failure this function exists to refuse.)
pub fn severity_rank(severity: Option<Severity>) -> u8 {
    match severity {
        Some(Severity::Irreversible) => 0,
        Some(Severity::Costly) => 1,
        Some(Severity::HouseStyle) => 2,
        None => 3,
    }
}
