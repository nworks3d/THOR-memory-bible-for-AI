//! Acceptance measurement for the Absent-check / Location guard
//! (`serve::absent_guard`) - NOT another unit or integration test. The
//! roadmap's own acceptance criterion for this guard is a set of real
//! dangerous actions (does it block?) against a set of innocent actions
//! (does it pass?), reported as two rates together: a CATCH RATE and a
//! FALSE-BLOCK RATE. `serve/src/absent_guard.rs`'s own unit tests and
//! `serve/tests/absent_check_guard.rs` / `serve/tests/location_guard.rs`
//! already prove the mechanism works on the handful of cases someone
//! thought of while building it. That is not a measurement. This is: a
//! synthetic but wide corpus, scored against the real decision path, with
//! both rates printed every time, together, whether the run is perfect or
//! not.
//!
//! WHICH FUNCTIONS THIS MEASURES. Exactly the two public, pure decision
//! functions `serve::absent_guard` exports:
//!   - `absent_guard::find_location_violation` (the location prohibition)
//!   - `absent_guard::find_violation` (the content prohibition)
//! composed by `decide` below in the SAME order the real caller uses:
//! location first, content second, via `.or_else` - see
//! `serve/src/bin/serve.rs`'s own `absent_guard_block`, which documents that
//! ordering ("LOCATION FIRST, deliberately: it is the broader refusal").
//! `absent_guard_block` itself is a private `fn` inside a `[[bin]]` target
//! (`serve/src/bin/serve.rs`), not part of any library surface, so it
//! cannot be imported here - this file's own constraints also forbid
//! editing that file to make it `pub`. `decide` below is therefore a
//! composition of the two real, already-tested, already-public decision
//! functions in their documented order, never a reimplementation of either
//! one's own matching, currency or ranking logic - that logic all still
//! lives, unmodified, in `serve::absent_guard`, `serve::live` and
//! `serve::rank`, exactly as it does for the real `hook` binary.
//!
//! DELIBERATELY NOT MODELLED, and why that is still an honest measurement:
//!   - The once-per-session-per-file marker file
//!     (`absent_guard::default_marker_path` /
//!     `already_blocked` / `mark_blocked_text`). That is session-repetition
//!     bookkeeping on top of the decision, not the decision itself, and it
//!     already has its own dedicated coverage (`a_second_write_to_the_same_
//!     file_in_the_same_session_is_not_blocked_again` in both
//!     `serve/tests/absent_check_guard.rs` and `serve/tests/location_guard.rs`).
//!     Every case in this corpus is an independent simulated tool call - a
//!     fresh moment, not a session replaying itself - so each one is run
//!     against a marker-free decision on purpose: nothing here should be
//!     allowed to "pass" only because an earlier case in this same process
//!     already tripped the marker for the same path.
//!   - Project scoping (`serve::project::applies_to`). Every seeded rule and
//!     every simulated call in this corpus carries `project: None` (the
//!     global tier), so scoping can never hide or manufacture a result here.
//!   - The compiled `serve hook` binary's JSON stdout envelope. The two test
//!     files named above already spawn the real binary end to end and prove
//!     that wiring; re-spawning a process per case here would add nothing to
//!     THIS measurement (the JSON envelope is a pure function of whatever
//!     `decide` below already returns) while making a 100-plus-case corpus
//!     far slower to run.
//!
//! CORPUS CONSTRUCTION. See the `SHOULD-BLOCK` and `SHOULD-PASS` sections
//! below for the full, commented list. In short: six realistic content
//! rules (a forbidden word, an em dash, a panic-prone `unwrap()`, a leaked
//! secret, a leftover debug log, an open security-group CIDR) each tried
//! against seven ways the literal can appear in a real write (start,
//! middle, end, inside a code fence, buried in a longer document, twice,
//! and pressed directly against a similar-but-different character);
//! eight realistic protected locations (a frozen directory, a vendored
//! dependency tree, a legacy archive, `node_modules`, `.git`, and three
//! exact protected files) each tried at least once directly inside and
//! once deep inside. The should-pass side mirrors every near-miss the
//! guard's own doc comments and unit tests name as a defect class it must
//! refuse to trip on: a look-alike character, the same literal in an
//! unrelated file, a directory that only shares a text prefix with a
//! protected one, a rule whose anchor has gone missing, a rule with prose
//! but no check, a rule whose check is real but whose severity is not
//! `Irreversible`, an ordinary file no rule has ever heard of, and a tool
//! this guard has no notion of at all - plus a few extra near-misses pulled
//! directly from the unit tests (a check of the wrong form, a check
//! pointing at a different path than its own binding) for extra rigor
//! beyond the minimum list.
//!
//! Run (no environment variables, no external store - fully self-contained):
//!   cargo run -p serve --example guard_acceptance

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use serve::absent_guard;
use serve::input::ServeInput;
use serve::live;
use serve::rank;
use std::fs;
use std::path::Path;
use thor_core::event_store::EventStore;

/// The exact character this workspace's own typography rule forbids in
/// prose - written only ever as this one unicode escape, per that same rule
/// (see `serve/tests/absent_check_guard.rs`'s own module doc comment, which
/// states the identical convention).
const EM_DASH: char = '\u{2014}';
/// A DIFFERENT, merely similar-looking dash character - never the one any
/// rule in this corpus forbids. Used both as a should-block "adjacent
/// confusable" and as a should-pass "similar-but-different character".
const EN_DASH: char = '\u{2013}';

// ------------------------------------------------------------- fixture I/O

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("create parent dir for {rel}: {e}"));
    }
    fs::write(&path, content).unwrap_or_else(|e| panic!("write fixture file {rel}: {e}"));
}

fn make_dir(root: &Path, rel: &str) {
    fs::create_dir_all(root.join(rel)).unwrap_or_else(|e| panic!("create fixture dir {rel}: {e}"));
}

fn abs(root: &Path, rel: &str) -> String {
    root.join(rel).to_string_lossy().into_owned()
}

// --------------------------------------------------------- payload builders

fn w(root: &Path, rel: &str, content: &str) -> serde_json::Value {
    serde_json::json!({ "file_path": abs(root, rel), "content": content })
}

/// A `Write` payload with no `content` field at all - the malformed-payload
/// near miss (`absent_guard::proposed_content` must read this as `None`,
/// never as empty-string content).
fn w_no_content(root: &Path, rel: &str) -> serde_json::Value {
    serde_json::json!({ "file_path": abs(root, rel) })
}

fn ed(root: &Path, rel: &str, old_string: &str, new_string: &str) -> serde_json::Value {
    serde_json::json!({ "file_path": abs(root, rel), "old_string": old_string, "new_string": new_string })
}

// ------------------------------------------------------------ item builders

fn content_rule(id: &str, path: &str, literal: &str, severity: Severity, text: &str, falsifier: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Path, value: path.to_string() }],
        severity: Some(severity),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: Some(Check::Absent { path: path.to_string(), literal: literal.to_string() }),
    }
}

fn location_rule(id: &str, kind: TargetKind, path: &str, text: &str, falsifier: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind, value: path.to_string() }],
        severity: Some(Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: Some(Check::PathExists { path: path.to_string() }),
    }
}

fn prose_only_content_rule(id: &str, path: &str, text: &str, falsifier: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Path, value: path.to_string() }],
        severity: Some(Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: None,
    }
}

fn prose_only_location_rule(id: &str, kind: TargetKind, path: &str, text: &str, falsifier: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind, value: path.to_string() }],
        severity: Some(Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: None,
    }
}

fn location_rule_with_severity(
    id: &str,
    kind: TargetKind,
    path: &str,
    severity: Option<Severity>,
    text: &str,
    falsifier: &str,
) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind, value: path.to_string() }],
        severity,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: Some(Check::PathExists { path: path.to_string() }),
    }
}

fn content_rule_with_check(id: &str, path: &str, check: Check, severity: Severity, text: &str, falsifier: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Path, value: path.to_string() }],
        severity: Some(severity),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: Some(check),
    }
}

fn location_rule_mismatched_anchor(
    id: &str,
    kind: TargetKind,
    binding_path: &str,
    check_path: &str,
    text: &str,
    falsifier: &str,
) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind, value: binding_path.to_string() }],
        severity: Some(Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: Some(Check::PathExists { path: check_path.to_string() }),
    }
}

// ------------------------------------------------------- content variations

/// A four-paragraph document with `literal` buried in the third paragraph -
/// the "inside a longer document" should-block variation. `lead`/`tail` give
/// each pair its own topical flavour rather than reusing identical filler
/// text for all six pairs.
fn long_document(lead: &str, literal: &str, tail: &str) -> String {
    format!(
        "{lead}\n\n\
This paragraph is deliberately unrelated filler, establishing context before the interesting part \
of the document even begins, so a reader has to read past the opening before anything stands out.\n\n\
This second paragraph keeps padding the document out to something closer to a realistic length, \
still saying nothing in particular, before the part that actually matters shows up at all.\n\n\
And here, in the third paragraph, is the part that matters: {literal} appears right here, buried in \
the middle of an otherwise ordinary-looking longer document rather than sitting at either edge of it.\n\n\
{tail}"
    )
}

