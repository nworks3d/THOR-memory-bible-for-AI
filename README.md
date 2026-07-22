![The AI Memory Bible](assets/banner.png)

# THOR - one lossless, local memory for AI coding agents

THOR is a from-scratch persistent memory for AI coding agents (such as Claude
Code). It ingests your **repositories, docs and decisions** into a single local
index and gives the relevant pieces back **automatically, at the right moment** -
so a session never starts from zero, even right after a compaction. It is a single
Rust binary: no external services, no git required, and it never loses a write.

> **Are you an AI agent setting THOR up for your user?** Start with
> **[AGENTS.md](AGENTS.md)** - one page: how to walk your user through setup,
> and the six rules of working with the memory correctly.

![THOR vs mimir - coverage, quality, multi-project, drift and speed](assets/benchmark.svg?v=20260722)

## Why THOR

THOR's thesis is to replace *both* the repo knowledge *and* the memory tool with
one thing the agent can search automatically. Measured against
[mimir](https://github.com/MakerViking/mimir) on the same machine
([full method + weaknesses](BENCHMARKS.md)):

- **On retrieval quality it is a tie with a THOR edge that is not significant,
  and that is the honest headline.** Over the 200-question as-deployed set
  (2026-07-22 round, THOR v0.9.6 vs mimir v0.15.0) THOR scores **89.2% vs
  84.6%**, 28 W / 17 L, p = 0.14 - published as a tie. Same-knowledge cuts:
  97.2% vs 95.8% and 89.1% vs 89.2%, ties. Multi-project: 91.1% vs 94.4%,
  mimir nominally ahead, not significant. Claims retracted in earlier rounds
  stay retracted - see BENCHMARKS.md.
- **The code-structure gap is closed.** The previous round's one significant
  mimir win (57.6% vs 74.2%, p = 0.013) reads **72.6% vs 72.6%** (8 W / 8 L)
  now that recall serves structure cards (v0.9.6). A tie, not a win - and the
  battery and jury changed across rounds, so it is a within-round reading.
- **On drift, THOR's as-deployed channel now beats mimir's best explicit
  channel, significantly.** After a compaction the agent starts blank; over 59
  fresh-session scenarios THOR's session channel (courier + guard advisories,
  what actually runs) surfaces the governing fact at **79.7%** against
  **64.4%** for mimir's full recall - which you must call explicitly - and
  wins 11 scenarios to 0 (p = 0.001); the courier alone wins 10 to 2
  (p = 0.039). This reverses the previous round's finding, after the
  serving-form work in between; the cross-round caveats (cleaned corpus, new
  jury) are stated next to the claim in BENCHMARKS.md. mimir's own hook misses
  50 of 59 scenarios.
- **What runs unasked stays the core claim.** THOR's courier answers **every**
  prompt in 146 ms and never stays silent. mimir's hook is faster (40 ms) but
  empty on 6 of 20 prompts; its full recall costs 323 ms and is not a hook.
  Honest note: the courier was 125 ms at 16.1k events and is 146 ms at 19.8k -
  that growth is real. The per-query fold behind it is materialized since
  2026-07-22 (cold paths measured about 4x faster, guard advisory 499 -> 211 ms,
  served bytes identical); the warm-courier figure stands until re-measured.
  Full picture, including retracted
  claims and two discarded scoring passes of our own, in BENCHMARKS.md.
- **It never loses a write.** Every fact is an event in a hash-chained append-only
  log; a conflicting edit *branches* (both heads kept) instead of overwriting, and
  `fsck` recomputes the chain so tampering is detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted, daemon
  down - each path falls back to bm25, so a broken setup costs you the extra
  layer and nothing else.

