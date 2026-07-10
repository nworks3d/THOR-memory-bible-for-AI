//! Repository helpers shared by ingest and project-scoped recall.
//!
//! THOR indexes a repo's TRACKED text files directly (source is the source of
//! truth). A chunk's entity_id is `<project>:<relpath>#<n>`, so the project a
//! fact belongs to is encoded in its id: everything before the FIRST `:`.
//! Unprefixed ids (ULID memories, mcp-* facts) are GLOBAL and never scoped out.
//! Project isolation (recall in project A must not surface project B's code or
//! memories) and ingest both agree on the project key via `project_key` (a
//! `.thor` marker, else the repo-root basename), so an id written by ingest
//! matches what the courier derives from the working directory. The effective
//! project can be reassigned by a `fact_reprojected` event (see cas).

use std::path::{Path, PathBuf};

/// Chunk cap in characters (line-boundary split). Matches the original ingest.
pub const MAX_CHUNK_CHARS: usize = 1800;
/// Per-file ceiling: giant minified bundles are truncated, not fully ingested.
pub const MAX_FILE_CHARS: usize = 200_000;

/// Binary / asset / lock extensions never worth indexing as text. Includes CAD,
/// mesh, EDA and toolpath formats: some (STEP, Gerber) are technically ASCII but
/// are coordinate dumps with no recall value, and one large file would otherwise
/// drown a small project's real docs in hundreds of noise chunks.
pub const SKIP_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "ico", "svg", "webp", "bmp", "tif", "tiff", "woff", "woff2", "ttf",
    "eot", "otf", "pdf", "zip", "gz", "tgz", "7z", "rar", "mp3", "mp4", "mov", "avi", "webm", "wav",
    "db", "sqlite", "bin", "exe", "dll", "so", "dylib", "class", "jar", "wasm", "lock",
    // CAD / mesh / EDA / toolpath (asset dumps, not source or docs)
    "step", "stp", "stl", "3mf", "f3d", "iges", "igs", "obj", "dxf", "dwg", "gbr", "gcode",
    // line-delimited data dumps (eval corpora, exports): rows with no recall
    // value that compete with the real facts they mention - a chunked eval
    // scenario looks exactly like the drift prompt it was written to test
    "jsonl", "ndjson",
];

/// Walk up from `start` until a directory containing a `.git` entry is found.
/// Returns that directory (the repo root). Pure filesystem stats - no `git`
/// subprocess - so it is cheap enough for the per-prompt courier.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir: &Path = start;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}


/// The project that owns an entity: the id prefix before the first `:` for repo
/// chunks; `None` for unprefixed (global) facts, which are never scoped out.
pub fn owner_project(entity_id: &str) -> Option<&str> {
    entity_id.split_once(':').map(|(p, _)| p)
}

/// Reserved project key for cross-cutting knowledge that must surface in EVERY
/// project (working rules, dev-loop, conventions - ingested with `--global`). A
/// leading `@` cannot be a git repo-root basename or a normal project key, so it
/// never collides with a real project or the chunk namespace.
pub const GLOBAL_KEY: &str = "@global";

/// True when a fact belongs to the always-in-scope global tier: an unprefixed id
/// (`None`, a global memory) OR the reserved `@global` key (a global file).
pub fn is_global(project: Option<&str>) -> bool {
    matches!(project, None | Some(GLOBAL_KEY))
}

/// The `.thor` marker key if the project is ONBOARDED: walk up, return the marker's
/// trimmed content (validated), or the dir basename if the marker is empty/invalid.
/// `None` once the git repo root is reached without a marker (or there is no repo) -
/// i.e. the project is not yet set up in THOR. No git subprocess (courier-cheap).
pub fn thor_marker_key(start: &Path) -> Option<String> {
    let mut dir: &Path = start;
    loop {
        let marker = dir.join(".thor");
        if let Ok(content) = std::fs::read_to_string(&marker) {
            let key = content.trim();
            if !key.is_empty() && !key.contains(':') && !key.contains('#') {
                return Some(key.to_string());
            }
            // marker present but empty/invalid -> fall back to this dir's basename
            return dir.file_name().and_then(|n| n.to_str()).map(|s| s.to_string());
        }
        if dir.join(".git").exists() {
            return None; // repo root, no marker -> not onboarded
        }
        dir = dir.parent()?;
    }
}

