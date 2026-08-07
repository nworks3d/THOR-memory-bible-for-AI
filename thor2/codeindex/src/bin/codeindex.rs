//! Manual CLI over the `codeindex` library. This is a hand-operated tool for
//! building, refreshing and querying an index - not a serve path. Nothing
//! here is ever called automatically by another crate in this workspace;
//! per the design note this crate answers to, code is an archive, found
//! only when a caller explicitly asks.
//!
//! Usage:
//!   codeindex <db-path> <repo-path> build
//!   codeindex <db-path> <repo-path> refresh
//!   codeindex <db-path> <repo-path> status
//!   codeindex <db-path> <repo-path> search <query> [limit]

use std::path::PathBuf;
use std::process::ExitCode;

use codeindex::{build_full, human_status, provenance, refresh, search, Store};

fn usage() -> String {
    "usage: codeindex <db-path> <repo-path> <build|refresh|status|search> [query] [limit]".to_string()
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err(usage());
    }
    let db_path = PathBuf::from(&args[0]);
    let repo_path = PathBuf::from(&args[1]);
    let command = args[2].as_str();

    let mut store = Store::open(&db_path).map_err(|e| e.to_string())?;

    match command {
        "build" => {
            let stats = build_full(&mut store, &repo_path).map_err(|e| e.to_string())?;
            println!(
                "indexed {} file(s), skipped {} binary file(s), wrote {} chunk(s), at commit {}",
                stats.files_indexed, stats.files_skipped_binary, stats.chunks_written, stats.commit
            );
            Ok(())
        }
        "refresh" => {
            let stats = refresh(&mut store, &repo_path).map_err(|e| e.to_string())?;
            if stats.no_op {
                println!("already up to date at commit {}", stats.new_commit);
            } else {
                println!(
                    "refreshed from commit {} to {}: {} file(s) changed, {} file(s) deleted",
                    stats.previous_commit, stats.new_commit, stats.files_changed, stats.files_deleted
                );
            }
            Ok(())
        }
        "status" => {
            let p = provenance(&store, &repo_path).map_err(|e| e.to_string())?;
            println!("{}", human_status(&p));
            Ok(())
        }
        "search" => {
            let query = args.get(3).ok_or_else(usage)?;
            let limit: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(20);
            let answer = search(&store, &repo_path, query, limit).map_err(|e| e.to_string())?;
            println!("{}", human_status(&answer.provenance));
            for hit in &answer.hits {
                println!(
                    "{}:{}-{} (commit {})\n{}\n",
                    hit.path, hit.start_line, hit.end_line, hit.commit_id, hit.text
                );
            }
            Ok(())
        }
        _ => Err(usage()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