Where mimir stays ahead structurally: a **first-class code-symbol graph** for
"which functions call X" - THOR's `where_used`/`impact` sidecar is derived,
not first-class. The judged code-structure category itself is a tie this
round. See [BENCHMARKS.md](BENCHMARKS.md) for the honest trade-offs.

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
  file, the guard surfaces stored memories that *name* that file, plus up to
  three prose doc chunks (CHANGELOG/design-doc paragraphs) that name it - never
  code chunks, and never a chunk of the touched file itself. A Stop-hook capture nudge fires (once per session, claimed
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
re-measured fresh 2026-07-22 (THOR v0.9.6 vs **mimir v0.15.0**), pre-registered
before scoring, with a 3-judge median (three sonnet lenses) on every test,
seeded blind arm order, one run and no re-rolls, both stores hygiene-passed
first and mimir's own indexer re-run over the same repositories the same day.
One documented amendment: the first pass judged mimir's deliberate arms in
their one-line-summary form; they were regenerated full-body (`--full`) and
re-judged before publication - the same error class, in the other direction,
as a run this project discarded the round before.

**Coverage is a tie with a non-significant THOR edge** (89.2% vs 84.6%,
p = 0.14); both same-knowledge cuts are ties; multi-project has mimir
nominally ahead (91.1% vs 94.4%, not significant). The previous round's one
significant mimir win, code-structure, is closed to an exact tie (72.6% vs
72.6%). On drift (59 cleaned scenarios) THOR's as-deployed session channel
beats mimir's best explicit channel 79.7% vs 64.4%, 11 W / 0 L, p = 0.001 -
reversing the previous round - and mimir's own hook misses 50 of 59. Speed:
THOR's courier 146 ms and never empty; mimir's hook 40 ms but empty on 6 of
20; mimir's full recall 323 ms.

Claims retracted in earlier rounds stay **retracted** there, alongside two
discarded scoring passes - one that shortchanged mimir, one that shortchanged
THOR - both on the record. Full method, per-category tables, significance
tests and honest weaknesses in [BENCHMARKS.md](BENCHMARKS.md).

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
New here? **[FEATURES.md](FEATURES.md)** explains in plain words what each part
does and whether it is worth your time - read that first. When you have decided,
**[OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md)** has the exact commands, the real
costs, and how to undo every one of them.

The flags combine; the Stop response guard is installed whatever flags you pass.

```sh
thor install                                             # the Stop response guard only
thor install --with-courier                              # + auto-recall, SessionStart warm, project refresh/onboarding, the pre-compact nudge
thor install --with-guard                                # + the moment-of-action guard
thor install --with-daemon                               # + the warm injection daemon (recommended, see below)
thor install --with-courier --with-guard --with-daemon   # the full setup on the machine your agent works on
```

Use it:

```sh
thor remember "<a durable fact>"     # (via the MCP tool in an agent session)
thor ingest <repo-path>              # index a repo's tracked files (incremental)
thor recall "how does X work"        # search memory (scoped to the current project)
thor get <entity_id>                 # the authoritative head(s) for one fact
thor fsck                            # verify integrity (exits 1 on damage) + footer health
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
- `thor install --with-courier` wires `thor session-start` into your `SessionStart`
  hook. No other flag installs that particular entry, so without `--with-courier` you
  add it by hand (other flags do write their own SessionStart entries - `--with-daemon`
  and `--backup-repo` - they just do not write this one). It refreshes a known project
  in the background, and for a **git** project you have not set up yet it asks the
  agent to offer setup rather than indexing silently; a plain non-git folder gets no
  cue and no index. Mis-scoped a fact? `thor reproject` moves it (it travels as an
  event, so a replica agrees after sync).

## Semantic recall (recommended on a client)

Lexical bm25 is always on. A dense **score-fusion** layer adds meaning-based
retrieval on top, so a paraphrased question still finds the right memory. Turn it
on unless you have one of the reasons below. If the model, the sidecar or the
daemon is missing it falls back to plain bm25, so a broken setup costs you the
feature and nothing else.

**What it is measured to buy, and where.** On 53 hand-written memory facts - the
thing THOR exists to recall - it moves the right fact from a mean rank of 4.6 to
2.5, with 14 facts moving up and 4 moving down. Every one of those four drops is
exactly one place (rank 1 to rank 2), while the gains include a fact rescued from
rank 50 to rank 8. Paired Wilcoxon p = 0.006; the cruder sign test, which ignores
how far each fact moved, gives 0.03.

Two honest limits on that. On indexed **repo code chunks** it is a wash: 84 golds
up, 89 down at the shipped weight, and turning the dense weight up to 3.0 makes it
measurably worse (p = 0.004). And the win is invisible to a hit@5 score, because
bm25 already puts 46 of those 53 facts in the top five - the fusion layer mostly
reorders inside the set the agent already reads, which is why it is measured by
rank rather than by a hit rate. Numbers from
`cargo run --release --features semantic --example recall_eval`; the corpus is
private, so they are not reproducible from this repo alone.

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

- Put the embedding model files in `model/` inside THOR's per-user home:
  `%LOCALAPPDATA%\thor\model\` on Windows, `$XDG_DATA_HOME/thor/model/` or
  `$HOME/.local/share/thor/model/` elsewhere - the same home the store uses.
  (`thor vectors build --model-dir <dir>` overrides it for that one command; the
  courier and the daemon always read the default.) Any local ONNX sentence-
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
  under `reranker/` in the same per-user home as the model (`%LOCALAPPDATA%\thor\reranker\`
  on Windows, `$HOME/.local/share/thor/reranker/` elsewhere); a multilingual base reranker is a good
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

Keep the authority's `thor.db` on a **local disk**. SQLite WAL requires real
shared memory, so on Windows `thor` refuses to open a store over a UNC path; on
Linux and macOS there is no such check, so avoiding an NFS or SMB mount is up to
you. Other machines get a replica via ship/recv, never a shared network file.

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
| `thor fsck` | verify chain integrity, FTS projection and FTS index structure - exits 1 on any of them, so a cron job or release step can gate on it. Repair a damaged index with `thor fsck --rebuild-fts` (derived from the log; nothing is lost). Also reports facts whose footer got lost (content health: it names them and never fails the run) |
| `thor consolidate [--apply-dedup]` | metabolism report: duplicate twins, decay candidates, same-topic clusters (exit 1 when anything needs digesting; only the dedup pass is ever applied mechanically) |
| `thor steward` | prepare a stewardship review: the consolidate report + the proven conservative rubric written to a file an agent session works through with the MCP tools (no writes itself) |
| `thor symbols` | (re)build the derived symbol sidecar (`thor-symbols.db`): which names every code chunk defines and calls - powers `where_used`/`impact` and a deliberate-recall ranking bonus; refreshed automatically by every ingest, including the one `thor init` runs, so you only need this command by hand for a store that was filled some other way (a shipped replica), or after deleting the sidecar |
| `thor daemon` / `thor ensure-daemon` | warm injection daemon: `/inject` + `/health` on the HTTP server, discovered via a flag file; the courier answers warm and falls back cold on any failure. **Recommended** - it holds the folded log + vector matrix resident, which is ~60% of per-prompt latency (349 -> 120 ms measured). Expect a few hundred MB of RAM; the repo has no measurement of this daemon's own footprint (the measured ~650 MB below is the *embedder* daemon). It is the same server as `thor mcp --http`, so the full MCP toolset - writes included - is mounted on that port with no auth: keep the bind on loopback. Wire it in with `thor install --with-daemon` (`ensure-daemon` is the SessionStart form) |
| `thor doctor` | one-line health per surface: store, semantic model + sidecars, injection daemon warm/cold, flags |
| `thor pre-compact` | PreCompact hook: one advisory per session, right before a compaction, to persist durable decisions via remember (installed by `--with-courier`) |
| `thor recall --rerank` | rescore the top hits with the local cross-encoder (feature `semantic` + downloaded reranker model; MCP recall takes `rerank: true`) |
| `thor mcp [--http <bind>]` | run as an MCP server (stdio or Streamable-HTTP) exposing the full stewardship toolset: recall (`kind:"memory"` filter, `detail:"index"` for a compact id list) / get / history / remember (typed, duplicate-refusing, optional `expires: YYYY-MM-DD` after which a fact stops surfacing - history keeps it; a later revise that carries no footer of its own keeps that date, and says so in its reply) / revise / retract / resolve / mark / pin / unpin / reproject / brief / outline (a file's signature map) / where_used / impact (symbol callers + change blast-radius on the derived sidecar) |

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
