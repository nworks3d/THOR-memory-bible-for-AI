![The AI Memory Bible](assets/banner.png)

# THOR - one lossless, local memory for AI coding agents

THOR is a from-scratch persistent memory for AI coding agents (such as Claude
Code). It ingests your **repositories, docs and decisions** into a single local
index and gives the relevant pieces back **automatically, at the right moment** -
so a session never starts from zero, even right after a compaction. It is a single
Rust binary: no external services, no git required, and it never loses a write.

![THOR vs mimir - coverage, quality, multi-project, drift and speed](assets/benchmark.svg)

## Why THOR

THOR's thesis is to replace *both* the repo knowledge *and* the memory tool with
one thing the agent can search automatically. Measured against
[mimir](https://github.com/MakerViking/mimir) on the same machine
([full method + weaknesses](BENCHMARKS.md)):

- **It has the answer, automatically.** THOR chunks your source, docs and memories
  into one index that auto-recall searches every prompt - so a code question is
  answered without the agent doing anything. As deployed, **86% vs 27%** on 500
  real questions.
- **It ranks better even on equal footing.** On facts both systems have, THOR still
  leads **91% vs 75%** - a dense score-fusion layer catches paraphrases that
  keyword search misses.
- **It compensates for session drift.** After a compaction the agent starts blank;
  THOR puts the governing gotcha/decision back in front of it **~1.25x more often**
  than mimir at its best (74% vs 59%). This is what the tool is *for*.
- **It is faster and lighter.** ~**3.1x** lower per-prompt latency (83 ms vs
  254 ms) as a single native binary; the default mode holds no resident process.
- **It never loses a write.** Every fact is an event in a hash-chained append-only
  log; a conflicting edit *branches* (both heads kept) instead of overwriting, and
  `fsck` recomputes the chain so tampering is detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted, daemon
  down - each path falls back to bm25 and can never make recall worse.

The one thing mimir does that THOR deliberately does not: a **code-symbol graph**
for "which functions call X". THOR chunks source into recall instead. See
[BENCHMARKS.md](BENCHMARKS.md) for the honest trade-offs.

## What it does

- **Unified local ingest.** `thor ingest <path>` chunks a folder's text files
  (source + docs) into the same append-only store as your remembered facts, so
  auto-recall answers questions about the code itself - not just saved notes. A
  **git repo** reads tracked files only (gitignored secrets are never indexed); a
  plain **non-git folder** is walked directly (dotfiles, heavy dirs and any nested
  git repo skipped), so a loose docs folder indexes too - the same reach as mimir's
  non-git doc collections. It runs incrementally (only changed files are re-chunked; a deleted
  file's chunks are retracted), and, wired into `SessionStart`, keeps the project
  you are working in indexed automatically. CAD/mesh/EDA asset dumps (STEP, STL,
  Gerber, ...) are skipped so they never drown a project's real docs.
- **Project isolation.** A chunk's id is `<project>:<path>#<n>`, so recall inside
  project A never surfaces project B's code (global memories are always kept). No
  bleed between repositories.
- **Lossless append-only store.** Every fact is an event in a hash-chained,
  append-only SQLite log. A concurrent conflicting edit *branches* (both heads are
  kept and surfaced) instead of silently overwriting - nothing is ever lost. A
  built-in `fsck` recomputes the chain, so tampering is detectable.
- **Automatic recall.** A per-prompt hook (the *courier*) searches memory for the
  current prompt and injects the top hits, so the agent starts each turn with the
  relevant context. Lexical bm25 (FTS5) by default; an optional semantic
  score-fusion layer improves recall on paraphrased questions (see below).
- **Guard.** A moment-of-action hook (`PreToolUse`) that emits an advisory when a
  tool call matches a risk rulebook (fail-open, never blocks).
- **Cross-machine sync.** Log-shipping (`thor ship` / `thor recv`) replicates the
  event log to another machine, verbatim and hash-identical.
- **Backup + restore.** `thor export` writes the log as canonical JSONL; `thor
  restore` replays it to an identical tip hash and verifies every recomputed hash.
- **Runs anywhere.** Local CLI + hooks, or a remote MCP server (Streamable-HTTP)
  behind an auth gate.

## Benchmarks

A blind, judged head-to-head against [mimir](https://github.com/MakerViking/mimir),
reported as two separate fair tests: **as-deployed coverage** (86% vs 27% on 500
questions, because THOR indexes repo code mimir's recall does not) and
**same-knowledge quality** (91% vs 75% on facts both have). Plus **multi-project
coverage** across three seeded repos (73% vs 53% overall - though mimir's curated
design docs win one project, 93% vs 67%), session-drift compensation (74% vs 59%), and
~3.1x lower latency. Full method, per-category tables and honest weaknesses in
[BENCHMARKS.md](BENCHMARKS.md).

## Quick start

```sh
cd thor
cargo test            # run the test suite
cargo build --release # build the binary (target/release/thor)
```

Install the hooks into your agent's settings (backs up first, only adds THOR
entries, idempotent). Full step-by-step, incl. project scoping: **[SETUP.md](SETUP.md)**.

```sh
thor install --with-courier          # auto-recall + SessionStart warm + project refresh/onboarding
thor install --with-guard            # + the moment-of-action guard
```

Use it:

```sh
thor remember "<a durable fact>"     # (via the MCP tool in an agent session)
thor ingest <repo-path>              # index a repo's tracked files (incremental)
thor recall "how does X work"        # search memory (scoped to the current project)
thor get <entity_id>                 # the authoritative head(s) for one fact
thor fsck                            # verify chain integrity
```

The courier runs automatically per prompt and injects a `<thor-recall>` block.

## Projects: index your repos, keep them isolated

THOR holds every project in one store but keeps them **isolated**: recall in project
A never surfaces project B's code or memories. Cross-cutting knowledge you mark
**global** (working rules, dev-loop, conventions) is the exception - it surfaces in
*every* project. The project is decided by the session's working directory (a `.thor`
marker, else the git repo name), exactly like the mimir convention.

```sh
thor init                       # set up the current project (writes .thor + indexes it)
thor ingest .                   # (re-)index the current repo (or a non-git folder), incrementally
thor ingest --project <key> <path>  # pin a canonical key (e.g. a NAS source folder named differently)
thor ingest --global <docs-dir> # hold cross-cutting docs in the @global tier (everywhere)
thor recall "how does X work"   # scoped to the current project + global
thor recall --all-projects "X"  # search every project
thor reproject <id> --project <key> | --global   # fix a fact's scope (sync-safe)
thor backfill-projects          # attribute legacy memories from their import footer (dry-run)
```

- Ingest is **incremental** (unchanged files skipped, changed re-chunked, deleted
  retracted). A **git repo** reads **tracked files only**, so gitignored secrets are
  never indexed; a **non-git folder** is walked directly (dotfiles like `.env`, heavy
  dirs, and any nested git repo skipped) - point it at docs, not at a tree with
  plaintext secrets in loose non-dot files.
- Chunk ids are `<project>:<path>#<n>`; scoped memories `<project>:mem-<uuid>`; global
  facts are unprefixed or under `@global:`. Recall (courier, CLI, MCP) scopes to the
  current project + the global tier by default.
- Wire `thor session-start` into your `SessionStart` hook: it refreshes a known project
  in the background, and for a new project it asks the agent to offer setup rather than
  indexing silently. Mis-scoped a fact? `thor reproject` moves it (it travels as an event,
  so a replica agrees after sync).

## Semantic recall (optional, off by default)

Lexical bm25 is the always-on default. A dense **score-fusion** layer adds
meaning-based retrieval so a paraphrased question still finds the right memory. It
is a compile-time feature, OFF by default, and degrades to bm25 whenever anything
is missing - it can never make recall worse.

```sh
cargo build --release --features semantic
```

- Put the embedding model files under `%LOCALAPPDATA%\thor\model\` (or point
  `thor vectors build --model-dir <dir>` at them). Any local ONNX sentence-
  embedding model with its tokenizer works; a multilingual MiniLM is a good
  default.
- Build the precomputed vector sidecar, then check it:
  ```sh
  thor vectors build      # embed every stored fact once
  thor vectors status
  ```
- Recall now fuses lexical and dense candidates: `fused = bm_norm + LAMBDA*cos`,
  with the bm25 leg min-max normalized per query. The per-prompt courier never
  pays the model load cost - a warm `thor embed-daemon` holds the model, and
  `thor warm` (safe to run at SessionStart) brings it up idempotently. The courier
  falls back to bm25 (and warms the daemon) if it is not up.
- `thor vectors sync` embeds only new facts (index maintenance).

The dense sidecar (`thor-vectors.db`) is derived and deletable: remove it and
recall silently returns to bm25.

## Sync (optional)

Replicate the log to another machine over the LAN/tailnet, bearer-token gated:

```sh
# on the replica:
THOR_TOKEN=<shared-token> thor recv --http 0.0.0.0:5555
# on the authority:
thor ship --to http://<replica>:5555 --token <shared-token> --watch
thor status --to http://<replica>:5555 --token <shared-token>
```

Keep the authority's `thor.db` on a **local disk** - it is never opened over a
network share (SQLite WAL requires real shared memory). Other machines get a
replica via ship/recv, never a shared network file.

## Deploy as a remote MCP server

`thor/deploy/` contains a `Dockerfile` and `docker-compose.yml` template. Run
`thor mcp --http 0.0.0.0:<port>` in the container, bind it to localhost/an
internal network, and front it with an authenticating reverse proxy (the
transport itself has no auth). Fill in the `<placeholder>` values in the compose
file for your own network and route.

## Command reference

| command | what |
|---|---|
| `thor remember` / `recall` / `get` / `history` | write / search / read facts |
| `thor ingest [<path>] [--global] [--project <key>] [--detach]` | index a folder's text files (incremental; git repo = tracked-only, plain folder = walked; `--global` = the `@global` tier; `--project` pins a key) |
| `thor init [<path>]` | set up a project: write a `.thor` marker + first ingest |
| `thor reproject <id> --project <key> \| --global` | reassign a fact's project scope (sync-safe) |
| `thor backfill-projects [--apply]` | attribute legacy memories from their import footer |
| `thor review-scope [--mark]` | list no-signal global memories to review (SessionStart nudges once/day) |
| `thor courier` / `thor session-start` | per-prompt recall hook / SessionStart refresh + setup cue |
| `thor warm` | pre-warm the semantic embedder (idempotent; for SessionStart) |
| `thor guard` / `thor stop-guard` | moment-of-action / response advisories |
| `thor install` | write the hooks into settings.json |
| `thor vectors build \| sync \| status` | semantic sidecar (feature `semantic`) |
| `thor embed-daemon` | warm embedder for the courier (feature `semantic`) |
| `thor export` / `restore` / `backup` | JSONL backup + verified restore |
| `thor ship` / `recv` / `status` | cross-machine log-shipping sync |
| `thor fsck` | verify chain integrity + FTS projection |
| `thor mcp [--http <bind>]` | run as an MCP server (stdio or Streamable-HTTP) |

## Build features

- default: pure lexical (bm25) - no ML runtime, no extra dependencies.
- `semantic`: adds the dense score-fusion recall layer (ONNX embedder, warm
  daemon, precomputed sidecar). Client-only; a server/deploy build can stay on the
  default and never pull the ONNX runtime.

## Layout

```
thor/
  src/            the Rust crate (event store, recall, ingest, guard, sync, mcp, courier)
  examples/       recall_eval.rs - measure recall over a query battery
  deploy/         Dockerfile + docker-compose template
  tools/          helper scripts (mimir export, side-by-side eval)
  *.example.json  guard rulebook templates (copy + fill in)
```

## License

GPLv3.
