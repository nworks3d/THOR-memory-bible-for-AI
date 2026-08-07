//! `codeindex`: a git-derived index of the owner's code.
//!
//! Built to solve one problem: the owner codes locally and sometimes
//! remotely, and a memory system that only refreshes its copy of the code
//! when explicitly told to gives two different truths - a fresh local one
//! and a stale remote one. This crate is the single source instead: an
//! index built by reading git (never a directory scan), with provenance on
//! every answer so a stale answer is always a LABELLED stale answer, never
//! a silent one.
//!
//! What each module does:
//! - [`gitutil`]: every fact about the repository (HEAD, tracked files, a
//!   file's content, whether the working tree is dirty) comes from running
//!   `git` as a subprocess, never from parsing `.git` by hand.
//! - [`chunk`]: splits one file's text into stored pieces.
//! - [`store`]: the sqlite-backed index itself - [`store::build_full`] (a
//!   full build from HEAD), [`store::refresh`] (a cheap, per-file update),
//!   [`store::search`] (the lookup, always paired with
//!   [`store::Provenance`]) and [`store::human_status`] (a status line a
//!   person can read without knowing any of this).
//!
//! Deliberately NOT built here: any serve path, hook, or injection channel.
//! Requirement 5 of the design note this crate answers is explicit that code
//! is an archive - findable only when something explicitly asks `search` or
//! `provenance` a question, never pushed into anyone's context on its own.
//! The `codeindex` binary in `src/bin` is a manual CLI over this library, not
//! a channel into anything else in this workspace on its own.
//!
//! `serve::lookup` (surface 4, OPZOEKEN) is this crate's one caller anywhere
//! in the workspace: `lookup::search_code`/`lookup::code_index_status` open a
//! `Store` and call `search`/`provenance` from here, translating the answer
//! into types `serve` owns before anything else sees it. That is a deliberate
//! narrow door, not a loosening of "archive, never injected": `serve/tests/
//! codeindex_is_lookup_only.rs` greps every other file under `serve/src` for
//! this crate's name and fails if session_start, the moment/prompt surfaces,
//! or the hook channel ever gain a path to it.

pub mod chunk;
pub mod gitutil;
pub mod store;
pub mod symbols;

pub use store::{
    build_full, human_status, provenance, refresh, search, BuildStats, CodeIndexError, Provenance,
    RefreshStats, SearchAnswer, SearchHit, Store,
};
pub use symbols::{is_indexed, outline, where_used, Site, Usage};
