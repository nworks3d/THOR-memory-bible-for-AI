//! `mcp --db <path>`: serve the agent surface over stdio for one store.
//! Never creates the store (see `EventStore::open_existing`'s own doc comment)
//! - a typo'd `--db` is a plain startup error, not a fresh, healthy-looking
//! empty memory.
//!
//! `--code-index-db` and `--repo` are optional and go together: without them
//! the `search_code` tool still exists and says plainly that no index is
//! configured, which is the honest answer. Silently returning no hits would
//! read as "your code does not contain that".

use clap::Parser;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "mcp",
    about = "Serve THOR 2.0's agent surface (remember/revise/retract/resolve/get/lookup/history/search_code/mark/status) over stdio"
)]
struct Cli {
    /// Path to the core event-log database. Must already exist.
    #[arg(long)]
    db: PathBuf,
    /// Path to a code index built with the `codeindex` command.
    #[arg(long, requires = "repo")]
    code_index_db: Option<PathBuf>,
    /// The git checkout that index was built from. Provenance on every code
    /// answer is measured against this, so it has to be the real one.
    #[arg(long, requires = "code_index_db")]
    repo: Option<PathBuf>,
    /// A DIRECTORY of code indexes, one per project, named `<project>.db`.
    /// Which one applies is resolved from the working directory this server
    /// was started in - the same resolution `serve` uses for everything else
    /// (`serve::project::resolve_project`: a marker file wins, else the git
    /// root's basename). This is the multi-project form: one registration,
    /// and every project gets its own index without naming any of them.
    /// Ignored when `--code-index-db` is given explicitly.
    #[arg(long)]
    code_index_root: Option<PathBuf>,
    /// Serve over HTTP on this address instead of stdio, e.g. 0.0.0.0:5557.
    /// Only for a replica connector that a session away from the main machine
    /// can reach, and only together with `--capture-inbox` - see
    /// `mcp::run_http` for why the port does not open without one.
    #[arg(long, requires = "capture_inbox")]
    http: Option<String>,
    /// Queue every write to this file instead of applying it, for the
    /// authority to drain later (`sync drain --from`). This is what makes a
    /// machine a replica; without it a write here would fork the log away
    /// from the main store.
    #[arg(long, requires = "http")]
    capture_inbox: Option<PathBuf>,
    /// A hostname or address clients will reach this connector at, repeatable.
    /// Required with `--http` on any bind beyond loopback: the transport
    /// refuses a request whose `Host` header is not listed, and its default
    /// list is loopback only. A bare host matches any port.
    #[arg(long = "allowed-host", requires = "http")]
    allowed_hosts: Vec<String>,
}

/// Which code index applies to the directory this process was started in.
/// `None` on anything unusual - not a git checkout, no index built for this
/// project yet - and the code tools then say so plainly rather than answering
/// from somebody else's repository, which would be worse than not answering.
fn resolve_code_index(root: &Path) -> Option<mcp::CodeIndexPaths> {
    let cwd = std::env::current_dir().ok()?;
    let project = serve::project::resolve_project(&cwd)?;
    let repo = serve::project::git_root(&cwd)?;
    let index_db = root.join(format!("{project}.db"));
    index_db.exists().then_some(mcp::CodeIndexPaths { index_db, repo })
}

fn main() {
    let cli = Cli::parse();
    let code = match (cli.code_index_db, cli.repo) {
        (Some(index_db), Some(repo)) => Some(mcp::CodeIndexPaths { index_db, repo }),
        // clap's `requires` makes the half-given cases unreachable.
        _ => cli.code_index_root.as_deref().and_then(resolve_code_index),
    };
    // The checkout this server was started in, for the write gate's
    // neighbourhood toll. Resolved once at startup, the same assumption
    // `resolve_code_index` above already makes about the working directory.
    // An HTTP connector has no meaningful working directory, so it gets none.
    let root = std::env::current_dir().ok().and_then(|cwd| serve::project::git_root(&cwd));

    let result = match cli.http {
        Some(bind) => mcp::run_http(&cli.db, &bind, code, cli.capture_inbox, cli.allowed_hosts),
        None => mcp::run_in(&cli.db, code, root),
    };
    if let Err(e) = result {
        eprintln!("mcp: fatal: {e}");
        std::process::exit(1);
    }
}