/// `literal` pressed directly against a DIFFERENT, merely similar-looking
/// dash character on both sides, with no whitespace between them - the
/// "adjacent to similar-but-different characters" should-block variation:
/// proves the guard is not fooled into a false NEGATIVE by lookalike noise
/// crowding the real literal.
fn adjacent_confusable(prefix: &str, literal: &str, suffix: &str) -> String {
    let dash = EN_DASH;
    format!("{prefix}{dash}{literal}{dash}{suffix}")
}

/// `literal` twice in the same write, at two different positions.
fn twice(lead: &str, literal: &str, mid: &str) -> String {
    format!("{lead} {literal} {mid} {literal}")
}

/// `literal` inside a fenced code block, with ordinary prose above and below
/// it - the "inside a code fence" should-block variation.
fn fenced(above: &str, code_prefix: &str, literal: &str, below: &str) -> String {
    format!("{above}\n\n```\n{code_prefix}{literal}\n```\n\n{below}")
}

// --------------------------------------------------------------- the corpus

/// One simulated `PreToolUse` call plus the outcome it should produce.
struct Case {
    id: &'static str,
    category: &'static str,
    tool_name: &'static str,
    tool_input: serde_json::Value,
    expect_block: bool,
    note: String,
}

fn case(
    id: &'static str,
    category: &'static str,
    tool_name: &'static str,
    tool_input: serde_json::Value,
    expect_block: bool,
    note: impl Into<String>,
) -> Case {
    Case { id, category, tool_name, tool_input, expect_block, note: note.into() }
}

/// The real decision, composed exactly the way `serve/src/bin/serve.rs`'s
/// private `absent_guard_block` composes it (see this file's own module doc
/// comment): `file_path` and `content` are both read from `tool_input`
/// first (a payload that resolves neither - the wrong tool, a missing
/// field - short-circuits the WHOLE guard, location half included, exactly
/// as the real caller does), then the location prohibition is tried before
/// the content one, and only a location miss ever falls through to content.
/// Never touches the once-per-session marker file (see this file's own
/// module doc comment for why that is deliberately out of scope here).
fn decide(store: &EventStore, root: &Path, tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
    let file_path = tool_input.get("file_path").and_then(|v| v.as_str())?;
    let content = absent_guard::proposed_content(tool_name, Some(tool_input))?;

    let mut location_input = ServeInput::default();
    location_input.add_target(TargetKind::Path, file_path);
    location_input.add_target(TargetKind::Dir, file_path);
    let location_candidates = live::candidates_for(store, &location_input);

    absent_guard::find_location_violation(&location_candidates, file_path, Some(root)).or_else(|| {
        let mut input = ServeInput::default();
        input.add_file(file_path);
        let candidates = live::candidates_for(store, &input);
        let ranked = rank::select(&candidates, &input);
        absent_guard::find_violation(&ranked, file_path, content, Some(root))
    })
}

