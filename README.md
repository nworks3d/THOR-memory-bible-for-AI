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

- **It wins where knowledge lives.** On the 200-question as-deployed set THOR
  leads overall **77.0% vs 70.5%** (mimir v0.14), winning four of six
  categories - decision +20, doc-reference +11, config +9, code-behavior +8 -
  and tying gotcha. Its one category loss is code-structure (63.6% vs 74.2%),
  mimir's tree-sitter symbol graph.
- **It wins the cleanest equal-corpus tests.** On the strict dual-written cut
  (facts both stores verifiably hold, n=53) THOR leads **97.2% vs 94.3%**, and
  the broad shared cut too (88.8% vs 86.2%) - pure memory recall was mimir's
  home turf across earlier juries.
- **It compensates for session drift.** After a compaction the agent starts blank;
  THOR's as-deployed courier surfaces the governing gotcha/decision **86.3% vs
  74.0%** (mimir's best case) and fully catches it **58.9% vs 50.7%**, judged
  three-way blind. This is what the tool is *for*.
- **On a like-for-like full recall it is 2.7x faster than mimir** with the
  inject daemon up (120 ms vs mimir 0.14's 322 ms), injecting a full block on
  every prompt. With the daemon stopped it is 349 ms - slightly slower than
  mimir there, and it grows with store size. mimir's as-deployed hook is much
  faster (~34 ms) but serves a single floor-gated memory (175 chars) and nothing
  on 6 of 20 prompts - fast because it serves less. Full honest picture in
  BENCHMARKS.md.
- **It never loses a write.** Every fact is an event in a hash-chained append-only
  log; a conflicting edit *branches* (both heads kept) instead of overwriting, and
  `fsck` recomputes the chain so tampering is detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted, daemon
  down - each path falls back to bm25 and can never make recall worse.

Where mimir stays ahead: a **first-class code-symbol graph** for "which
functions call X" - THOR has a derived `where_used`/`impact` sidecar, but
mimir's graph is why it wins the code-structure category. See
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
  relevant context. Lexical bm25 (FTS5) is always on; a semantic score-fusion
  layer on top improves recall on paraphrased questions and is what you want on
  a client machine (see below).
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
re-measured fresh 2026-07-14 against **mimir v0.14.0** with a 3-judge median on
every test, fresh random blind maps and one-run-no-re-rolls, both stores
hygiene-passed before the run: **coverage** THOR 77.0% vs 70.5% on the
200-question balanced set (THOR wins four of six categories and loses only
code-structure, 63.6% vs 74.2%), **same-knowledge quality** THOR wins both
cuts (strict dual-written 97.2% vs 94.3%, broad shared 88.8% vs 86.2%),
**multi-project** THOR edges it 96.7% vs 95.6%, **session drift** THOR-led on
both metrics judged three-way blind (surfaces the preventing fact 86.3% vs
74.0%, full catch 58.9% vs 50.7%), and on **speed** (re-measured 2026-07-15 vs
mimir 0.14) a like-for-like full recall favours THOR with the daemon up (120 ms
vs 322 ms) but not without it (349 ms), while mimir's as-deployed hook is much
faster (~34 ms) by serving a single floor-gated memory and nothing on 6 of 20
prompts. Full method, per-category tables and honest weaknesses - including why
the code-structure loss is probably not the symbol-graph gap we first called it
- in [BENCHMARKS.md](BENCHMARKS.md).

Drift compensation is also measurable IN-REPO, no judge needed: `cargo run
--example drift_eval` replays a committed synthetic corpus
([eval/drift_scenarios.jsonl](thor/eval/drift_scenarios.jsonl), 52 scenarios:
46 should-fire + 6 must-stay-silent, EN/NL, distractors included) through the
REAL courier and guard hook paths and scores catches AND false fires (current
build: courier 76%, guard channel 16/16, either-channel 96%, noise 1 under a
one-way ratchet). `--live <corpus>` replays a private prompt set against your
live store read-only, scoring both entity-id and content presence.

## Quick start

Grab a prebuilt binary from [Releases](../../releases) - `windows-x86_64` or
`linux-x86_64` for the semantic client build, `linux-x86_64-bm25` for a
server/NAS (no ONNX). Each has a `.sha256`. On Windows the semantic build needs
the Microsoft Visual C++ Redistributable installed.

No embedding model ships with it: without one THOR runs pure bm25 and degrades
cleanly (see [Semantic recall](#semantic-recall-recommended-on-a-client)).

Or build it yourself:

```sh
cd thor
cargo test            # run the test suite
cargo build --release # build the binary (target/release/thor)
```

Install the hooks into your agent's settings (backs up first, only adds THOR
entries, idempotent). Full step-by-step, incl. project scoping: **[SETUP.md](SETUP.md)**.
Not sure what to switch on? **[OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md)** goes
through every optional piece one by one: what it buys you, what it costs, when to
leave it alone, and how to undo it.

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
thor fsck                            # verify chain integrity + footer health
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

## Semantic recall (recommended on a client)

Lexical bm25 is always on. A dense **score-fusion** layer adds meaning-based
retrieval on top, so a paraphrased question still finds the right memory. Turn it
on unless you have one of the reasons below: it degrades to bm25 whenever
anything is missing, so it can never make recall worse.

The **release binaries for Windows and Linux are already built with it** - you
only need to supply a model (below). If you build from source, add the feature:

```sh
cargo build --release --features semantic
```

**When to leave it off**, and these are the only reasons:

- **Servers, containers, the NAS.** The default build is bm25-only and pulls no
  ONNX at all; that is what `thor-linux-x86_64-bm25.tar.gz` is for. A remote
  store does not run the courier anyway.
- **Not enough RAM.** Fast semantic recall wants a warm `thor embed-daemon`
  holding the model resident (~650 MB). Without the daemon the courier still
  works - it just falls back to bm25 rather than pay a cold model load on your
  prompt.
- **You have no model and do not want to fetch one** (~235 MB, see below).

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

### Cross-encoder rerank (deliberately per-call, NOT a default)

Unlike the semantic layer, this one is opt-in for a real reason: **it is not
strictly better.** Measured, it wins on paraphrase-heavy questions and *loses*
on exact lookups (numbers below). So it is a second try when the normal order
looks wrong, not something to switch on and forget.

A cross-encoder scores each (query, hit) pair through a full transformer pass -
much better paraphrase ordering than vector cosines, but one forward pass per
document (~1s median for a 12-hit pool on CPU), so it never runs by default and
never touches the per-prompt courier. MCP recall takes `rerank: true`, the CLI
takes `thor recall --rerank`.

- Put a reranker model (ONNX + tokenizer, five files, onnx named `model.onnx`)
  under `%LOCALAPPDATA%\thor\reranker\`; a multilingual base reranker is a good
  default. Nothing auto-downloads.
- Contract mirrors the semantic layer: model missing or any failure = the
  normal order is returned with an explicit note, never an error.
- Measured on a 53-question same-knowledge set (gold-term coverage): top-1
  +3pp with 16 wins / 7 losses, top-3 flat, top-5 slightly negative - and
  exact-lookup questions (doc references) can get WORSE while paraphrase-heavy
  ones improve. That trade-off is WHY it is opt-in rather than default.

## Sync (only if you have a second machine)

Replicate the log to another machine over the LAN/tailnet, bearer-token gated.
Nothing to turn on if you work on one machine; this exists for a laptop plus a
desktop, or a NAS holding a replica:

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
| `thor fsck` | verify chain integrity + FTS projection, and report facts whose footer got lost (content health: it names them and never fails the run) |
| `thor consolidate [--apply-dedup]` | metabolism report: duplicate twins, decay candidates, same-topic clusters (exit 1 when anything needs digesting; only the dedup pass is ever applied mechanically) |
| `thor steward` | prepare a stewardship review: the consolidate report + the proven conservative rubric written to a file an agent session works through with the MCP tools (no writes itself) |
| `thor symbols` | (re)build the derived symbol sidecar (`thor-symbols.db`): which names every code chunk defines and calls - powers `where_used`/`impact` and a deliberate-recall ranking bonus; refreshed automatically after `thor ingest`, but **not** after `thor init` (run it once yourself, or ingest again); safe to delete and rebuild |
| `thor daemon` / `thor ensure-daemon` | warm injection daemon: `/inject` + `/health` on the HTTP server, discovered via a flag file; the courier answers warm and falls back cold on any failure. **Recommended** - it holds the folded log + vector matrix resident, which is ~60% of per-prompt latency (349 -> 120 ms measured). Expect a few hundred MB of RAM; the repo has no measurement of this daemon's own footprint (the measured ~650 MB below is the *embedder* daemon). It is the same server as `thor mcp --http`, so the full MCP toolset - writes included - is mounted on that port with no auth: keep the bind on loopback. Wire it in with `thor install --with-daemon` (`ensure-daemon` is the SessionStart form) |
| `thor doctor` | one-line health per surface: store, semantic model + sidecars, injection daemon warm/cold, flags |
| `thor pre-compact` | PreCompact hook: one advisory per session, right before a compaction, to persist durable decisions via remember (installed by `--with-courier`) |
| `thor recall --rerank` | rescore the top hits with the local cross-encoder (feature `semantic` + downloaded reranker model; MCP recall takes `rerank: true`) |
| `thor mcp [--http <bind>]` | run as an MCP server (stdio or Streamable-HTTP) exposing the full stewardship toolset: recall (`kind:"memory"` filter, `detail:"index"` for a compact id list) / get / history / remember (typed, duplicate-refusing, optional `expires: YYYY-MM-DD` after which a fact stops surfacing - history keeps it) / revise / retract / resolve / mark / pin / unpin / reproject / brief / outline (a file's signature map) / where_used / impact (symbol callers + change blast-radius on the derived sidecar) |

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

## Support this project

THOR is free and GPLv3, built by [N-Works 3D](https://www.youtube.com/@NoizieWorks).
If it has earned its keep - saved you a re-explanation, caught a drift before it
cost you, or just kept a session from starting cold - and you'd like to help keep
it moving, there are two easy ways:

- **PayPal**: https://www.paypal.com/paypalme/ognoizieworks
- **YouTube members**: https://www.youtube.com/@NoizieWorks/join

No pressure and no paywall - the whole thing stays open either way. Skål, and
thanks for reading this far.

## Contributing

Bug reports and PRs welcome. THOR is a memory an agent is supposed to trust, so
the bar is correctness and honest measurement rather than volume of features -
the checklist before you commit, and the ways measuring THOR goes wrong, are in
**[CONTRIBUTING.md](CONTRIBUTING.md)**. Maintainer release procedure:
[RELEASING.md](RELEASING.md).

## License

GPLv3.
