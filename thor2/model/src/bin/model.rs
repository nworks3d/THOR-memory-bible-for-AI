//! Minimal CLI over the model crate: `declare` writes an item and refuses
//! loudly (stderr + non-zero exit) when the write gate rejects it; `show`
//! prints the current item stored under an id. Nothing more.

use clap::{Parser, Subcommand};
use model::item::{Binding, Item, Kind, Severity, TargetKind};
use model::{gate, store};
use std::str::FromStr;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(name = "model", about = "Declare and show typed memory items (Rule/Orientation/Report/Lookup)")]
struct Cli {
    /// Path to the core event-log database.
    #[arg(long)]
    db: std::path::PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a new item. Refuses loudly if the write gate rejects it: the
    /// reason and what to do instead go to stderr, exit code 1, nothing is
    /// written to the log.
    Declare {
        #[arg(long)]
        id: String,
        #[arg(long)]
        kind: Kind,
        #[arg(long)]
        text: String,
        #[arg(long)]
        severity: Option<Severity>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// ISO-8601 date. Only meaningful (and only accepted) for a Report.
        #[arg(long)]
        expires: Option<String>,
        /// Required for a Lookup.
        #[arg(long)]
        key: Option<String>,
        /// What observation would prove this fact wrong. Required for a
        /// Rule or an Orientation (see `gate::declare`); optional for the
        /// other three kinds.
        #[arg(long)]
        falsifier: Option<String>,
        /// One of: path_exists, contains, absent, absent_all, forbidden. A
        /// machine-runnable check this item can prove itself against,
        /// alongside (never instead of) --falsifier - see model::check::run.
        /// Only a Rule/Orientation may carry one. This is the ONLY kind of
        /// rule this memory ever uses to block a write outright, and only
        /// while the check currently HOLDS (see serve's write guard); a rule
        /// backed by prose alone can inform, but can never block. Every kind
        /// except forbidden requires --check-path; path_exists refuses
        /// --check-literal/--check-literals; contains/absent require
        /// --check-literal; absent_all and forbidden require
        /// --check-literals (repeatable) instead. Prefer forbidden when the
        /// rule forbids something self-contained, with nothing to anchor it
        /// to (a banned character or word, wherever written); prefer
        /// absent_all when the rule is really about one file that could
        /// move, or content that could change.
        #[arg(long)]
        check_kind: Option<String>,
        /// The exact file this check inspects, relative to the root the
        /// checker is run against. Required together with --check-kind, for
        /// every check_kind except forbidden - forbidden carries no path at
        /// all and refuses one if given.
        #[arg(long)]
        check_path: Option<String>,
        /// The exact literal a "contains" or "absent" --check-kind looks
        /// for. Refused together with "path_exists" or --check-literals;
        /// required with "contains"/"absent".
        #[arg(long)]
        check_literal: Option<String>,
        /// A literal to forbid, for --check-kind absent_all or forbidden -
        /// repeat this flag once per literal to forbid a whole SET of them
        /// as ONE rule (e.g. every typography character a house style
        /// bans), rather than declaring one near-duplicate item per literal.
        /// A separate repeatable flag rather than a delimited single string
        /// on purpose: these literals are themselves punctuation, so any
        /// delimiter chosen could collide with one of them. Refused together
        /// with --check-literal - use exactly one of the two, never both.
        #[arg(long)]
        check_literals: Vec<String>,
        /// Bind this item to an action (repeatable): one of the known,
        /// closed action names (see `intent::Action`).
        #[arg(long = "moment", value_parser = parse_moment_arg)]
        moments: Vec<Binding>,
        /// Bind this item to a target, given as `<kind>:<value>` (repeatable),
        /// kind one of path/dir/symbol/command/project.
        #[arg(long = "target", value_parser = parse_target_arg)]
        targets: Vec<Binding>,
        /// Bind this item to the pinned Always layer.
        #[arg(long)]
        always: bool,
        #[arg(long, default_value = "cli")]
        session_id: String,
        #[arg(long, default_value = "cli")]
        lineage_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
    },
    /// Show the current item stored under an id.
    Show {
        #[arg(long)]
        id: String,
    },
    /// Turn a live item into archive material: same id, text and history,
    /// still found by lookup, but it stops claiming to fire. For a fact whose
    /// SUBJECT is gone (a frozen tree, a deleted file) - not for one that is
    /// wrong, which is `retract`'s job. Refuses an item that carries a
    /// runnable check (that is the only kind that can still block a wrong
    /// write) and one already archived. See `store::archive`.
    ///
    /// The one runnable entry point to `store::archive`, which until
    /// 2026-08-14 existed only in tests - so the one session that needed it
    /// had to read the raw SQLite to do by hand what this now does.
    Archive {
        #[arg(long)]
        id: String,
        /// Why it can no longer fire. Becomes an `archived:<reason>` tag, so
        /// under 120 characters, one plain phrase, no comma or line break.
        #[arg(long)]
        reason: String,
        #[arg(long, default_value = "cli")]
        session_id: String,
        #[arg(long, default_value = "cli")]
        lineage_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
    },
}