/// The project KEY for a working directory: the `.thor` marker (onboarded) FIRST,
/// else the git repo-root basename. `None` for a scratch dir (no marker, no repo).
pub fn project_key(start: &Path) -> Option<String> {
    if let Some(key) = thor_marker_key(start) {
        return Some(key);
    }
    let mut dir: &Path = start;
    loop {
        if dir.join(".git").exists() {
            return dir.file_name().and_then(|n| n.to_str()).map(|s| s.to_string());
        }
        dir = dir.parent()?;
    }
}

/// Mint a memory entity_id. A project-scoped memory is `<key>:mem-<uuid>` (disjoint
/// from the chunk namespace `<key>:<rel>#<n>` by the `mem-` segment + no `#<n>`);
/// a global memory stays unprefixed `mcp-<uuid>`.
pub fn memory_entity_id(project: Option<&str>, uuid: &str) -> String {
    match project {
        Some(key) if !is_global(Some(key)) => format!("{}:mem-{}", key, uuid),
        _ => format!("mcp-{}", uuid),
    }
}

/// Tracked text files of a repo via `git ls-files` (only tracked paths, so
/// gitignored secrets like `.env` are never read). Empty on any git failure.
pub fn tracked_files(repo: &Path) -> Vec<String> {
    match std::process::Command::new("git").arg("-C").arg(repo).arg("ls-files").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Directory names never worth walking when indexing a NON-git folder (build
/// output, dependency caches). A git repo uses `git ls-files` instead, so these
/// are already excluded there via .gitignore.
pub const SKIP_DIR: &[&str] =
    &["node_modules", "target", "dist", "build", "out", "vendor", "venv", "__pycache__"];

/// Files of a plain (NON-git) directory, walked directly. Returns `/`-joined
/// relative paths so a chunk id matches the git convention cross-platform, plus a
/// `complete` flag (see below). This is the fallback for `thor ingest` on a folder
/// with no `.git` (mimir parity: index a loose docs folder), so it CANNOT lean on
/// `.gitignore` to hide secrets. The guards that stand in for it:
/// - skip every dotfile/dot-dir (`.git`, `.env`, `.ssh`, `.venv`, `.thor`, ...);
/// - skip known heavy dirs (`SKIP_DIR`);
/// - never follow a symlink (cycle-safe);
/// - treat a NESTED git repo (a subdir with its own `.git`) as a boundary and never
///   walk its working tree - that would ingest files its own `.gitignore` hides
///   (a secret leak) and it is a different project anyway. Ingest it separately.
/// Binary/asset extensions are filtered by the caller via `is_skip_ext`.
///
/// `complete` is false if any `read_dir`/`file_type` errored (e.g. a transient SMB
/// hiccup on the NAS), so the caller can tell "the folder is empty/shrunk" from "I
/// could not read part of it" and must NOT retract chunks for a subtree it failed to
/// read - unlike `git ls-files`, a directory walk can return a PARTIAL list.
pub fn walk_files(root: &Path) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut complete = true;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // nested-repo boundary: never descend into a subdir that is its own git repo
        if dir != root && dir.join(".git").exists() {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => {
                complete = false; // a subtree we could not read: not the same as deleted
                continue;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue; // dotfiles/dot-dirs: .git, .env, .ssh, .venv, .thor ...
            }
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            if ft.is_symlink() {
                continue; // never follow symlinks (avoids cycles + escapes)
            }
            let path = entry.path();
            if ft.is_dir() {
                if SKIP_DIR.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    if !rel.is_empty() {
                        out.push(rel);
                    }
                }
            }
        }
    }
    out.sort(); // deterministic order so re-ingest is a stable no-op
    (out, complete)
}

