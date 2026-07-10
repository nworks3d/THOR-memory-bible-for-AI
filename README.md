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
  answered without the agent doing anything. As deployed, **68.5% vs 59.8%** on a
  200-question balanced set - mimir wins only the code-structure category, on the
  strength of its own new code-content indexing.
- **It ranks better on the broad shared set** *(not re-measured this round;
  previous round's result)*: on facts both systems have, THOR led **61% vs 54%**
  thanks to a dense score-fusion layer that catches paraphrases keyword search
  misses - though on the strictest dual-written-only cut mimir won (94% vs 92%),
  pure memory recall being its home turf.
- **It compensates for session drift.** After a compaction the agent starts blank;
  THOR puts the governing gotcha/decision back in front of it more often than mimir
  at its best (surfaced **70% vs 51%**, full catch **51% vs 40%**). This is what
  the tool is *for*.
- **It is faster than mimir's default path.** ~**2.3x** lower latency than mimir's
  as-deployed cold hook (253 ms vs 589.5 ms) - though mimir's opt-in warm daemon is
  faster still (62 ms, at lower coverage: 5 of 20 prompts get an empty injection),
  and THOR injects more tokens than mimir's cold path, not fewer. Full honest
  picture in BENCHMARKS.md.
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
  The courier never repeats itself (a per-session ledger suppresses recently-shown
  hits and rotates deeper ones in), stays silent instead of injecting weak
  one-word coincidences, and re-reads a chunk's file live so changed code is
  injected `[refreshed]` (or flagged `[stale?]`) instead of as a stale snapshot.
  Ranking is query-routed: a knowledge-phrased question ("what did we decide
  about X") gives hand-written facts a small prior over the wall of same-topic
  code chunks, hits matching the WHOLE question tightly outrank one-word tf
  spam, and slot 3 is reserved for a close-ranked typed constraint
  (gotcha/decision/preference) when none made the top - while code-phrased
  queries get none of this, so code ranking stays untouched. A fact stored
  with `triggers` ("when should this fire?" - commands, file names, error
  strings) carries a `fires-when` footer field: a prompt hitting those words
  gives the fact a bounded boost, and it may compete from below the relevance
  floor - the author declared exactly this moment. Facts without the field
  rank exactly as before, by construction. Hook/debounce
  state lives in one SQLite sidecar (`thor-ledger.db`), so parallel hooks and
  sessions never lose each other's entries.
- **Drift hooks.** Pin standing rules (`thor pin`) and SessionStart re-injects
  their full body at every start - including right after a compaction, when
  prompt-recall has nothing to match against. The first time a session touches a
  file, the guard surfaces stored memories that *name* that file (memories only,
  never code chunks). A Stop-hook capture nudge fires (once per session, claimed
  atomically) when a reply contains an unstored decision/gotcha, so durable facts
  stop depending on the model remembering to remember; its trigger list is
  tunable via `guard-capture-triggers.json` next to the store (built-in list as
  fail-open fallback, like the guard rulebooks).
- **Agent stewardship.** Over MCP the agent can maintain the memory, not just
  fill it: `revise`/`retract` with real CAS (a stale parent returns the fresh
  head-set instead of minting a silent branch), `resolve` for DIVERGED facts,
  `mark` ("this helped" - feeds the ranking prior), typed `remember` whose
  duplicate/exists refusal is atomic with the write, `reproject`, and a `brief`
  overview of what THOR knows here. MCP `recall` runs the same semantic
  score-fusion path the courier uses (fused parity), and every read surface
  (MCP/CLI recall and `get`) carries the `[refreshed]`/`[stale?]` freshness tags.
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
re-measured fresh on 2026-07-11 (second full round of that day) with a 3-judge
median on every test and freshly salted blind maps, after two same-day THOR
improvement rounds, against mimir's strongest opponent build to date
(unreleased main, its own code-content-indexing round, unchanged between the
two rounds) - wins and losses on a level playing field: **coverage** 68.5% vs
59.8% on a 200-question balanced set (both systems scored lower than the
earlier same-day round - jury strictness varies even with a 3-judge median;
mimir wins one category, code-structure, on its new code-content indexing),
**same-knowledge quality** not re-measured this round (previous round: THOR
61.4% vs mimir 53.8% overall; mimir kept the strict dual-written cut, 94.3%
vs 91.5%), **multi-project** mimir leads outright, 98.9% vs 92.2%, after its
code-content indexing erased what used to be THOR's biggest structural edge,
**session drift** THOR-led on both metrics (surfaces the preventing fact
69.9% vs 50.7% as deployed, full catch 50.7% vs 39.7%, and 72.5% of courier
surfacings are now full catches, up from 64.7%), and on speed THOR is ~2.3x
faster than mimir's as-deployed cold path (253 ms vs 589.5 ms) but slower
than mimir's opt-in warm daemon (62 ms, which however served nothing on 5 of
the 20 canonical prompts), while injecting more tokens than mimir's cold path
(679 vs 236) - the old "1.5x faster / 2.1x fewer tokens" headline stays
retired. Full method, per-category tables and honest weaknesses in
[BENCHMARKS.md](BENCHMARKS.md).

Drift compensation is also measurable IN-REPO, no judge needed: `cargo run
--example drift_eval` replays a committed synthetic corpus
([eval/drift_scenarios.jsonl](thor/eval/drift_scenarios.jsonl), 43 scenarios,
EN/NL, distractors included) through the REAL courier and guard hook paths and
scores whether the mistake-preventing fact actually surfaces (current build:
courier 74%, guard channel 16/16, either-channel 95%). `--live <corpus>` replays
a private prompt set against your live store read-only.

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

### Cross-encoder rerank (optional, opt-in per call)

A cross-encoder scores each (query, hit) pair through a full transformer pass -
much better paraphrase ordering than vector cosines, but one forward pass per
document (~1s median for a 12-hit pool on CPU), so it NEVER runs by default and
never touches the per-prompt courier. Use it as a deliberate second try when
the normal order looks wrong: MCP recall takes `rerank: true`, the CLI takes
`thor recall --rerank`.

- Put a reranker model (ONNX + tokenizer, five files, onnx named `model.onnx`)
  under `%LOCALAPPDATA%\thor\reranker\`; a multilingual base reranker is a good
  default. Nothing auto-downloads.
- Contract mirrors the semantic layer: model missing or any failure = the
  normal order is returned with an explicit note, never an error.
- Measured on a 53-question same-knowledge set (gold-term coverage): top-1
  +3pp with 16 wins / 7 losses, top-3 flat, top-5 slightly negative - and
  exact-lookup questions (doc references) can get WORSE while paraphrase-heavy
  ones improve. That trade-off is WHY it is opt-in rather than default.

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

For sudo-less redeploys from your workstation, `deploy/deploy-watcher.sh` is a
root scheduled-task template (Synology-tested): copy a `git archive` tarball of
the crate over ssh, touch a trigger file, and the watcher unpacks + rebuilds +
restarts the container on its next tick, logging to `deploy.log`. It never
overwrites your live compose file and never touches the data volume.

## Command reference

| command | what |
|---|---|
| `thor remember` / `recall` / `get` / `history` | write / search / read facts |
| `thor ingest [<path>] [--global] [--project <key>] [--detach]` | index a folder's text files (incremental; git repo = tracked-only, plain folder = walked; `--global` = the `@global` tier; `--project` pins a key) |
| `thor init [<path>]` | set up a project: write a `.thor` marker + first ingest |
| `thor reproject <id> --project <key> \| --global` | reassign a fact's project scope (sync-safe) |
| `thor backfill-projects [--apply]` | attribute legacy memories from their import footer |
| `thor review-scope [--mark]` | list no-signal global memories to review (SessionStart nudges once/day) |
| `thor courier` / `thor session-start` | per-prompt recall hook (session-dedup, noise gate, live-file freshness) / SessionStart refresh + pinned `<thor-brief>` + setup cue |
| `thor pin <id> \| --list` / `thor unpin <id>` | pin standing rules: their full body re-injects at every session start and right after a compaction |
| `thor mark <id> [--noise]` | record that a fact actually helped - or was noise here (local; one unified usage strength feeds the courier's promotion and consolidate's decay) |
| `thor warm` | pre-warm the semantic embedder (idempotent; for SessionStart) |
| `thor guard` / `thor stop-guard` | moment-of-action advisories (risk rulebook + first-touch file memories) / response advisories + a once-per-session capture nudge for unstored decisions/gotchas |
| `thor install` | write the hooks into settings.json |
| `thor vectors build \| sync \| status` | semantic sidecar (feature `semantic`) |
| `thor embed-daemon` | warm embedder for the courier (feature `semantic`) |
| `thor export` / `restore` / `backup` | JSONL backup + verified restore |
| `thor ship` / `recv` / `status` | cross-machine log-shipping sync |
| `thor fsck` | verify chain integrity + FTS projection |
| `thor consolidate [--apply-dedup]` | metabolism report: duplicate twins, decay candidates, same-topic clusters (exit 1 when anything needs digesting; only the dedup pass is ever applied mechanically) |
| `thor recall --rerank` | rescore the top hits with the local cross-encoder (feature `semantic` + downloaded reranker model; MCP recall takes `rerank: true`) |
| `thor mcp [--http <bind>]` | run as an MCP server (stdio or Streamable-HTTP) exposing the full stewardship toolset: recall (`kind:"memory"` filter) / get / history / remember (typed, duplicate-refusing) / revise / retract / resolve / mark / pin / unpin / reproject / brief |

## Build features

- default: pure lexical (bm25) - no ML runtime, no extra dependencies.
- `semantic`: adds the dense score-fusion recall layer (ONNX embedder, warm
  daemon, precomputed sidecar). Client-only; a server/deploy build can stay on the
  default and never pull the ONNX runtime.

## Layout

```
thor/
  src/            the Rust crate (event store, recall, ingest, guard, sync, mcp, courier)
  examples/       recall_eval.rs (recall battery) + drift_eval.rs (drift compensation)
  eval/           drift_scenarios.jsonl - the committed synthetic drift corpus
  deploy/         Dockerfile + docker-compose template + deploy-watcher.sh
  tools/          helper scripts (mimir export, side-by-side eval)
  *.example.json  guard rulebook templates (copy + fill in)
```

## Acknowledgments

- **MakerViking** - for the inspiration and the great fight. This project would
  not exist without the spark, and it would not be half as good without a worthy
  rival to measure against. Skål!
- **mimir** ([MakerViking/mimir](https://github.com/MakerViking/mimir)) - the
  wise opponent in every benchmark in this repo. In the sagas, Mimir guards the
  well of knowledge; here it set the bar THOR had to clear. The scoreboard
  shows real wins and real losses on purpose: a rival this good deserves honest
  numbers.
- **Idea credit, both directions.** Two THOR mechanisms are idea adoptions from
  mimir's own improvement rounds, reimplemented here in THOR's idiom: the
  identifier/path-aware matching in recall (mimir's identifier RRF leg) and the
  eval discipline that scores the injection DECISION as a confusion table with
  a one-way "injected-wrong must never rise" ratchet. Mimir in turn credits
  THOR for code-content indexing and per-prompt auto-recall - exactly the kind
  of exchange open source is for. Thanks, MakerViking.

## License

GPLv3.