fn parse_moment_arg(s: &str) -> Result<Binding, String> {
    gate::parse_moment(s).map_err(|e| e.to_string())
}

fn parse_target_arg(s: &str) -> Result<Binding, String> {
    let (kind_str, value) = s
        .split_once(':')
        .ok_or_else(|| format!("expected <kind>:<value> (e.g. path:src/main.rs), got '{s}'"))?;
    let kind = TargetKind::from_str(kind_str).map_err(|e| e.to_string())?;
    Ok(Binding::Target { kind, value: value.to_string() })
}

// The four flat `--check-kind`/`--check-path`/`--check-literal`/
// `--check-literals` flags are turned into `model::item::Check` by
// `model::gate::build_check` - the one shared home this CLI and
// `mcp/src/lib.rs`'s tools both call, right beside `check_check` and
// `parse_moment` there (see `parse_moment_arg` above, which already calls
// `gate::parse_moment` rather than re-deciding the action vocabulary here).
// It used to be a byte-for-byte duplicated copy in each of the two binaries
// (gate.rs was locked at the time); now that it is not, both call sites were
// moved onto the one real function instead - `--check-literals` (added for
// the `absent_all` set form) went through that same one function too, never
// a second copy of its argument-shape rules here.

/// The checkout this command was run in, for the write gate's neighbourhood
/// toll (`store::declare_in`), or `None` outside one.
///
/// Resolved by walking up for a `.git`, rather than by asking `serve`: this
/// crate sits UNDER serve in the workspace, and inverting that dependency to
/// share eight lines would be the tail wagging the dog. `None` simply means
/// no toll, the same fail-open rule every check here follows when it has no
/// anchor to resolve.
fn checkout_root() -> Option<std::path::PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn open_store(path: &std::path::Path) -> EventStore {
    match EventStore::new(path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("could not open store at {}: {e}", path.display());
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Declare {
            id,
            kind,
            text,
            severity,
            project,
            tags,
            expires,
            key,
            falsifier,
            check_kind,
            check_path,
            check_literal,
            check_literals,
            moments,
            targets,
            always,
            session_id,
            lineage_id,
            actor,
        } => {
            let mut bindings = moments;
            bindings.extend(targets);
            if always {
                bindings.push(Binding::Always);
            }
            let check = match gate::build_check(check_kind.as_deref(), check_path, check_literal, check_literals) {
                Ok(check) => check,
                Err(e) => {
                    eprintln!("refused: invalid check: {e}");
                    std::process::exit(1);
                }
            };
            let item = Item { id, kind, text, bindings, severity, project, tags, expires, key, falsifier, check };

            let mut event_store = open_store(&cli.db);
            // The same neighbourhood toll the agent's own write pays. Wiring
            // it only into the tool surface would have left this command as
            // the way around it, and a gate with a documented detour is not
            // a gate - it is a habit with extra steps.
            match store::declare_in(&mut event_store, &session_id, &lineage_id, &actor, &item, checkout_root().as_deref()) {
                Ok(event) => println!("stored {} (seq {})", item.id, event.seq),
                Err(e) => {
                    eprintln!("refused: {e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Show { id } => {
            let event_store = open_store(&cli.db);
            match store::show(&event_store, &id) {
                Ok(item) => match serde_json::to_string_pretty(&item) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        eprintln!("could not render item: {e}");
                        std::process::exit(1);
                    }
                },
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Archive { id, reason, session_id, lineage_id, actor } => {
            let mut event_store = open_store(&cli.db);
            // The checkout this ran in, so a check pointing at a file that is
            // gone no longer blocks the archive - see `store::archive`.
            let root = checkout_root();
            match store::archive(
                &mut event_store,
                &session_id,
                &lineage_id,
                &actor,
                &id,
                &reason,
                root.as_deref(),
            ) {
                Ok(event) => println!("archived {id} (seq {})", event.seq),
                Err(e) => {
                    eprintln!("refused: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::Check;

    // `build_check`'s own pure-logic test set (every accepted/refused
    // argument combination) now lives once, canonically, in `model/src/
    // gate.rs`'s own `tests` module, beside the function itself - this file
    // used to carry a byte-for-byte duplicate of that same test set (see
    // this task's own report). What stays here is what only this CLI can
    // prove: the clap flag wiring below, and the end-to-end store round trip
    // further down.

    // ------------------------------------------------------ clap wiring

    /// Proves the three `--check-*` flags actually reach `build_check`'s
    /// parameters under the names the doc comments promise - clap's default
    /// kebab-case derivation is what turns `check_kind` into `--check-kind`,
    /// never spelled out by hand, so this is what would catch a typo'd
    /// `#[arg(long = "...")]` override or a field rename.
    #[test]
    fn declare_check_flags_parse_into_the_expected_fields() {
        let cli = Cli::try_parse_from([
            "model",
            "--db",
            "x.db",
            "declare",
            "--id",
            "i1",
            "--kind",
            "rule",
            "--text",
            "t",
            "--always",
            "--check-kind",
            "contains",
            "--check-path",
            "README.md",
            "--check-literal",
            "GPLv3",
        ])
        .expect("the check flags must parse");
        match cli.command {
            Command::Declare { check_kind, check_path, check_literal, .. } => {
                assert_eq!(check_kind, Some("contains".to_string()));
                assert_eq!(check_path, Some("README.md".to_string()));
                assert_eq!(check_literal, Some("GPLv3".to_string()));
            }
            _ => panic!("expected Declare"),
        }
    }

    #[test]
    fn declare_without_any_check_flag_leaves_every_check_field_empty() {
        let cli = Cli::try_parse_from([
            "model", "--db", "x.db", "declare", "--id", "i1", "--kind", "rule", "--text", "t", "--always",
        ])
        .expect("declare with no check flags must still parse");
        match cli.command {
            Command::Declare { check_kind, check_path, check_literal, check_literals, .. } => {
                assert_eq!(check_kind, None);
                assert_eq!(check_path, None);
                assert_eq!(check_literal, None);
                assert!(check_literals.is_empty());
            }
            _ => panic!("expected Declare"),
        }
    }

    /// THE DEFECT THIS PREVENTS: `--check-literals` is a repeatable flag
    /// (see `Check::AbsentAll` and `gate::build_check`'s own doc comment for
    /// why a repeatable flag was chosen over a delimited single string) - if
    /// it were accidentally wired with `value_delimiter` instead of plain
    /// repetition, a literal that happens to contain the delimiter character
    /// would silently split into two entries. Proven here the same way
    /// `declare_check_flags_parse_into_the_expected_fields` proves the
    /// singular flags: parse real argv, inspect the resulting Vec.
    #[test]
    fn declare_check_literals_flag_parses_one_entry_per_repetition() {
        let cli = Cli::try_parse_from([
            "model",
            "--db",
            "x.db",
            "declare",
            "--id",
            "i1",
            "--kind",
            "rule",
            "--text",
            "t",
            "--always",
            "--check-kind",
            "absent_all",
            "--check-path",
            "docs/STYLE.md",
            "--check-literals",
            "\u{2014}",
            "--check-literals",
            "\u{2013}",
            "--check-literals",
            "...",
        ])
        .expect("repeated --check-literals must parse");
        match cli.command {
            Command::Declare { check_literals, .. } => {
                assert_eq!(check_literals, vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "...".to_string()]);
            }
            _ => panic!("expected Declare"),
        }
    }

    /// The Archive subcommand's own flag wiring - the one thing only this CLI
    /// can prove, mirroring the declare wiring tests above. `store::archive`'s
    /// behaviour is proven canonically beside the function in store.rs; what
    /// stays here is that `--id`/`--reason` reach the parameters clap's
    /// kebab-case derivation is expected to name.
    #[test]
    fn archive_flags_parse_into_the_expected_fields() {
        let cli = Cli::try_parse_from([
            "model", "--db", "x.db", "archive", "--id", "i1", "--reason", "the tree it describes is frozen",
        ])
        .expect("the archive flags must parse");
        match cli.command {
            Command::Archive { id, reason, .. } => {
                assert_eq!(id, "i1");
                assert_eq!(reason, "the tree it describes is frozen");
            }
            _ => panic!("expected Archive"),
        }
    }

    // -------------------------------------------------- end-to-end (store)
    //
    // Exercises the SAME item-construction path `main`'s `Command::Declare`
    // arm uses (bindings, then build_check, then the Item literal), against
    // a real in-memory store - proving the CLI's new flags actually round-
    // trip through model::store::declare/show, not just that build_check
    // returns the right Rust value in isolation.

    fn declare_item_with_check(id: &str, text: &str, check: Option<Check>) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: text.to_string(),
            bindings: vec![Binding::Always],
            severity: Some(Severity::HouseStyle),
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("{id} turns out not to matter")),
            check,
        }
    }

    #[test]
    fn declare_with_an_accepted_check_combination_round_trips_into_the_store() {
        let mut db = thor_core::event_store::EventStore::in_memory().unwrap();
        let check = gate::build_check(Some("path_exists"), Some("CHANGELOG.md".to_string()), None, vec![]).unwrap();
        let item = declare_item_with_check("cli-check-1", "the changelog lives in CHANGELOG.md", check);

        store::declare(&mut db, "s", "l", "test", &item).unwrap();
        let back = store::show(&db, "cli-check-1").unwrap();
        assert_eq!(back.check, Some(Check::PathExists { path: "CHANGELOG.md".to_string() }));
    }

    #[test]
    fn declare_with_a_refused_check_combination_writes_nothing() {
        let db = thor_core::event_store::EventStore::in_memory().unwrap();
        // path_exists + a literal: refused by gate::build_check before an
        // Item is even constructed - main() exits 1 before ever calling
        // open_store, let alone store::declare. Reproduced here at the
        // build_check layer (nothing to write yet) plus a direct proof the
        // store stays empty.
        let err =
            gate::build_check(Some("path_exists"), Some("README.md".to_string()), Some("GPLv3".to_string()), vec![]);
        assert!(err.is_err());
        assert!(db.get_all_events().unwrap().is_empty(), "a refused check must leave the store untouched");
    }

    // ------------------------------------------------- absent_all (the CLI)
    //
    // THE DEFECT THIS PREVENTS: `Check::AbsentAll` existed on the model, and
    // `gate::build_check` could construct one, but nothing on this CLI's own
    // `--check-literals` flag had ever been proven to reach a real stored
    // item through the same path `main`'s `Command::Declare` arm takes - see
    // this task's own report. Mirrors `declare_with_an_accepted_check_
    // combination_round_trips_into_the_store` above, exercising the SAME
    // item-construction path (bindings, then build_check, then the Item
    // literal) with a repeated --check-literals-shaped Vec this time.

    #[test]
    fn declare_with_an_absent_all_check_round_trips_into_the_store() {
        let mut db = thor_core::event_store::EventStore::in_memory().unwrap();
        let literals = vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "...".to_string()];
        let check =
            gate::build_check(Some("absent_all"), Some("docs/STYLE.md".to_string()), None, literals.clone())
                .unwrap();
        let item = declare_item_with_check(
            "cli-check-absent-all-1",
            "docs/STYLE.md never carries an em dash, en dash or ellipsis",
            check,
        );

        store::declare(&mut db, "s", "l", "test", &item).unwrap();
        let back = store::show(&db, "cli-check-absent-all-1").unwrap();
        assert_eq!(back.check, Some(Check::AbsentAll { path: "docs/STYLE.md".to_string(), literals }));
    }

    #[test]
    fn declare_with_an_empty_absent_all_literal_list_writes_nothing() {
        // The empty-list refusal from gate::build_check itself (never
        // reaching store::declare at all) - mirrors declare_with_a_refused_
        // check_combination_writes_nothing above for the new set form.
        let db = thor_core::event_store::EventStore::in_memory().unwrap();
        let err = gate::build_check(Some("absent_all"), Some("docs/STYLE.md".to_string()), None, vec![]);
        assert!(err.is_err());
        assert!(db.get_all_events().unwrap().is_empty(), "a refused check must leave the store untouched");
    }

    #[test]
    fn declare_with_both_check_literal_and_check_literals_writes_nothing() {
        // The other refusal ground point 3 of this task adds: giving both
        // the single and the set form at once is refused before an Item is
        // ever built.
        let db = thor_core::event_store::EventStore::in_memory().unwrap();
        let err = gate::build_check(
            Some("absent_all"),
            Some("docs/STYLE.md".to_string()),
            Some("TODO".to_string()),
            vec!["FIXME".to_string()],
        );
        assert!(err.is_err());
        assert!(db.get_all_events().unwrap().is_empty(), "a refused check must leave the store untouched");
    }

    // -------------------------------------------------- forbidden (the CLI)
    //
    // THE DEFECT THIS PREVENTS: `Check::Forbidden` existed on the model, and
    // `gate::build_check` could construct one, but nothing on this CLI's own
    // flags had ever been proven to reach a real stored item through the
    // SAME path `main`'s `Command::Declare` arm takes - mirrors
    // `declare_with_an_absent_all_check_round_trips_into_the_store` above,
    // with no `--check-path` at all this time, since forbidden carries none.

    #[test]
    fn declare_with_a_forbidden_check_round_trips_into_the_store() {
        let mut db = thor_core::event_store::EventStore::in_memory().unwrap();
        let literals = vec!["\u{2014}".to_string(), "\u{2013}".to_string(), "...".to_string()];
        let check = gate::build_check(Some("forbidden"), None, None, literals.clone()).unwrap();
        let item = declare_item_with_check(
            "cli-check-forbidden-1",
            "never write an em dash, en dash or ellipsis, anywhere",
            check,
        );

        store::declare(&mut db, "s", "l", "test", &item).unwrap();
        let back = store::show(&db, "cli-check-forbidden-1").unwrap();
        assert_eq!(back.check, Some(Check::Forbidden { literals }));
    }

    #[test]
    fn declare_with_an_empty_forbidden_literal_list_writes_nothing() {
        // The empty-list refusal from gate::build_check itself (never
        // reaching store::declare at all) - mirrors declare_with_an_empty_
        // absent_all_literal_list_writes_nothing above for this form.
        let db = thor_core::event_store::EventStore::in_memory().unwrap();
        let err = gate::build_check(Some("forbidden"), None, None, vec![]);
        assert!(err.is_err());
        assert!(db.get_all_events().unwrap().is_empty(), "a refused check must leave the store untouched");
    }

    #[test]
    fn declare_with_a_forbidden_check_given_a_check_path_writes_nothing() {
        // THE GROUND UNIQUE TO THIS KIND: forbidden carries no path at all -
        // a `--check-path` given alongside `--check-kind forbidden` must be
        // refused before an Item is ever built, the same as every other
        // refused combination above, never silently accepted and dropped.
        let db = thor_core::event_store::EventStore::in_memory().unwrap();
        let err = gate::build_check(
            Some("forbidden"),
            Some("docs/STYLE.md".to_string()),
            None,
            vec!["TODO".to_string()],
        );
        assert!(err.is_err());
        assert!(db.get_all_events().unwrap().is_empty(), "a refused check must leave the store untouched");
    }
}