fn main() {
    // ------------------------------------------------------------- fixture
    let tmp = tempfile::tempdir().expect("create a throwaway temp dir for this run");
    let root = tmp.path().to_path_buf();
    let db_path = root.join("thor.db");
    let mut store = EventStore::new(&db_path).expect("create a throwaway store in the temp dir");

    // Six content-rule anchors, varying extension and depth.
    write_file(&root, "NOTES.md", "placeholder notes, nothing forbidden here yet\n");
    write_file(&root, "docs/POLICY.md", "placeholder house style policy\n");
    write_file(&root, "src/config.rs", "// placeholder config module\n");
    write_file(&root, "deploy/prod.env", "PLACEHOLDER=1\n");
    write_file(&root, "server/routes/admin.ts", "// placeholder admin route\n");
    write_file(&root, "infra/terraform/main.tf", "# placeholder terraform module\n");

    // Eight location-rule anchors: five protected directories, three exact
    // protected files, varying depth and extension.
    make_dir(&root, "frozen");
    make_dir(&root, "vendor/locked");
    make_dir(&root, "legacy/archive");
    make_dir(&root, "node_modules");
    make_dir(&root, ".git");
    write_file(&root, "secrets.env", "PLACEHOLDER=1\n");
    write_file(&root, "config/prod.key", "placeholder\n");
    write_file(&root, "infra/terraform/state.tfstate", "{}\n");

    // D-category fixtures: a currency check must fail cleanly two ways - an
    // anchor that never existed, and one that existed and is now gone.
    write_file(&root, "tmp/STALE.md", "will be removed before any case runs\n");
    fs::remove_file(root.join("tmp/STALE.md")).expect("remove tmp/STALE.md for the currency-fails fixture");
    make_dir(&root, "sandbox/locked");
    fs::remove_dir_all(root.join("sandbox/locked")).expect("remove sandbox/locked for the currency-fails fixture");
    write_file(&root, "release/CHANGELOG.lock", "will be removed before any case runs\n");
    fs::remove_file(root.join("release/CHANGELOG.lock"))
        .expect("remove release/CHANGELOG.lock for the currency-fails fixture");
    // cache/BUILD-NOTES.md, scripts/deploy.sh, quarantine, restricted/zone
    // and keys/prod.pem are deliberately never created at all.

    // E-category fixtures: a real anchor, but the rule carries no check.
    write_file(&root, "ideas/ROADMAP.md", "placeholder roadmap\n");
    write_file(&root, "billing/INVOICES.md", "placeholder invoices\n");
    make_dir(&root, "staging");
    make_dir(&root, "archive/cold");
    write_file(&root, "important.lock", "placeholder\n");
    write_file(&root, "release/NOTES-DRAFT.md", "placeholder draft\n");

    // F-category fixtures: a real, current PathExists check, wrong severity.
    make_dir(&root, "beta/preview");
    make_dir(&root, "beta/hidden");
    write_file(&root, "keys/staging.pem", "placeholder\n");

    // X-category fixtures: a real anchor, but the check is Contains/PathExists.
    write_file(&root, "web/index.html", "<html></html>\n");
    write_file(&root, "web/footer.html", "<footer></footer>\n");

    // Y-category fixtures: the binding's own anchor exists, and so does the
    // (different) path the check actually names.
    make_dir(&root, "ops/keys");
    make_dir(&root, "ops/keys-real");
    make_dir(&root, "shared/vault");
    make_dir(&root, "shared/vault-legacy");

    // --------------------------------------------------------------- rules
    let em_dash_literal = EM_DASH.to_string();
    let rules: Vec<Item> = vec![
        // ---- six should-block content rules ----
        content_rule(
            "rule-notes-forbidden",
            "NOTES.md",
            "forbidden",
            Severity::HouseStyle,
            "NOTES.md must never contain the literal word forbidden anywhere in its body.",
            "the word forbidden lands in NOTES.md and nobody notices or minds",
        ),
        content_rule(
            "rule-policy-emdash",
            "docs/POLICY.md",
            &em_dash_literal,
            Severity::HouseStyle,
            "docs/POLICY.md must never carry an em dash character in its prose.",
            "an em dash lands in docs/POLICY.md and passes review anyway",
        ),
        content_rule(
            "rule-config-unwrap",
            "src/config.rs",
            "unwrap()",
            Severity::Costly,
            "src/config.rs must never call unwrap() directly on a Result value.",
            "a direct unwrap() in src/config.rs ships and never once panics in production",
        ),
        content_rule(
            "rule-prodenv-secret",
            "deploy/prod.env",
            "SECRET_KEY=",
            Severity::Irreversible,
            "deploy/prod.env must never commit a literal SECRET_KEY= assignment to version control.",
            "a literal SECRET_KEY= assignment sits in deploy/prod.env with no incident ever tracing back to it",
        ),
        content_rule(
            "rule-admints-consolelog",
            "server/routes/admin.ts",
            "console.log(",
            Severity::HouseStyle,
            "server/routes/admin.ts must never contain a leftover console.log( debug call.",
            "a console.log( call sits in server/routes/admin.ts and nobody ever trips over the noise",
        ),
        content_rule(
            "rule-terraform-cidr",
            "infra/terraform/main.tf",
            "0.0.0.0/0",
            Severity::Irreversible,
            "infra/terraform/main.tf must never open an ingress security rule to 0.0.0.0/0.",
            "an ingress rule open to 0.0.0.0/0 ships from infra/terraform/main.tf with no incident",
        ),
        // ---- eight should-block location rules ----
        location_rule(
            "rule-loc-frozen",
            TargetKind::Dir,
            "frozen",
            "frozen is a frozen archive directory and must never be written to again.",
            "frozen receives a new write and nothing about the project breaks",
        ),
        location_rule(
            "rule-loc-vendor-locked",
            TargetKind::Dir,
            "vendor/locked",
            "vendor/locked holds vendored third-party code pinned at a fixed revision and must never be edited in place.",
            "vendor/locked is hand edited and the next vendor refresh does not silently discard the edit",
        ),
        location_rule(
            "rule-loc-legacy-archive",
            TargetKind::Dir,
            "legacy/archive",
            "legacy/archive is retained for historical reference only and must never receive a new write.",
            "legacy/archive gains a new write and the historical record stays trustworthy anyway",
        ),
        location_rule(
            "rule-loc-secrets-env",
            TargetKind::Path,
            "secrets.env",
            "secrets.env holds live credentials and must never be overwritten by an automated write.",
            "secrets.env is overwritten by a tool call and no credential is ever affected",
        ),
        location_rule(
            "rule-loc-config-prodkey",
            TargetKind::Path,
            "config/prod.key",
            "config/prod.key is the production signing key and must never be replaced by a tool call.",
            "config/prod.key is replaced by a tool call and production signing keeps working unaffected",
        ),
        location_rule(
            "rule-loc-terraform-state",
            TargetKind::Path,
            "infra/terraform/state.tfstate",
            "infra/terraform/state.tfstate is the live terraform state file and must never be hand edited.",
            "infra/terraform/state.tfstate is hand edited and the next apply reconciles cleanly anyway",
        ),
        location_rule(
            "rule-loc-node-modules",
            TargetKind::Dir,
            "node_modules",
            "node_modules is a generated dependency tree and must never be hand edited by a tool call.",
            "node_modules is hand edited and the next install does not silently discard the edit",
        ),
        location_rule(
            "rule-loc-dotgit",
            TargetKind::Dir,
            ".git",
            ".git holds the repository's own internal object database and must never be written to directly.",
            ".git is written to directly and the repository's history stays intact regardless",
        ),
        // ---- D: currency-fails near-miss rules ----
        content_rule(
            "rule-d-cache-buildnotes",
            "cache/BUILD-NOTES.md",
            "DEPRECATED",
            Severity::HouseStyle,
            "cache/BUILD-NOTES.md must never describe a DEPRECATED build step as still active.",
            "cache/BUILD-NOTES.md calls a DEPRECATED step active and someone follows it anyway",
        ),
        content_rule(
            "rule-d-deploy-script",
            "scripts/deploy.sh",
            "curl | sh",
            Severity::Irreversible,
            "scripts/deploy.sh must never pipe a remote curl download straight into sh.",
            "scripts/deploy.sh pipes curl straight into sh and the download source is never once verified",
        ),
        content_rule(
            "rule-d-stale-md",
            "tmp/STALE.md",
            "LEAKED",
            Severity::Irreversible,
            "tmp/STALE.md must never record a LEAKED credential without an immediate rotation noted alongside it.",
            "tmp/STALE.md records a LEAKED credential and the credential is never rotated",
        ),
        prose_only_location_rule(
            "rule-d-quarantine",
            TargetKind::Dir,
            "quarantine",
            "quarantine holds material pending review and must never be written to directly.",
            "quarantine receives a direct write and the pending review is not affected by it",
        ),
        location_rule(
            "rule-d-restricted-zone",
            TargetKind::Dir,
            "restricted/zone",
            "restricted/zone is off limits to every automated write, without exception.",
            "restricted/zone takes an automated write and nothing about it turns out to matter",
        ),
        location_rule(
            "rule-d-keys-prodpem",
            TargetKind::Path,
            "keys/prod.pem",
            "keys/prod.pem is the production TLS private key and must never be replaced by a tool call.",
            "keys/prod.pem is replaced by a tool call and production TLS keeps working unaffected",
        ),
        location_rule(
            "rule-d-sandbox-locked",
            TargetKind::Dir,
            "sandbox/locked",
            "sandbox/locked is a locked experiment directory and must never be written to.",
            "sandbox/locked is written to and the locked experiment's result stays valid anyway",
        ),
        location_rule(
            "rule-d-release-changelock",
            TargetKind::Path,
            "release/CHANGELOG.lock",
            "release/CHANGELOG.lock pins the published changelog and must never be edited after release.",
            "release/CHANGELOG.lock is edited after release and the published changelog stays accurate anyway",
        ),
        // ---- E: prose-only near-miss rules (no check at all) ----
        prose_only_content_rule(
            "rule-e-roadmap",
            "ideas/ROADMAP.md",
            "ideas/ROADMAP.md should stay focused on the next two quarters, not the whole year at once.",
            "ideas/ROADMAP.md drifts past two quarters out and still reads as useful to plan against",
        ),
        prose_only_content_rule(
            "rule-e-invoices",
            "billing/INVOICES.md",
            "billing/INVOICES.md should list each invoice in chronological order for the accountant.",
            "billing/INVOICES.md goes out of order and the accountant never once notices or minds",
        ),
        prose_only_location_rule(
            "rule-e-staging",
            TargetKind::Dir,
            "staging",
            "staging should mirror production closely enough that a deploy rehearsal there actually means something.",
            "staging drifts from production and a rehearsal there still predicts the real deploy correctly",
        ),
        prose_only_location_rule(
            "rule-e-archive-cold",
            TargetKind::Dir,
            "archive/cold",
            "archive/cold is where a project goes once nobody expects to touch it again soon.",
            "archive/cold turns out to need regular edits after all",
        ),
        prose_only_location_rule(
            "rule-e-importantlock",
            TargetKind::Path,
            "important.lock",
            "important.lock should only ever be regenerated by the release tool, never typed by hand.",
            "important.lock is typed by hand and the release tool accepts it without complaint",
        ),
        prose_only_content_rule(
            "rule-e-notesdraft",
            "release/NOTES-DRAFT.md",
            "release/NOTES-DRAFT.md should read like a first pass, not a polished announcement yet.",
            "release/NOTES-DRAFT.md reads as polished long before release and that turns out fine",
        ),
        // ---- F: wrong-severity near-miss rules ----
        location_rule_with_severity(
            "rule-f-beta-preview",
            TargetKind::Dir,
            "beta/preview",
            Some(Severity::Costly),
            "beta/preview carries an early build that is expensive to regenerate if it breaks.",
            "beta/preview breaks and regenerating it turns out to be cheap after all",
        ),
        location_rule_with_severity(
            "rule-f-beta-hidden",
            TargetKind::Dir,
            "beta/hidden",
            Some(Severity::HouseStyle),
            "beta/hidden should keep its own README up to date with whatever is currently toggled on.",
            "beta/hidden's README goes stale and nobody relying on it is ever misled",
        ),
        location_rule_with_severity(
            "rule-f-keys-staging",
            TargetKind::Path,
            "keys/staging.pem",
            None,
            "keys/staging.pem rotates automatically every thirty days through the staging pipeline.",
            "keys/staging.pem stops rotating and the staging pipeline keeps working unaffected",
        ),
        // ---- X: wrong check-form near-miss rules (content) ----
        content_rule_with_check(
            "rule-x-web-index",
            "web/index.html",
            Check::Contains { path: "web/index.html".to_string(), literal: "TRACKING_ID".to_string() },
            Severity::HouseStyle,
            "web/index.html should keep carrying its TRACKING_ID analytics snippet in the header.",
            "web/index.html loses its TRACKING_ID snippet and analytics keep reporting correctly anyway",
        ),
        content_rule_with_check(
            "rule-x-web-footer",
            "web/footer.html",
            Check::PathExists { path: "web/footer.html".to_string() },
            Severity::HouseStyle,
            "web/footer.html should keep the same footer links across every page of the site.",
            "web/footer.html's links diverge from the rest of the site and no visitor is ever confused by it",
        ),
        // ---- Y: mismatched-anchor near-miss rules (location) ----
        location_rule_mismatched_anchor(
            "rule-y-ops-keys",
            TargetKind::Dir,
            "ops/keys",
            "ops/keys-real",
            "ops/keys stores per-environment credentials issued by the platform team on request.",
            "ops/keys stops being where credentials are issued and nobody on the platform team notices",
        ),
        location_rule_mismatched_anchor(
            "rule-y-shared-vault",
            TargetKind::Dir,
            "shared/vault",
            "shared/vault-legacy",
            "shared/vault is where every team's shared secrets live once approved by security.",
            "shared/vault stops being where approved secrets live and security signs off on the drift",
        ),
    ];

    for item in &rules {
        model::store::declare(&mut store, "seed", "seed", "guard_acceptance", item)
            .unwrap_or_else(|e| panic!("seed rule '{}' was refused by the write gate: {e}", item.id));
    }

    // --------------------------------------------------------------- corpus
    let mut cases: Vec<Case> = Vec::new();
    push_should_block_content(&mut cases, &root);
    push_should_block_location(&mut cases, &root);
    push_should_pass_near_miss_character(&mut cases, &root);
    push_should_pass_different_file(&mut cases, &root);
    push_should_pass_shared_prefix(&mut cases, &root);
    push_should_pass_currency_gone(&mut cases, &root);
    push_should_pass_prose_only(&mut cases, &root);
    push_should_pass_wrong_severity(&mut cases, &root);
    push_should_pass_no_rule_nearby(&mut cases, &root);
    push_should_pass_unrelated_tool(&mut cases, &root);
    push_should_pass_clean_content(&mut cases, &root);
    push_should_pass_edit_old_string_only(&mut cases, &root);
    push_should_pass_malformed_payload(&mut cases, &root);
    push_should_pass_check_form_mismatch(&mut cases, &root);
    push_should_pass_anchor_mismatch(&mut cases, &root);

    // ----------------------------------------------------------------- run
    struct Result_ {
        id: &'static str,
        category: &'static str,
        tool_name: &'static str,
        file_path: String,
        expect_block: bool,
        actual: Option<String>,
        note: String,
    }

    let results: Vec<Result_> = cases
        .iter()
        .map(|c| {
            let file_path = c
                .tool_input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("<no file_path in payload>")
                .to_string();
            let actual = decide(&store, &root, c.tool_name, &c.tool_input);
            Result_ {
                id: c.id,
                category: c.category,
                tool_name: c.tool_name,
                file_path,
                expect_block: c.expect_block,
                actual,
                note: c.note.clone(),
            }
        })
        .collect();

    let should_block: Vec<&Result_> = results.iter().filter(|r| r.expect_block).collect();
    let should_pass: Vec<&Result_> = results.iter().filter(|r| !r.expect_block).collect();
    let misses: Vec<&&Result_> = should_block.iter().filter(|r| r.actual.is_none()).collect();
    let caught_count = should_block.len() - misses.len();
    let false_blocks: Vec<&&Result_> = should_pass.iter().filter(|r| r.actual.is_some()).collect();
    let clean_count = should_pass.len() - false_blocks.len();

    let pct = |n: usize, d: usize| -> f64 {
        if d == 0 { 0.0 } else { (n as f64 / d as f64) * 100.0 }
    };

    // -------------------------------------------------------------- report
    println!("=== THOR absent-guard acceptance corpus ===");
    println!();
    println!(
        "measuring: serve::absent_guard::find_location_violation + serve::absent_guard::find_violation,"
    );
    println!(
        "composed location-first-then-content, the same order serve/src/bin/serve.rs's absent_guard_block uses"
    );
    println!();
    println!("total cases:        {}", results.len());
    println!("should-block cases: {}", should_block.len());
    println!("should-pass cases:  {}", should_pass.len());
    println!();
    println!(
        "CATCH RATE (should-block set):      {caught_count}/{} caught ({:.1}%)",
        should_block.len(),
        pct(caught_count, should_block.len())
    );
    println!(
        "FALSE-BLOCK RATE (should-pass set):  {}/{} false-blocked ({:.1}%)",
        false_blocks.len(),
        should_pass.len(),
        pct(false_blocks.len(), should_pass.len())
    );
    println!(
        "RESULT: catch rate {caught_count}/{} ({:.1}%) | false-block rate {}/{} ({:.1}%)",
        should_block.len(),
        pct(caught_count, should_block.len()),
        false_blocks.len(),
        should_pass.len(),
        pct(false_blocks.len(), should_pass.len())
    );
    println!();
    println!("clean should-pass (correctly not blocked): {clean_count}/{}", should_pass.len());
    println!();

    println!("--- misses: should-block cases that were NOT blocked ({}) ---", misses.len());
    if misses.is_empty() {
        println!("(none)");
    } else {
        for r in &misses {
            println!("id: {}", r.id);
            println!("  category:  {}", r.category);
            println!("  tool_name: {}", r.tool_name);
            println!("  file_path: {}", r.file_path);
            println!("  why this should have blocked: {}", r.note);
            println!("  actual decision: not blocked");
        }
    }
    println!();

    println!("--- false blocks: should-pass cases that WERE blocked ({}) ---", false_blocks.len());
    if false_blocks.is_empty() {
        println!("(none)");
    } else {
        for r in &false_blocks {
            println!("id: {}", r.id);
            println!("  category:  {}", r.category);
            println!("  tool_name: {}", r.tool_name);
            println!("  file_path: {}", r.file_path);
            println!("  why this should have passed: {}", r.note);
            println!("  actual block reason: {}", r.actual.as_deref().unwrap_or("<none>"));
        }
    }
    println!();

    println!(
        "NOTE: this corpus was written by the same person being graded by it. It is a synthetic \
corpus, so a perfect score here shows only that the mechanism behaves as specified on cases \
someone imagined, never that it catches what a real session would actually do."
    );
}