/// Validate a user-supplied project KEY: non-empty and free of the `:` / `#`
/// separators that structure a chunk id (`<project>:<rel>#<n>`). Shared by
/// `init`, `reproject`, and `ingest --project` so no path can mint a mis-scoped id.
pub fn validate_project_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.contains(':') || key.contains('#') {
        return Err(format!("invalid project key '{}': must be non-empty with no ':' or '#'", key));
    }
    Ok(())
}

/// Strip Windows' verbatim/extended-length prefix that `canonicalize` adds, so a
/// path is usable by `git -C`, `read_dir`, and `join`. `\\?\UNC\server\share` ->
/// `\\server\share` (a real UNC path); `\\?\C:\x` -> `C:\x`. A no-op elsewhere.
pub fn clean_verbatim_prefix(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{}", rest))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

/// True when the file extension is a known binary/asset/lock type to skip.
pub fn is_skip_ext(rel: &str) -> bool {
    match rel.rsplit_once('.') {
        Some((_, ext)) => SKIP_EXT.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Truncate a file's text to MAX_FILE_CHARS on a char boundary. Returns true
/// when it truncated. ONE implementation shared by ingest and the courier's
/// freshness check: the freshness comparison only works if both sides chunk
/// byte-for-byte the same text, so the cut rule must never fork.
pub fn truncate_to_max_file_chars(text: &mut String) -> bool {
    if text.chars().count() <= MAX_FILE_CHARS {
        return false;
    }
    let cut = text.char_indices().nth(MAX_FILE_CHARS).map(|(i, _)| i).unwrap_or(text.len());
    text.truncate(cut);
    true
}

/// Split text into <= `max`-char chunks on line boundaries; a single line longer
/// than `max` is hard-split on char boundaries. Blank-only chunks are dropped.
/// Mirrors the reference ingest so re-ingesting an unchanged file is a no-op.
pub fn chunk_text(text: &str, max: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut size = 0usize; // chars currently in buf
    for mut line in text.split_inclusive('\n') {
        // hard-split an over-long single line (minified code)
        while line.chars().count() > max {
            if !buf.is_empty() {
                chunks.push(std::mem::take(&mut buf));
                size = 0;
            }
            let cut = line.char_indices().nth(max).map(|(i, _)| i).unwrap_or(line.len());
            chunks.push(line[..cut].to_string());
            line = &line[cut..];
        }
        let ll = line.chars().count();
        if size + ll > max && !buf.is_empty() {
            chunks.push(std::mem::take(&mut buf));
            size = 0;
        }
        buf.push_str(line);
        size += ll;
    }
    if !buf.is_empty() {
        chunks.push(buf);
    }
    chunks.into_iter().filter(|c| !c.trim().is_empty()).collect()
}

/// Source extensions that get symbol-boundary chunking. Everything else
/// (markdown, config, prose) keeps the plain line-packing chunker.
const SOURCE_EXTS: &[&str] = &[
    "rs", "js", "jsx", "ts", "tsx", "py", "sh", "bash", "ps1", "psm1", "go", "c", "h", "hpp",
    "cpp", "cc", "java", "rb", "php", "cs", "swift", "kt",
];

pub fn is_source_file(rel: &str) -> bool {
    match rel.rsplit_once('.') {
        Some((_, ext)) => SOURCE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// True when `line` opens a TOP-LEVEL symbol (function/type/class/section of
/// code): non-indented and led by a definition keyword, after any number of
/// common visibility/async prefixes. Deliberately a cheap surface check - this
/// runs on the courier's hot freshness path, so it must stay a linear scan
/// (no parser, no regex engine).
fn is_symbol_start(line: &str) -> bool {
    if line.is_empty() || line.starts_with(char::is_whitespace) {
        return false;
    }
    let mut l = line;
    loop {
        let before = l;
        for p in ["pub(crate) ", "pub(super) ", "pub ", "export ", "default ", "async ",
                  "unsafe ", "extern \"C\" ", "static ", "final ", "abstract "] {
            l = l.strip_prefix(p).unwrap_or(l);
        }
        if l == before {
            break;
        }
    }
    const KW: &[&str] = &[
        "fn ", "impl ", "impl<", "struct ", "enum ", "trait ", "mod ", "macro_rules!",
        "class ", "def ", "function ", "interface ", "const fn ",
    ];
    KW.iter().any(|k| l.starts_with(k))
}

/// Symbol-boundary chunking for source files: the text is split into blocks at
/// top-level symbol starts (the preamble - imports, consts - is the first
/// block), whole blocks are then greedily packed into <= `max`-char chunks so
/// small symbols share a chunk instead of each minting a near-dup-prone sliver,
/// and a single block larger than `max` falls back to the plain line packer
/// INSIDE the block - its parts stay adjacent ordinals, which is exactly what
/// neighbor stitching re-joins at serving time. Concatenating the returned
/// chunks reproduces the input byte-for-byte (same invariant as chunk_text -
/// the freshness comparison depends on it). Idea credit: symbol-boundary code
/// chunks come from mimir's CodeChunk round (MakerViking/mimir); this is a
/// dependency-free reimplementation, not tree-sitter.
pub fn chunk_source(text: &str, max: usize) -> Vec<String> {
    // 1. blocks at top-level symbol boundaries
    let mut blocks: Vec<String> = Vec::new();
    let mut buf = String::new();
    for line in text.split_inclusive('\n') {
        if is_symbol_start(line) && !buf.is_empty() {
            blocks.push(std::mem::take(&mut buf));
        }
        buf.push_str(line);
    }
    if !buf.is_empty() {
        blocks.push(buf);
    }
    // 2. pack whole blocks up to max; oversized blocks split internally
    let mut chunks: Vec<String> = Vec::new();
    let mut packed = String::new();
    let mut size = 0usize;
    for block in blocks {
        let bl = block.chars().count();
        if bl > max {
            if !packed.is_empty() {
                chunks.push(std::mem::take(&mut packed));
                size = 0;
            }
            chunks.extend(chunk_text(&block, max));
            continue;
        }
        if size + bl > max && !packed.is_empty() {
            chunks.push(std::mem::take(&mut packed));
            size = 0;
        }
        packed.push_str(&block);
        size += bl;
    }
    if !packed.is_empty() {
        chunks.push(packed);
    }
    chunks.into_iter().filter(|c| !c.trim().is_empty()).collect()
}

/// THE chunker dispatch, used by ingest AND the courier's freshness re-read:
/// both sides must chunk byte-for-byte identically or freshness misreports
/// every chunk of a file as changed. Never call chunk_text/chunk_source
/// directly for file content - route through here.
pub fn chunk_file(rel: &str, text: &str, max: usize) -> Vec<String> {
    if is_source_file(rel) {
        chunk_source(text, max)
    } else {
        chunk_text(text, max)
    }
}

/// Markdown files get a heading-trail crumb in the chunk footer; other doc
/// formats have no `#` headings and code files must never get one (a `#`
/// comment is not a heading).
const CRUMB_EXTS: &[&str] = &["md", "markdown"];

pub fn is_crumb_doc(rel: &str) -> bool {
    std::path::Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| CRUMB_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Rendered length cap for a trail: a crumb is orientation, not content.
const MAX_CRUMB_CHARS: usize = 90;

/// The markdown heading trail ACTIVE at the START of each chunk ("Setup >
/// Windows > Paths"): the doc-chunk breadcrumb as a STRUCTURED footer field,
/// so a chunk cut below its heading still carries where it belongs - findable
/// by FTS, stripped with the footer by dedup/snippets, and (footer = body
/// tail) essentially invisible to the embedder's content window. One
/// (possibly empty) trail per chunk.
pub fn heading_trails(chunks: &[String]) -> Vec<String> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    // Fence state carries ACROSS chunks: a ``` block split over a chunk
    // boundary must keep masking its `# comment` lines - a shell comment in a
    // fenced example is not a heading and must never pop the real stack.
    let mut in_fence = false;
    let mut trails = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let trail = stack.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > ");
        trails.push(trail.chars().take(MAX_CRUMB_CHARS).collect());
        for line in chunk.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
            let hashes = line.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&hashes) && line.chars().nth(hashes).is_some_and(|c| c == ' ') {
                let title: String = crate::footer::field_safe(line[hashes..].trim())
                    .chars()
                    .take(60)
                    .collect();
                if !title.is_empty() {
                    stack.retain(|(lvl, _)| *lvl < hashes);
                    stack.push((hashes, title));
                }
            }
        }
    }
    trails
}

/// The recall body for chunk `i` (0-based) of `total` from `project/rel`.
/// `crumb` (empty = no field) is the heading trail from [`heading_trails`].
pub fn chunk_body(chunk: &str, project: &str, rel: &str, i: usize, total: usize, crumb: &str) -> String {
    if crumb.is_empty() {
        format!("{}\n\n[repo file | {}/{} | chunk {}/{}]", chunk.trim_end(), project, rel, i + 1, total)
    } else {
        format!(
            "{}\n\n[repo file | {}/{} | chunk {}/{} | crumb: {}]",
            chunk.trim_end(),
            project,
            rel,
            i + 1,
            total,
            crumb
        )
    }
}

/// The stable entity_id for chunk `i` (0-based) of `project/rel`.
pub fn chunk_entity_id(project: &str, rel: &str, i: usize) -> String {
    format!("{}:{}#{}", project, rel, i)
}

/// True when an id is a repo-chunk id `<project>:<rel>#<n>` (has a `:` and ends in
/// `#<digits>`). This distinguishes a chunk from a project-scoped memory id
/// `<project>:mem-<uuid>`, so ingest never retracts a memory as a vanished chunk.
pub fn is_chunk_id(entity_id: &str) -> bool {
    match entity_id.rsplit_once('#') {
        Some((head, n)) => head.contains(':') && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// The constraint class of a hand-written fact, so the courier / guard / brief
/// can label (and later prioritize) the facts that prevent drift. Only these
/// three classes get a tag; anything else (note, insight, plain text) is None.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactType {
    Gotcha,
    Decision,
    Preference,
}

impl FactType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FactType::Gotcha => "gotcha",
            FactType::Decision => "decision",
            FactType::Preference => "preference",
        }
    }
}

/// Classify a fact body (shim: the footer format, its parsers and the leading
/// type markers live together in crate::footer, so the writer and every reader
/// share ONE definition and can never drift apart).
pub fn fact_type(body: &str) -> Option<FactType> {
    crate::footer::fact_type(body)
}

/// The project ROOT directory for a working dir: the nearest ancestor holding a
/// `.thor` marker or a `.git` entry. This is the base repo-chunk `rel` paths
/// resolve against (the courier's freshness check re-reads files from here).
pub fn project_root(start: &Path) -> Option<PathBuf> {
    let mut dir: &Path = start;
    loop {
        if dir.join(".thor").exists() || dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_project_prefix() {
        // The birth project = the prefix before the first ':' (repo chunk or a
        // project-minted memory); unprefixed = global. (Scope enforcement itself
        // lives in recall::RecallScope, tested there.)
        assert_eq!(owner_project("ProjA:src/x.rs#0"), Some("ProjA"));
        assert_eq!(owner_project("ProjA:mem-abc"), Some("ProjA"));
        assert_eq!(owner_project("@global:dev-loop.md#0"), Some("@global"));
        assert_eq!(owner_project("01ARZ3NDEKTSV4RRFFQ69G5FAV"), None);
        assert_eq!(owner_project("mcp-40480511-8679-4244"), None);
    }

    #[test]
    fn heading_trails_track_the_markdown_stack() {
        let text = "# Guide\nintro line\n## Setup\nsetup text\n### Windows\nwin text\n## Usage\nusage text\n";
        let chunks: Vec<String> = text.lines().map(|l| format!("{l}\n")).collect();
        let trails = heading_trails(&chunks);
        assert_eq!(trails[0], "", "nothing active before the first heading");
        assert_eq!(trails[1], "Guide", "intro sits under the H1");
        assert_eq!(trails[3], "Guide > Setup");
        assert_eq!(trails[5], "Guide > Setup > Windows");
        assert_eq!(trails[7], "Guide > Usage", "an H2 pops the deeper H3");
        // '#' without a space is code/comment, never a heading
        let code: Vec<String> = vec!["#!/bin/sh\n".into(), "#define X 1\n".into(), "echo hi\n".into()];
        assert!(heading_trails(&code).iter().all(|t| t.is_empty()));

        // a '# comment' inside a fenced code block is NOT a heading - even
        // when the fence spans a chunk boundary
        let fenced: Vec<String> = vec![
            "# Guide\n## Setup\n```bash\n".into(),
            "# a shell comment, not a heading\n```\n".into(),
            "## Usage\n".into(),
            "usage text\n".into(),
        ];
        let trails = heading_trails(&fenced);
        assert_eq!(trails[2], "Guide > Setup", "fenced comment never entered the stack");
        assert_eq!(trails[3], "Guide > Usage", "the real H2 still lands after the fence");
    }

    #[test]
    fn chunk_body_renders_crumb_and_strip_removes_it() {
        let with = chunk_body("content line", "Proj", "docs/guide.md", 1, 3, "Guide > Setup");
        assert!(with.ends_with("[repo file | Proj/docs/guide.md | chunk 2/3 | crumb: Guide > Setup]"), "{with}");
        assert_eq!(crate::footer::strip(&with), "content line", "the crumb strips with the footer");
        let without = chunk_body("content line", "Proj", "src/a.rs", 0, 1, "");
        assert!(without.ends_with("[repo file | Proj/src/a.rs | chunk 1/1]"), "no empty crumb field: {without}");
        assert!(is_crumb_doc("docs/guide.md") && is_crumb_doc("README.MD"));
        assert!(!is_crumb_doc("src/a.rs") && !is_crumb_doc("notes.txt"));
    }

    #[test]
    fn symbol_chunker_packs_small_symbols_and_splits_oversized_ones() {
        for (line, want) in [
            ("pub fn alpha() {", true),
            ("fn beta() {", true),
            ("    fn indented_method() {", false),
            ("class Foo:", true),
            ("def foo():", true),
            ("async def bar():", true),
            ("export default async function go() {", true),
            ("let x = 1;", false),
            ("use std::fs;", false),
            ("impl Widget {", true),
        ] {
            assert_eq!(is_symbol_start(line), want, "{line}");
        }
        let big_body = format!("fn big() {{\n{}}}\n", "    line();\n".repeat(20));
        let text = format!(
            "use std::fs;\n\nfn a() {{ one(); }}\n\nfn b() {{ two(); }}\n\n{}fn c() {{ three(); }}\n",
            big_body
        );
        let chunks = chunk_source(&text, 80);
        // concatenation reproduces the input byte-for-byte (freshness invariant)
        assert_eq!(chunks.concat(), text, "chunk concat must equal input");
        // small fns pack together instead of minting sliver chunks
        let a_chunk = chunks.iter().find(|c| c.contains("fn a()")).unwrap();
        assert!(a_chunk.contains("fn b()"), "small siblings share a chunk: {a_chunk}");
        // the oversized fn is split internally across ADJACENT chunks
        let big_parts = chunks.iter().filter(|c| c.contains("line();")).count();
        assert!(big_parts >= 2, "oversized symbol splits into multiple parts");
        // a small symbol after the big one starts fresh, never glued mid-symbol
        assert!(chunks.iter().any(|c| c.contains("fn c()")));
        // dispatcher: markdown keeps the plain packer, source routes to symbols
        assert_eq!(chunk_file("docs/x.md", &text, 80), chunk_text(&text, 80));
        assert_eq!(chunk_file("src/x.rs", &text, 80), chunks);
    }

    #[test]
    fn chunking_matches_reference() {
        // short text: one chunk
        assert_eq!(chunk_text("hello\nworld\n", 1800), vec!["hello\nworld\n"]);
        // blank-only dropped
        assert!(chunk_text("   \n\n", 1800).is_empty());
        // line longer than cap is hard-split
        let long = "x".repeat(50);
        let out = chunk_text(&long, 20);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].chars().count(), 20);
        // splitting on line boundary when the buffer would overflow
        let two = "aaaaaaaaaa\nbbbbbbbbbb\n"; // 11 + 11 chars incl newlines
        let out = chunk_text(two, 15);
        assert_eq!(out.len(), 2, "each ~11-char line lands in its own <=15 chunk");
    }

    #[test]
    fn skip_ext() {
        assert!(is_skip_ext("logo.PNG"));
        assert!(is_skip_ext("a/b/pkg.lock"));
        assert!(is_skip_ext("cad/housing.step"), "CAD dumps are skipped");
        assert!(is_skip_ext("pcb/top.GBR"), "Gerber files are skipped");
        assert!(!is_skip_ext("src/main.rs"));
        assert!(!is_skip_ext("Makefile"));
    }

    #[test]
    fn ids_are_stable_and_prefixed() {
        assert_eq!(chunk_entity_id("Proj", "a/b.rs", 2), "Proj:a/b.rs#2");
        assert_eq!(owner_project(&chunk_entity_id("Proj", "a/b.rs", 2)), Some("Proj"));
    }

    #[test]
    fn global_tier_and_ids() {
        assert!(is_global(None));
        assert!(is_global(Some(GLOBAL_KEY)));
        assert!(!is_global(Some("ProjA")));
        // chunk vs memory id shapes
        assert!(is_chunk_id("Proj:a/b.rs#0"));
        assert!(is_chunk_id("@global:dev-loop.md#3"));
        assert!(!is_chunk_id("Proj:mem-abc123"), "project memory is not a chunk");
        assert!(!is_chunk_id("01KGLOBALMEMORY"));
        assert!(!is_chunk_id("mcp-uuid"));
        // memory id minting
        assert_eq!(memory_entity_id(Some("Proj"), "u1"), "Proj:mem-u1");
        assert_eq!(memory_entity_id(None, "u2"), "mcp-u2");
        assert_eq!(memory_entity_id(Some(GLOBAL_KEY), "u3"), "mcp-u3", "global memory stays unprefixed");
    }

    #[test]
    fn project_key_from_marker_then_git() {
        let tmp = tempfile::tempdir().unwrap();
        // .thor marker wins, its content is the key
        let a = tmp.path().join("A");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join(".thor"), "MyStableKey\n").unwrap();
        assert_eq!(project_key(&a).as_deref(), Some("MyStableKey"));
        // git repo with no marker -> repo-root basename
        let b = tmp.path().join("CoolRepo");
        std::fs::create_dir_all(b.join(".git")).unwrap();
        assert_eq!(project_key(&b).as_deref(), Some("CoolRepo"));
        // subdir walks up to the git root
        let sub = b.join("src").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(project_key(&sub).as_deref(), Some("CoolRepo"));
        // empty/invalid marker -> falls back to this dir's basename
        let c = tmp.path().join("Weird");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join(".thor"), "  bad:key#  ").unwrap();
        assert_eq!(project_key(&c).as_deref(), Some("Weird"));
        // scratch dir (no marker, no git) -> None
        let d = tmp.path().join("scratch");
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(project_key(&d), None);
    }

    #[test]
    fn walk_files_skips_dotfiles_and_heavy_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let w = |rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        };
        w("readme.md");
        w("src/main.rs");
        w("logo.png"); // walk returns it; is_skip_ext filters binaries at ingest, not here
        w(".env"); // dotfile -> skipped (stands in for the missing .gitignore)
        w(".hidden/inside.txt"); // dot-dir -> skipped
        w("node_modules/junk.js"); // heavy dir -> skipped
        // a NESTED git repo: its working tree must NOT be walked (secret-leak boundary)
        w("vendored/lib/config.local.json"); // gitignored-by-inner-repo secret
        std::fs::create_dir_all(root.join("vendored/lib/.git")).unwrap();
        let (got, complete) = walk_files(root); // already sorted
        assert_eq!(
            got,
            vec!["logo.png".to_string(), "readme.md".to_string(), "src/main.rs".to_string()],
            "walk keeps normal files (fwd-slash), drops dotfiles/dot-dirs/heavy dirs/nested repos"
        );
        assert!(complete, "a fully-readable tree reports complete");
    }

    #[test]
    fn fact_type_from_footer_and_markers() {
        // mimir footer wins (the exact live format)
        assert_eq!(
            fact_type("never open the db over SMB\n\n[memory/gotcha | tags: db wal | project: P | mimir:01K]"),
            Some(FactType::Gotcha)
        );
        assert_eq!(
            fact_type("body\n\n[memory/decision | tags: x | project: global | mimir:01K]"),
            Some(FactType::Decision)
        );
        // a typed footer of another class stays untyped (authoritative footer)
        assert_eq!(fact_type("body\n\n[memory/note | tags: x | project: P | mimir:01K]"), None);
        // leading uppercase markers (EN + NL)
        assert_eq!(fact_type("GOTCHA: never do X when Y"), Some(FactType::Gotcha));
        assert_eq!(fact_type("DECISION: budget stays top-3"), Some(FactType::Decision));
        assert_eq!(fact_type("BESLISSING (2026): we kiezen A"), Some(FactType::Decision));
        assert_eq!(fact_type("HARDE REGEL: geheugenstore is bron van waarheid"), Some(FactType::Preference));
        assert_eq!(fact_type("WERKWIJZE-VOORKEUR bij analyse"), Some(FactType::Preference));
        // prose does NOT classify (case-sensitive markers)
        assert_eq!(fact_type("the decision was made to defer"), None);
        assert_eq!(fact_type("plain chunk text fn main() {}"), None);
        assert_eq!(fact_type(""), None);
    }

    #[test]
    fn project_root_finds_marker_or_git() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("A");
        std::fs::create_dir_all(a.join("src")).unwrap();
        std::fs::write(a.join(".thor"), "A\n").unwrap();
        assert_eq!(project_root(&a.join("src")), Some(a.clone()));
        let b = tmp.path().join("B");
        std::fs::create_dir_all(b.join(".git")).unwrap();
        assert_eq!(project_root(&b), Some(b.clone()));
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        assert_eq!(project_root(&scratch), None);
    }

    #[test]
    fn validate_project_key_guards_separators() {
        assert!(validate_project_key("ProjB").is_ok());
        assert!(validate_project_key(GLOBAL_KEY).is_ok(), "@global is a valid pinned key");
        assert!(validate_project_key("").is_err(), "empty key rejected");
        assert!(validate_project_key("acme:widgets").is_err(), "':' would mis-split the id");
        assert!(validate_project_key("a#b").is_err(), "'#' would corrupt is_chunk_id");
    }

    #[test]
    fn clean_verbatim_prefix_cases() {
        // canonicalize's verbatim prefix is stripped; a UNC path stays a real UNC path
        assert_eq!(clean_verbatim_prefix(Path::new(r"\\?\C:\x\y")), PathBuf::from(r"C:\x\y"));
        assert_eq!(
            clean_verbatim_prefix(Path::new(r"\\?\UNC\Server\Share\p")),
            PathBuf::from(r"\\Server\Share\p")
        );
        assert_eq!(clean_verbatim_prefix(Path::new(r"C:\normal\p")), PathBuf::from(r"C:\normal\p"));
    }
}