// ============================================================================
// SHOULD-BLOCK: content. Six realistic rules, seven ways each for the
// forbidden literal to appear in a real write - start, middle, end, inside a
// code fence, buried in a longer document, twice, and pressed directly
// against a similar-but-different character. 6 x 7 = 42 cases.
// ============================================================================

fn push_should_block_content(cases: &mut Vec<Case>, root: &Path) {
    // ---- pair 1: NOTES.md / "forbidden" ----
    let lit = "forbidden";
    cases.push(case(
        "blk-c-notes-start", "should-block/content/start", "Write",
        w(root, "NOTES.md", "forbidden as it sounds, the team agreed to skip review for this one hotfix, and that is exactly the kind of note that belongs here."),
        true, "content opens with the literal 'forbidden', anchored via rule-notes-forbidden",
    ));
    cases.push(case(
        "blk-c-notes-middle", "should-block/content/middle", "Write",
        w(root, "NOTES.md", "Quick log entry for today: the deploy went fine, though one reviewer used the word forbidden to describe skipping the checklist, and that comment stayed in by accident."),
        true, "content carries 'forbidden' mid-sentence, anchored via rule-notes-forbidden",
    ));
    cases.push(case(
        "blk-c-notes-end", "should-block/content/end", "Write",
        w(root, "NOTES.md", "Nothing unusual happened during the sync, the backlog is unchanged, and the meeting closed on a single word nobody meant to leave in: forbidden"),
        true, "content ends with the literal 'forbidden', anchored via rule-notes-forbidden",
    ));
    cases.push(case(
        "blk-c-notes-fence", "should-block/content/fence", "Edit",
        ed(root, "NOTES.md", "TBD", &fenced("Pasted straight from the incident channel:", "status: ", lit, "Needs cleanup before this goes out.")),
        true, "new_string carries 'forbidden' inside a fenced code block, anchored via rule-notes-forbidden",
    ));
    cases.push(case(
        "blk-c-notes-longdoc", "should-block/content/longdoc", "Write",
        w(root, "NOTES.md", &long_document("Weekly notes for the team, nothing unusual expected below.", lit, "That is everything for this week.")),
        true, "content buries 'forbidden' in the third paragraph of a longer document, anchored via rule-notes-forbidden",
    ));
    cases.push(case(
        "blk-c-notes-twice", "should-block/content/twice", "Write",
        w(root, "NOTES.md", &twice("The draft used", lit, "once in the summary and then again in the closing line, so both need to go:")),
        true, "content contains 'forbidden' twice, anchored via rule-notes-forbidden",
    ));
    cases.push(case(
        "blk-c-notes-adjacent", "should-block/content/adjacent", "Write",
        w(root, "NOTES.md", &adjacent_confusable("boundary", lit, "zone marks where the old policy used to apply.")),
        true, "content presses 'forbidden' directly against en dash characters on both sides, anchored via rule-notes-forbidden",
    ));

    // ---- pair 2: docs/POLICY.md / em dash ----
    let lit = em_dash_str();
    let lit = lit.as_str();
    cases.push(case(
        "blk-c-policy-start", "should-block/content/start", "Write",
        w(root, "docs/POLICY.md", &format!("{lit} opens this paragraph before the actual policy text even begins.")),
        true, "content opens with an em dash, anchored via rule-policy-emdash",
    ));
    cases.push(case(
        "blk-c-policy-middle", "should-block/content/middle", "Write",
        w(root, "docs/POLICY.md", &format!("The policy reads clearly enough until a stray {lit} breaks the sentence in two right in the middle of the page.")),
        true, "content carries an em dash mid-sentence, anchored via rule-policy-emdash",
    ));
    cases.push(case(
        "blk-c-policy-end", "should-block/content/end", "Write",
        w(root, "docs/POLICY.md", &format!("Every rule on this page follows the house style guide without exception, except for one character at the very end: {lit}")),
        true, "content ends with an em dash, anchored via rule-policy-emdash",
    ));
    cases.push(case(
        "blk-c-policy-fence", "should-block/content/fence", "Write",
        w(root, "docs/POLICY.md", &fenced("Style notes above the block.", "title ", lit, "Copied from the old template by mistake.")),
        true, "content carries an em dash inside a fenced code block, anchored via rule-policy-emdash",
    ));
    cases.push(case(
        "blk-c-policy-longdoc", "should-block/content/longdoc", "Write",
        w(root, "docs/POLICY.md", &long_document("House style policy, revised this quarter.", lit, "Everything else on this page follows the same guide.")),
        true, "content buries an em dash in the third paragraph of a longer document, anchored via rule-policy-emdash",
    ));
    cases.push(case(
        "blk-c-policy-twice", "should-block/content/twice", "Edit",
        ed(root, "docs/POLICY.md", "TBD", &twice("The heading uses one", lit, "and the footer repeats the same character a second time:")),
        true, "new_string contains an em dash twice, anchored via rule-policy-emdash",
    ));
    cases.push(case(
        "blk-c-policy-adjacent", "should-block/content/adjacent", "Write",
        w(root, "docs/POLICY.md", &adjacent_confusable("range", lit, "range mixes several dash characters back to back on purpose.")),
        true, "content presses an em dash directly against en dash characters on both sides, anchored via rule-policy-emdash",
    ));

    // ---- pair 3: src/config.rs / "unwrap()" ----
    let lit = "unwrap()";
    cases.push(case(
        "blk-c-config-start", "should-block/content/start", "Write",
        w(root, "src/config.rs", "unwrap() is called first thing in this function, before any of the surrounding error handling is even set up."),
        true, "content opens with 'unwrap()', anchored via rule-config-unwrap",
    ));
    cases.push(case(
        "blk-c-config-middle", "should-block/content/middle", "Write",
        w(root, "src/config.rs", "fn load() -> Config { let raw = std::fs::read_to_string(PATH).unwrap(); toml::from_str(&raw).unwrap() }"),
        true, "content carries 'unwrap()' mid-line (twice, in real code), anchored via rule-config-unwrap",
    ));
    cases.push(case(
        "blk-c-config-end", "should-block/content/end", "Write",
        w(root, "src/config.rs", "The rest of this module handles errors carefully with a proper Result type, right up until the final line, which just calls .unwrap()"),
        true, "content ends with '.unwrap()', which contains the literal 'unwrap()', anchored via rule-config-unwrap",
    ));
    cases.push(case(
        "blk-c-config-fence", "should-block/content/fence", "Write",
        w(root, "src/config.rs", &fenced("Snippet from the crash report:", "let value = maybe_config.", lit, "This is almost certainly the panic site.")),
        true, "content carries 'unwrap()' inside a fenced code block, anchored via rule-config-unwrap",
    ));
    cases.push(case(
        "blk-c-config-longdoc", "should-block/content/longdoc", "Write",
        w(root, "src/config.rs", &long_document("// Module doc comment for the config loader, nothing unusual expected below.", lit, "// That covers the whole module.")),
        true, "content buries 'unwrap()' in the third paragraph of a longer document, anchored via rule-config-unwrap",
    ));
    cases.push(case(
        "blk-c-config-twice", "should-block/content/twice", "Write",
        w(root, "src/config.rs", &twice("let a = first.unwrap(); let b = second.", lit, "// both of these should really use ? instead of")),
        true, "content contains 'unwrap()' twice, anchored via rule-config-unwrap",
    ));
    cases.push(case(
        "blk-c-config-adjacent", "should-block/content/adjacent", "Edit",
        ed(root, "src/config.rs", "TBD", &adjacent_confusable("value", lit, "then continues chaining right after it with no space at all.")),
        true, "new_string presses 'unwrap()' directly against en dash characters on both sides, anchored via rule-config-unwrap",
    ));

    // ---- pair 4: deploy/prod.env / "SECRET_KEY=" ----
    let lit = "SECRET_KEY=";
    cases.push(case(
        "blk-c-prodenv-start", "should-block/content/start", "Write",
        w(root, "deploy/prod.env", "SECRET_KEY=hardcoded-value-here is the very first line of this environment file, which should never happen."),
        true, "content opens with 'SECRET_KEY=', anchored via rule-prodenv-secret",
    ));
    cases.push(case(
        "blk-c-prodenv-middle", "should-block/content/middle", "Write",
        w(root, "deploy/prod.env", "# production overrides\nDEBUG=false\nSECRET_KEY=abc123printthisnowhere\nPORT=8080\n"),
        true, "content carries 'SECRET_KEY=' on a middle line, anchored via rule-prodenv-secret",
    ));
    cases.push(case(
        "blk-c-prodenv-end", "should-block/content/end", "Write",
        w(root, "deploy/prod.env", "Every other line in this file is a harmless feature flag, until the last one: SECRET_KEY=do-not-commit-this"),
        true, "content ends with 'SECRET_KEY=', anchored via rule-prodenv-secret",
    ));
    cases.push(case(
        "blk-c-prodenv-fence", "should-block/content/fence", "Write",
        w(root, "deploy/prod.env", &fenced("Copied from a teammate's local .env by accident:", "", lit, "Remove before merging.")),
        true, "content carries 'SECRET_KEY=' inside a fenced code block, anchored via rule-prodenv-secret",
    ));
    cases.push(case(
        "blk-c-prodenv-longdoc", "should-block/content/longdoc", "Write",
        w(root, "deploy/prod.env", &long_document("# deploy notes for the production environment file", lit, "# end of notes")),
        true, "content buries 'SECRET_KEY=' in the third paragraph of a longer document, anchored via rule-prodenv-secret",
    ));
    cases.push(case(
        "blk-c-prodenv-twice", "should-block/content/twice", "Edit",
        ed(root, "deploy/prod.env", "TBD", &twice("old config used", lit, "first-value, the migration script still references")),
        true, "new_string contains 'SECRET_KEY=' twice, anchored via rule-prodenv-secret",
    ));
    cases.push(case(
        "blk-c-prodenv-adjacent", "should-block/content/adjacent", "Write",
        w(root, "deploy/prod.env", &adjacent_confusable("prefix", lit, "suffix appears mid line with dashes crowding it on both sides.")),
        true, "content presses 'SECRET_KEY=' directly against en dash characters on both sides, anchored via rule-prodenv-secret",
    ));

    // ---- pair 5: server/routes/admin.ts / "console.log(" ----
    let lit = "console.log(";
    cases.push(case(
        "blk-c-admints-start", "should-block/content/start", "Write",
        w(root, "server/routes/admin.ts", "console.log(\"admin route hit\") is the first statement in this handler, left over from debugging yesterday."),
        true, "content opens with 'console.log(', anchored via rule-admints-consolelog",
    ));
    cases.push(case(
        "blk-c-admints-middle", "should-block/content/middle", "Write",
        w(root, "server/routes/admin.ts", "export function handler(req, res) { console.log(\"debug\", req.body); return res.status(200).send(); }"),
        true, "content carries 'console.log(' mid-line, anchored via rule-admints-consolelog",
    ));
    cases.push(case(
        "blk-c-admints-end", "should-block/content/end", "Write",
        w(root, "server/routes/admin.ts", "The handler validates every field carefully and logs nothing sensitive anywhere, except for one trailing call: console.log("),
        true, "content ends with 'console.log(', anchored via rule-admints-consolelog",
    ));
    cases.push(case(
        "blk-c-admints-fence", "should-block/content/fence", "Write",
        w(root, "server/routes/admin.ts", &fenced("Left in from a debugging session:", "", &format!("{lit}user);"), "Should have been removed before commit.")),
        true, "content carries 'console.log(' inside a fenced code block, anchored via rule-admints-consolelog",
    ));
    cases.push(case(
        "blk-c-admints-longdoc", "should-block/content/longdoc", "Write",
        w(root, "server/routes/admin.ts", &long_document("// review thread for the admin routes handler", lit, "// end of review thread")),
        true, "content buries 'console.log(' in the third paragraph of a longer document, anchored via rule-admints-consolelog",
    ));
    cases.push(case(
        "blk-c-admints-twice", "should-block/content/twice", "Write",
        w(root, "server/routes/admin.ts", &twice("console.log(\"start\"); doWork();", lit, "\"end\"); // left in from a debugging session, second call was")),
        true, "content contains 'console.log(' twice, anchored via rule-admints-consolelog",
    ));
    cases.push(case(
        "blk-c-admints-adjacent", "should-block/content/adjacent", "Edit",
        ed(root, "server/routes/admin.ts", "TBD", &adjacent_confusable("trace", lit, "marker sits right where a teammate pasted it mid-refactor.")),
        true, "new_string presses 'console.log(' directly against en dash characters on both sides, anchored via rule-admints-consolelog",
    ));

    // ---- pair 6: infra/terraform/main.tf / "0.0.0.0/0" ----
    let lit = "0.0.0.0/0";
    cases.push(case(
        "blk-c-terraform-start", "should-block/content/start", "Write",
        w(root, "infra/terraform/main.tf", "0.0.0.0/0 is the very first cidr_block listed in this ingress rule, which is far too permissive."),
        true, "content opens with '0.0.0.0/0', anchored via rule-terraform-cidr",
    ));
    cases.push(case(
        "blk-c-terraform-middle", "should-block/content/middle", "Write",
        w(root, "infra/terraform/main.tf", "resource \"aws_security_group_rule\" \"ingress\" { cidr_blocks = [\"0.0.0.0/0\"] from_port = 22 }"),
        true, "content carries '0.0.0.0/0' mid-line, anchored via rule-terraform-cidr",
    ));
    cases.push(case(
        "blk-c-terraform-end", "should-block/content/end", "Write",
        w(root, "infra/terraform/main.tf", "This module locks down every port carefully according to policy, apart from the range on the last line: 0.0.0.0/0"),
        true, "content ends with '0.0.0.0/0', anchored via rule-terraform-cidr",
    ));
    cases.push(case(
        "blk-c-terraform-fence", "should-block/content/fence", "Write",
        w(root, "infra/terraform/main.tf", &fenced("Pasted from a tutorial without adjusting it:", "cidr_blocks = [\"", &format!("{lit}\"]"), "Needs to be scoped down before merging.")),
        true, "content carries '0.0.0.0/0' inside a fenced code block, anchored via rule-terraform-cidr",
    ));
    cases.push(case(
        "blk-c-terraform-longdoc", "should-block/content/longdoc", "Write",
        w(root, "infra/terraform/main.tf", &long_document("# infra design notes for the network module", lit, "# end of design notes")),
        true, "content buries '0.0.0.0/0' in the third paragraph of a longer document, anchored via rule-terraform-cidr",
    ));
    cases.push(case(
        "blk-c-terraform-twice", "should-block/content/twice", "Write",
        w(root, "infra/terraform/main.tf", &twice("ingress { cidr_blocks = [\"0.0.0.0/0\"] } egress { cidr_blocks = [\"", lit, "\"] } # both directions use")),
        true, "content contains '0.0.0.0/0' twice, anchored via rule-terraform-cidr",
    ));
    cases.push(case(
        "blk-c-terraform-adjacent", "should-block/content/adjacent", "Write",
        w(root, "infra/terraform/main.tf", &adjacent_confusable("cidr", lit, "suffix glues two different-looking ranges together.")),
        true, "content presses '0.0.0.0/0' directly against en dash characters on both sides, anchored via rule-terraform-cidr",
    ));
}

fn em_dash_str() -> String {
    EM_DASH.to_string()
}

// ============================================================================
// SHOULD-BLOCK: location. Eight realistic protected locations (five
// directories, three exact files), each tried directly inside and/or deep
// inside. 13 cases.
// ============================================================================

fn push_should_block_location(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "blk-l-frozen-direct", "should-block/location/direct-child", "Write",
        w(root, "frozen/cli.rs", "fn main() {}"),
        true, "write directly inside the protected directory 'frozen', anchored via rule-loc-frozen",
    ));
    cases.push(case(
        "blk-l-frozen-deep", "should-block/location/deep-child", "Write",
        w(root, "frozen/sub/deep/module.py", "def handler(): pass"),
        true, "write three levels deep inside the protected directory 'frozen', anchored via rule-loc-frozen",
    ));
    cases.push(case(
        "blk-l-vendorlocked-direct", "should-block/location/direct-child", "Write",
        w(root, "vendor/locked/pkg.json", "{}"),
        true, "write directly inside the protected directory 'vendor/locked', anchored via rule-loc-vendor-locked",
    ));
    cases.push(case(
        "blk-l-vendorlocked-deep", "should-block/location/deep-child", "Edit",
        ed(root, "vendor/locked/a/b/c/d/deep.txt", "old", "new"),
        true, "write four levels deep inside the protected directory 'vendor/locked', anchored via rule-loc-vendor-locked",
    ));
    cases.push(case(
        "blk-l-legacyarchive-direct", "should-block/location/direct-child", "Write",
        w(root, "legacy/archive/old.py", "print('legacy')"),
        true, "write directly inside the protected directory 'legacy/archive', anchored via rule-loc-legacy-archive",
    ));
    cases.push(case(
        "blk-l-legacyarchive-deep", "should-block/location/deep-child", "Write",
        w(root, "legacy/archive/2019/q1/report.csv", "col1,col2\n1,2\n"),
        true, "write two levels deep inside the protected directory 'legacy/archive', anchored via rule-loc-legacy-archive",
    ));
    cases.push(case(
        "blk-l-secretsenv-exact", "should-block/location/exact-file", "Write",
        w(root, "secrets.env", "NEW_SECRET=1\n"),
        true, "write to the exact protected file 'secrets.env', anchored via rule-loc-secrets-env",
    ));
    cases.push(case(
        "blk-l-configprodkey-exact", "should-block/location/exact-file", "Edit",
        ed(root, "config/prod.key", "old-key-material", "new-key-material"),
        true, "write to the exact protected file 'config/prod.key', anchored via rule-loc-config-prodkey",
    ));
    cases.push(case(
        "blk-l-terraformstate-exact", "should-block/location/exact-file", "Write",
        w(root, "infra/terraform/state.tfstate", "{\"version\": 4}"),
        true, "write to the exact protected file 'infra/terraform/state.tfstate', anchored via rule-loc-terraform-state",
    ));
    cases.push(case(
        "blk-l-nodemodules-direct", "should-block/location/direct-child", "Write",
        w(root, "node_modules/left-pad/index.js", "module.exports = function() {};"),
        true, "write directly inside the protected directory 'node_modules', anchored via rule-loc-node-modules",
    ));
    cases.push(case(
        "blk-l-nodemodules-deep", "should-block/location/deep-child", "Write",
        w(root, "node_modules/.cache/babel/tmp/file.js", "// cache artifact"),
        true, "write deep inside the protected directory 'node_modules', anchored via rule-loc-node-modules",
    ));
    cases.push(case(
        "blk-l-dotgit-direct", "should-block/location/direct-child", "Write",
        w(root, ".git/config", "[core]\nrepositoryformatversion = 0\n"),
        true, "write directly inside the protected directory '.git', anchored via rule-loc-dotgit",
    ));
    cases.push(case(
        "blk-l-dotgit-deep", "should-block/location/deep-child", "Write",
        w(root, ".git/objects/pack/pack-abc123.idx", "binary-ish content"),
        true, "write deep inside the protected directory '.git', anchored via rule-loc-dotgit",
    ));
}

// ============================================================================
// SHOULD-PASS categories. Each function below is named after the exact
// near-miss it exercises; several are pulled verbatim from requirement 3's
// own list, a few more (marked BONUS) are pulled from absent_guard.rs's own
// unit tests for extra rigor beyond the minimum.
// ============================================================================

/// REQUIRED near miss 1: a similar-but-different character where the real
/// literal is forbidden - a plain hyphen where an em dash is forbidden, an
/// en dash where an em dash is forbidden, a different case, a substring that
/// is not the full word. 6 cases.
fn push_should_pass_near_miss_character(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-char-hyphen-start", "should-pass/near-miss/character", "Write",
        w(root, "docs/POLICY.md", "- this line starts with a plain hyphen, never the special character the rule forbids."),
        false, "a plain ASCII hyphen at the start, never the em dash rule-policy-emdash forbids",
    ));
    cases.push(case(
        "pss-char-hyphen-middle", "should-pass/near-miss/character", "Write",
        w(root, "docs/POLICY.md", "the range spans 10 - 20 pages, using an ordinary hyphen the whole way through this note."),
        false, "a plain ASCII hyphen mid-sentence, never the em dash rule-policy-emdash forbids",
    ));
    cases.push(case(
        "pss-char-hyphen-end", "should-pass/near-miss/character", "Write",
        w(root, "docs/POLICY.md", "everything about this note is unremarkable except the trailing character -"),
        false, "a plain ASCII hyphen at the end, never the em dash rule-policy-emdash forbids",
    ));
    cases.push(case(
        "pss-char-endash", "should-pass/near-miss/character", "Write",
        w(root, "docs/POLICY.md", &format!("the schedule reads 09:00{}17:00 daily, printed with an en dash rather than anything else.", EN_DASH)),
        false, "an en dash, a genuinely different character from the em dash rule-policy-emdash forbids",
    ));
    cases.push(case(
        "pss-char-case", "should-pass/near-miss/character", "Write",
        w(root, "NOTES.md", "Forbidden territory ahead, but written with a capital letter this time around in the opening word."),
        false, "capitalised 'Forbidden' does not contain the exact lowercase literal rule-notes-forbidden checks for",
    ));
    cases.push(case(
        "pss-char-substring", "should-pass/near-miss/character", "Write",
        w(root, "NOTES.md", "we should forbid this pattern going forward, though the full word never actually appears here at all."),
        false, "'forbid' is present but the full literal 'forbidden' rule-notes-forbidden checks for never appears",
    ));
}

/// REQUIRED near miss 2: the forbidden literal, but in a DIFFERENT,
/// unrelated file (a different basename, not just a different directory, so
/// this cannot accidentally match by last-path-segment either). 6 cases.
fn push_should_pass_different_file(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-diff-file-notes", "should-pass/near-miss/different-file", "Write",
        w(root, "journal/OTHER-LOG.md", "forbidden shows up here too, but this file is not the one rule-notes-forbidden is anchored to."),
        false, "'forbidden' appears in journal/OTHER-LOG.md, an unrelated file rule-notes-forbidden never anchors to",
    ));
    cases.push(case(
        "pss-diff-file-policy", "should-pass/near-miss/different-file", "Write",
        w(root, "readme/INTRO.md", &format!("this introduction carries a {} too, but readme/INTRO.md is not docs/POLICY.md.", EM_DASH)),
        false, "an em dash appears in readme/INTRO.md, an unrelated file rule-policy-emdash never anchors to",
    ));
    cases.push(case(
        "pss-diff-file-config", "should-pass/near-miss/different-file", "Write",
        w(root, "src/helpers.rs", "fn helper() { let x = maybe().unwrap(); x }"),
        false, "'unwrap()' appears in src/helpers.rs, an unrelated file rule-config-unwrap never anchors to",
    ));
    cases.push(case(
        "pss-diff-file-prodenv", "should-pass/near-miss/different-file", "Write",
        w(root, "deploy/staging.env", "SECRET_KEY=staging-only-value\n"),
        false, "'SECRET_KEY=' appears in deploy/staging.env, an unrelated file rule-prodenv-secret never anchors to",
    ));
    cases.push(case(
        "pss-diff-file-admints", "should-pass/near-miss/different-file", "Write",
        w(root, "server/routes/public.ts", "export function handler() { console.log(\"public route\"); }"),
        false, "'console.log(' appears in server/routes/public.ts, an unrelated file rule-admints-consolelog never anchors to",
    ));
    cases.push(case(
        "pss-diff-file-terraform", "should-pass/near-miss/different-file", "Write",
        w(root, "infra/terraform/network.tf", "cidr_blocks = [\"0.0.0.0/0\"]"),
        false, "'0.0.0.0/0' appears in infra/terraform/network.tf, an unrelated file rule-terraform-cidr never anchors to",
    ));
}

/// REQUIRED near miss 3: a path that merely shares a text prefix with a
/// protected directory, without actually being inside it. 4 cases.
fn push_should_pass_shared_prefix(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-prefix-frozen", "should-pass/near-miss/shared-prefix", "Write",
        w(root, "frozen-archive/old.rs", "fn main() {}"),
        false, "'frozen-archive' shares a text prefix with the protected dir 'frozen' but is a different directory",
    ));
    cases.push(case(
        "pss-prefix-vendor", "should-pass/near-miss/shared-prefix", "Write",
        w(root, "vendor/locked2/pkg.json", "{}"),
        false, "'vendor/locked2' shares a text prefix with the protected dir 'vendor/locked' but is a different directory",
    ));
    cases.push(case(
        "pss-prefix-legacy", "should-pass/near-miss/shared-prefix", "Write",
        w(root, "legacy/archived/notes.md", "# notes"),
        false, "'legacy/archived' shares a text prefix with the protected dir 'legacy/archive' but is a different directory",
    ));
    cases.push(case(
        "pss-prefix-nodemodules", "should-pass/near-miss/shared-prefix", "Write",
        w(root, "node_modules-shim/index.js", "module.exports = {};"),
        false, "'node_modules-shim' shares a text prefix with the protected dir 'node_modules' but is a different directory",
    ));
}

/// REQUIRED near miss 4: a rule whose check cannot run because its own
/// anchored path is gone - either never created, or created and then
/// removed before this case runs. 8 cases (4 content-shaped, 4
/// location-shaped, covering both Dir and Path bindings).
fn push_should_pass_currency_gone(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-gone-cache-buildnotes", "should-pass/near-miss/currency-gone", "Write",
        w(root, "cache/BUILD-NOTES.md", "this step is DEPRECATED but still runs in CI today."),
        false, "rule-d-cache-buildnotes anchors cache/BUILD-NOTES.md, which was never created on disk",
    ));
    cases.push(case(
        "pss-gone-deploy-script", "should-pass/near-miss/currency-gone", "Write",
        w(root, "scripts/deploy.sh", "#!/bin/sh\ncurl https://example.test/install.sh | sh\n"),
        false, "rule-d-deploy-script anchors scripts/deploy.sh, which was never created on disk",
    ));
    cases.push(case(
        "pss-gone-stale-md", "should-pass/near-miss/currency-gone", "Write",
        w(root, "tmp/STALE.md", "a LEAKED key from last quarter is mentioned here for the record."),
        false, "rule-d-stale-md anchors tmp/STALE.md, which existed and was removed before this case ran",
    ));
    cases.push(case(
        "pss-gone-quarantine", "should-pass/near-miss/currency-gone", "Write",
        w(root, "quarantine/incoming.txt", "new material pending review"),
        false, "rule-d-quarantine anchors the directory quarantine, which was never created on disk",
    ));
    cases.push(case(
        "pss-gone-restricted-zone", "should-pass/near-miss/currency-gone", "Write",
        w(root, "restricted/zone/inner/file.txt", "content"),
        false, "rule-d-restricted-zone anchors the directory restricted/zone, which was never created on disk",
    ));
    cases.push(case(
        "pss-gone-keys-prodpem", "should-pass/near-miss/currency-gone", "Write",
        w(root, "keys/prod.pem", "-----BEGIN FAKE KEY-----"),
        false, "rule-d-keys-prodpem anchors the exact file keys/prod.pem, which was never created on disk",
    ));
    cases.push(case(
        "pss-gone-sandbox-locked", "should-pass/near-miss/currency-gone", "Write",
        w(root, "sandbox/locked/x.txt", "content"),
        false, "rule-d-sandbox-locked anchors the directory sandbox/locked, which existed and was removed before this case ran",
    ));
    cases.push(case(
        "pss-gone-release-changelock", "should-pass/near-miss/currency-gone", "Edit",
        ed(root, "release/CHANGELOG.lock", "old", "new"),
        false, "rule-d-release-changelock anchors the exact file release/CHANGELOG.lock, which existed and was removed before this case ran",
    ));
}

/// REQUIRED near miss 5: a rule with real prose (and a real, existing
/// anchor) but no machine-runnable check at all. 6 cases (3 content-shaped,
/// 3 location-shaped, the latter even carrying Irreversible severity to
/// prove severity alone is never enough).
fn push_should_pass_prose_only(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-prose-roadmap", "should-pass/near-miss/prose-only", "Write",
        w(root, "ideas/ROADMAP.md", "this draft now plans two full years ahead, well past the next two quarters."),
        false, "rule-e-roadmap anchors a real, existing file but carries no check at all",
    ));
    cases.push(case(
        "pss-prose-invoices", "should-pass/near-miss/prose-only", "Write",
        w(root, "billing/INVOICES.md", "invoice 003 is listed before invoice 001 here, out of order."),
        false, "rule-e-invoices anchors a real, existing file but carries no check at all",
    ));
    cases.push(case(
        "pss-prose-staging", "should-pass/near-miss/prose-only", "Write",
        w(root, "staging/build.rs", "fn main() { println!(\"staging build\"); }"),
        false, "rule-e-staging anchors a real, existing directory with Irreversible severity but no check at all",
    ));
    cases.push(case(
        "pss-prose-archivecold", "should-pass/near-miss/prose-only", "Write",
        w(root, "archive/cold/deep/x.md", "# revisiting an old project"),
        false, "rule-e-archive-cold anchors a real, existing directory with Irreversible severity but no check at all",
    ));
    cases.push(case(
        "pss-prose-importantlock", "should-pass/near-miss/prose-only", "Write",
        w(root, "important.lock", "hand typed content, not generated by the release tool"),
        false, "rule-e-importantlock anchors a real, existing file with Irreversible severity but no check at all",
    ));
    cases.push(case(
        "pss-prose-notesdraft", "should-pass/near-miss/prose-only", "Write",
        w(root, "release/NOTES-DRAFT.md", "this reads like a fully polished announcement already."),
        false, "rule-e-notesdraft anchors a real, existing file but carries no check at all",
    ));
}

/// REQUIRED near miss 6: a rule with a real, current PathExists check for a
/// real directory, but a severity short of Irreversible - the exact thing
/// that turns a description into a prohibition, per location_anchor's own
/// doc comment. 3 cases (Costly, HouseStyle, and no severity at all).
fn push_should_pass_wrong_severity(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-severity-costly", "should-pass/near-miss/wrong-severity", "Write",
        w(root, "beta/preview/x.rs", "fn main() {}"),
        false, "rule-f-beta-preview has a real, current PathExists check but severity Costly, not Irreversible",
    ));
    cases.push(case(
        "pss-severity-housestyle", "should-pass/near-miss/wrong-severity", "Write",
        w(root, "beta/hidden/y.rs", "fn main() {}"),
        false, "rule-f-beta-hidden has a real, current PathExists check but severity HouseStyle, not Irreversible",
    ));
    cases.push(case(
        "pss-severity-none", "should-pass/near-miss/wrong-severity", "Write",
        w(root, "keys/staging.pem", "-----BEGIN FAKE KEY-----"),
        false, "rule-f-keys-staging has a real, current PathExists check but no severity at all, not Irreversible",
    ));
}

/// REQUIRED near miss 7: an ordinary write nowhere near any rule this
/// corpus declared. 10 cases, varying extension, depth and length.
fn push_should_pass_no_rule_nearby(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-norule-1", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "reports/q3-summary.md", "# Q3 summary\n\nRevenue is up, nothing else to flag this quarter."),
        false, "reports/q3-summary.md has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-2", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "src/utils/formatter.ts", "export function formatDate(d: Date): string { return d.toISOString(); }"),
        false, "src/utils/formatter.ts has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-3", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "infra/k8s/deployment.yaml", "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n"),
        false, "infra/k8s/deployment.yaml has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-4", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "docs/guides/onboarding.md", "# Onboarding\n\nWelcome! Start by cloning the repository."),
        false, "docs/guides/onboarding.md has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-5", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "test/fixtures/sample.json", "{\"id\": 1, \"name\": \"sample\"}"),
        false, "test/fixtures/sample.json has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-6", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "assets/notes.txt", "just a plain text note, nothing special about it at all"),
        false, "assets/notes.txt has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-7", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "billing/receipts/2025-01.csv", "date,amount\n2025-01-05,42.00\n"),
        false, "billing/receipts/2025-01.csv has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-8", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "app/models/user.py", "class User:\n    def __init__(self, name):\n        self.name = name\n"),
        false, "app/models/user.py has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-9", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "config/local.toml", "[server]\nport = 3000\n"),
        false, "config/local.toml has no rule anchored to it anywhere in this corpus",
    ));
    cases.push(case(
        "pss-norule-10", "should-pass/near-miss/no-rule-nearby", "Write",
        w(root, "README.md", "# Project\n\nThis is the top-level readme for the whole repository."),
        false, "README.md has no rule anchored to it anywhere in this corpus",
    ));
}

/// REQUIRED near miss 8: a tool this guard has no notion of at all - proves
/// BOTH short-circuit points in `decide` (a payload with no `file_path` at
/// all, and a payload with a `file_path` but the wrong tool name), including
/// against paths that ARE genuinely protected, to prove the tool-name gate
/// overrides even a real match. 8 cases.
fn push_should_pass_unrelated_tool(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-tool-read-notes", "should-pass/near-miss/unrelated-tool", "Read",
        serde_json::json!({ "file_path": abs(root, "NOTES.md") }),
        false, "tool_name 'Read' is not Write/Edit, so proposed_content is None even with a real file_path",
    ));
    cases.push(case(
        "pss-tool-bash-frozen", "should-pass/near-miss/unrelated-tool", "Bash",
        serde_json::json!({ "command": "cat frozen/cli.rs", "description": "inspect a file" }),
        false, "a Bash payload carries no file_path field at all, so decide short-circuits before any guard runs",
    ));
    cases.push(case(
        "pss-tool-read-secrets", "should-pass/near-miss/unrelated-tool", "Read",
        serde_json::json!({ "file_path": abs(root, "secrets.env") }),
        false, "tool_name 'Read' targets the genuinely protected secrets.env, but is not Write/Edit",
    ));
    cases.push(case(
        "pss-tool-grep-frozen", "should-pass/near-miss/unrelated-tool", "Grep",
        serde_json::json!({ "pattern": "TODO", "path": "frozen" }),
        false, "a Grep payload carries no file_path field at all, so decide short-circuits before any guard runs",
    ));
    cases.push(case(
        "pss-tool-notebookedit", "should-pass/near-miss/unrelated-tool", "NotebookEdit",
        serde_json::json!({ "notebook_path": abs(root, "NOTES.md"), "new_source": "forbidden" }),
        false, "a NotebookEdit payload keys its target as notebook_path, not file_path, so decide short-circuits",
    ));
    cases.push(case(
        "pss-tool-multiedit-notes", "should-pass/near-miss/unrelated-tool", "MultiEdit",
        serde_json::json!({ "file_path": abs(root, "NOTES.md"), "edits": [{ "old_string": "x", "new_string": "forbidden" }] }),
        false, "tool_name 'MultiEdit' has a real file_path but is not Write/Edit, so proposed_content is None",
    ));
    cases.push(case(
        "pss-tool-read-frozen-file", "should-pass/near-miss/unrelated-tool", "Read",
        serde_json::json!({ "file_path": abs(root, "frozen/cli.rs") }),
        false, "tool_name 'Read' targets a file genuinely inside the protected dir 'frozen', but is not Write/Edit",
    ));
    cases.push(case(
        "pss-tool-bash-rm", "should-pass/near-miss/unrelated-tool", "Bash",
        serde_json::json!({ "command": "rm -rf frozen", "description": "clean up" }),
        false, "a Bash payload carries no file_path field at all, even one whose command targets a protected dir",
    ));
}

/// BONUS (true negative, not a required near-miss category): genuinely
/// clean content written to a real, currently-anchored content-rule file -
/// proves the guard does not block indiscriminately on the anchored file
/// alone. 5 cases.
fn push_should_pass_clean_content(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-clean-notes", "should-pass/bonus/clean-content", "Write",
        w(root, "NOTES.md", "an entirely ordinary line about today's sync, nothing flagged, nothing unusual to report."),
        false, "clean content with no forbidden literal, written to the genuinely anchored NOTES.md",
    ));
    cases.push(case(
        "pss-clean-policy", "should-pass/bonus/clean-content", "Write",
        w(root, "docs/POLICY.md", "this paragraph uses only plain punctuation throughout, nothing exotic at all."),
        false, "clean content with no em dash, written to the genuinely anchored docs/POLICY.md",
    ));
    cases.push(case(
        "pss-clean-config", "should-pass/bonus/clean-content", "Write",
        w(root, "src/config.rs", "fn load() -> Result<Config, Error> { let raw = std::fs::read_to_string(PATH)?; toml::from_str(&raw).map_err(Error::from) }"),
        false, "clean content using proper ? error handling, no unwrap(), written to the genuinely anchored src/config.rs",
    ));
    cases.push(case(
        "pss-clean-prodenv", "should-pass/bonus/clean-content", "Write",
        w(root, "deploy/prod.env", "DEBUG=false\nPORT=8080\nLOG_LEVEL=info\n"),
        false, "clean content with no SECRET_KEY= assignment, written to the genuinely anchored deploy/prod.env",
    ));
    cases.push(case(
        "pss-clean-admints", "should-pass/bonus/clean-content", "Write",
        w(root, "server/routes/admin.ts", "export function handler(req, res) { return res.status(200).send(); }"),
        false, "clean content with no console.log(, written to the genuinely anchored server/routes/admin.ts",
    ));
}

/// BONUS: an Edit whose old_string carries the forbidden literal but whose
/// new_string does not - proves the guard reads the replacement half, never
/// the removed half. 3 cases.
fn push_should_pass_edit_old_string_only(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-editold-notes", "should-pass/bonus/edit-old-string-only", "Edit",
        ed(root, "NOTES.md", "this used to say forbidden right here", "this now says something else entirely"),
        false, "old_string carries 'forbidden' but new_string does not, and the guard only ever reads new_string",
    ));
    cases.push(case(
        "pss-editold-config", "should-pass/bonus/edit-old-string-only", "Edit",
        ed(root, "src/config.rs", "let raw = std::fs::read_to_string(PATH).unwrap();", "let raw = std::fs::read_to_string(PATH)?;"),
        false, "old_string carries 'unwrap()' but new_string does not, and the guard only ever reads new_string",
    ));
    cases.push(case(
        "pss-editold-prodenv", "should-pass/bonus/edit-old-string-only", "Edit",
        ed(root, "deploy/prod.env", "SECRET_KEY=old-leaked-value", "DEBUG=false"),
        false, "old_string carries 'SECRET_KEY=' but new_string does not, and the guard only ever reads new_string",
    ));
}

/// BONUS: a Write payload missing the `content` field entirely - proves
/// `proposed_content` reads this as None, never as empty-string content,
/// AND that the whole guard (location half included) is gated behind it.
/// 2 cases: one against a content anchor, one against a location anchor.
fn push_should_pass_malformed_payload(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-malformed-write-notes", "should-pass/bonus/malformed-payload", "Write",
        w_no_content(root, "NOTES.md"),
        false, "a Write payload with no 'content' field at all, targeting the genuinely anchored NOTES.md",
    ));
    cases.push(case(
        "pss-malformed-write-frozen", "should-pass/bonus/malformed-payload", "Write",
        w_no_content(root, "frozen/cli.rs"),
        false, "a Write payload with no 'content' field at all, targeting a file genuinely inside the protected dir 'frozen' - proves the location half is gated behind content resolution too",
    ));
}

/// BONUS: a check of the wrong FORM for the content guard (Contains or
/// PathExists instead of Absent) - only Check::Absent may ever cause a
/// content block, per first_violation's own filter. 2 cases.
fn push_should_pass_check_form_mismatch(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-checkform-contains", "should-pass/bonus/check-form-mismatch", "Write",
        w(root, "web/index.html", "<html><head></head><body>no TRACKING_ID snippet left in this page anymore</body></html>"),
        false, "rule-x-web-index carries a Check::Contains, never a Check::Absent, so it can never cause a content block",
    ));
    cases.push(case(
        "pss-checkform-pathexists", "should-pass/bonus/check-form-mismatch", "Write",
        w(root, "web/footer.html", "<footer>completely different footer links than every other page uses</footer>"),
        false, "rule-x-web-footer carries a Check::PathExists, never a Check::Absent, so it can never cause a content block",
    ));
}

/// BONUS: a location rule whose OWN check path names a different location
/// than its own binding - location_anchor requires the two to match
/// exactly, so a mismatch must never block even though both paths are
/// genuinely current on disk. 2 cases.
fn push_should_pass_anchor_mismatch(cases: &mut Vec<Case>, root: &Path) {
    cases.push(case(
        "pss-anchormismatch-ops", "should-pass/bonus/anchor-mismatch", "Write",
        w(root, "ops/keys/secret.pem", "-----BEGIN FAKE KEY-----"),
        false, "rule-y-ops-keys binds to ops/keys but its Check::PathExists names ops/keys-real instead, so location_anchor refuses it",
    ));
    cases.push(case(
        "pss-anchormismatch-vault", "should-pass/bonus/anchor-mismatch", "Write",
        w(root, "shared/vault/token.txt", "fake-token-value"),
        false, "rule-y-shared-vault binds to shared/vault but its Check::PathExists names shared/vault-legacy instead, so location_anchor refuses it",
    ));
}
