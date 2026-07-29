# THOR reference

The full tour, moved here from the front page to keep that page readable. This
is the depth: what every part does and how it behaves, project scoping, the
semantic layer, sync, deployment, the complete command table, build features
and the repo layout. If you are new, read [FEATURES.md](FEATURES.md) first - it
explains the same parts in plain words and tells you which are worth your time.

- [What it does](#what-it-does)
- [Projects: index your repos, keep them isolated](#projects-index-your-repos-keep-them-isolated)
- [Semantic recall](#semantic-recall-recommended-on-a-client)
- [Cross-encoder rerank](#cross-encoder-rerank-deliberately-per-call-not-a-default)
- [Sync](#sync-only-if-you-have-a-second-machine)
- [Deploy as a remote MCP server](#deploy-as-a-remote-mcp-server)
- [Measuring it yourself](#measuring-it-yourself)
- [Command reference](#command-reference)
- [Build features](#build-features)
- [Layout](#layout)

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
  It also never re-serves what another layer already carries: a PINNED fact (the
  session-start `<thor-brief>` carries the whole pin list) and a fact tagged
  `guarded` (its author declares it has a gate of its own - a guard anchor that
  fires on the command it governs, or a response-rulebook rule that holds the
  reply) move to the back of the pool, so a fact you have not been shown takes
  the slot and the demoted one still surfaces when nothing else matched
  (`THOR-NO-PIN-DEDUP.flag` restores the old behaviour for both). And a fact tagged
  `wegwijzer` - a scope pointer, whose content is "this domain lives in project
  X, recall there" - is looked up by that tag on every prompt rather than left to
  ranking (a pointer holds no domain content, so it can never pass the relevance
  gate on its own) and appended as at most two `Scope hint:` lines that take no
  content slot (`THOR-NO-SCOPE-HINT.flag` switches it off). The MCP `recall` tool
  appends the same hints, including on its no-hits answer - a chat on the hosted
  connector has no courier at all, and that is exactly where a scoped fact is
  otherwise unreachable. Deliberate recall also says when it is guessing: if
  EVERY hit in an answer is a weak match (no all-terms match, no declared
  trigger, under half the question's terms covered) it appends one `Weak match:`
  line pointing at the scope hint / `all_projects` / asking the user
  (`THOR-NO-WEAK-NOTE.flag` removes it). It warns rather than filters on
  measurement: dropping weak hits cost 15 of 187 known answers on the
  460-question battery, while the warning fires on 1.3% of it and on 0.5% of the
  questions whose answer was present.
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
  code chunks, and never a chunk of the touched file itself. "Names it" means the
  full file name or its stem, with two measured refinements (2026-07-25, over 39
  real files): a doc chunk is
  never admitted on a *defined symbol* alone - 11 of 14 such admissions were a
  chunk sharing a generic identifier ("access", "current", "existing") from an
  unrelated paragraph - and a stem that only names a file's ROLE (`main`, `lib`,
  `mod`, `test`, `index`, `util`) is not evidence either, since touching
  `main.rs` otherwise pulled in paragraphs about the git branch. An underscored
  stem is additionally searched as words, because prose says "the event store"
  where the code says `event_store`. The memory-fact lane keeps its symbol
  bridge: there the symbol is the point (a gotcha about a function that never
  names its file). A Stop-hook capture nudge fires (once per session, claimed
  atomically) when a reply contains an unstored decision/gotcha, so durable facts
  stop depending on the model remembering to remember; its trigger list is
  tunable via `guard-capture-triggers.json` next to the store (built-in list as
  fail-open fallback, like the guard rulebooks).
- **Agent stewardship.** Over MCP the agent can maintain the memory, not just
  fill it: `revise`/`retract` with real CAS (a stale parent returns the fresh
  head-set instead of minting a silent branch), `resolve` for DIVERGED facts,
  `mark` ("this helped" - feeds the ranking prior), typed `remember` whose
  duplicate/exists refusal is atomic with the write, `reproject`, and a `brief`
  overview of what THOR knows here. `revise` supports body surgery next to the
  metadata surgery: `append` adds a dated status paragraph under the current
  content, `replace_from`/`replace_to` does one exact, unique in-place edit
  (zero or multiple matches are rejected, never silently applied) - a one-line
  status change never means retyping a long fact. `mark` and `get` resolve a
  bare, differently-prefixed or truncated id when it matches exactly one live
  entity (ambiguity never resolves), and `mark` rejects unknown parameter
  names outright - a misspelled option must never silently invert a judgment.
  A fact stored global, and a repeat noise mark on a global fact, both come
  back with a scope hint: cross-cutting rules belong global, domain knowledge
  belongs in a project scope. A write-time anchor has a floor too: `remember`
  and `revise` refuse a bare tool name (`git`), a bare role-file (`main.rs`),
  and anchors that could never match anything real - a glob, a `...`
  truncation, a `<ref>:` prefix, or two file names glued by a slash - because
  the guard compares an anchor verbatim against touched paths, and a dead
  anchor still counts toward anchor coverage without ever gating anything.
  And a ceiling to match that floor: when a write leaves the fact carrying an
  anchor whose target already holds more live facts than the guard can serve
  for it, the reply says so and names the target. It warns, never refuses -
  the write already happened, and which anchor to drop is a judgement call.
  Without it the surplus piles up unseen, since the guard drops it silently
  and `thor consolidate` only reports it after the fact.
  MCP `recall` runs the same semantic score-fusion path the courier uses
  (fused parity), and every read surface (MCP/CLI recall and `get`) carries
  the `[refreshed]`/`[stale?]` freshness tags.
- **Guard.** A moment-of-action hook (`PreToolUse`) that emits an advisory when a
  tool call matches a risk rulebook (fail-open, never blocks).
- **Cross-machine sync.** Log-shipping (`thor ship` / `thor recv`) replicates the
  event log to another machine, verbatim and hash-identical.
- **Backup + restore.** `thor export` writes the log as canonical JSONL; `thor
  restore` replays it to an identical tip hash and verifies every recomputed hash.
- **Runs anywhere.** Local CLI + hooks, or a remote MCP server (Streamable-HTTP)
  behind an auth gate.

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
  in the background, and for any folder you have not set up yet - a git repo or a
  plain non-git folder - it asks the agent to offer `thor init` rather than indexing
  silently (for a plain folder the cue also flags that notes would otherwise land in
  the global tier, and that Claude Code must restart after `thor init` before the
  tools see the new project). Nothing is indexed without your OK. Mis-scoped a fact? `thor reproject` moves it (it travels as an
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
rank rather than by a hit rate. Those numbers came from a private corpus, so they
are not reproducible from this repo - measure your own store if the question
matters to you.

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

Writing FROM a replica (a phone whose only endpoint is the container) goes
through the capture inbox, never the replica's log.

## Deploy as a remote MCP server

`thor/deploy/` contains a `Dockerfile` and `docker-compose.yml` template. Run
`thor mcp --http 0.0.0.0:<port>` in the container, bind it to localhost/an
internal network, and front it with an authenticating reverse proxy (the
transport itself has no auth). Fill in the `<placeholder>` values in the compose
file for your own network and route.

## Measuring it yourself

This repository publishes no scores and ships no corpus. The reason is in
[CONTRIBUTING.md](../CONTRIBUTING.md): a corpus written alongside the mechanism
it validates measures its author, not the tool.

If you want a number for your own store, the shape that works is a **drift
test**, because drift is what this tool exists to fix. Write down a handful of
tasks you have actually given an agent, and for each one the fact that should
have stopped it going wrong. Start a fresh session, give it the task, and check
whether the rule arrived before the mistake. Score two things, not one: the
catches, and the times something irrelevant was injected. A memory that surfaces
everything scores well on catches and is useless to work with.

Do that before and after a change and you have a real answer about your store,
which is the only store whose behaviour you care about.

## Command reference

| command | what |
|---|---|
| `thor recall` / `get` / `history` | search / read facts from the shell |
| `thor create <id> "<body>"` / `revise` | write from the shell - you choose the id yourself. The `remember` tool your assistant uses mints one for you; there is no `thor remember` command |
| `thor ingest [<path>] [--global] [--project <key>] [--detach]` | index a folder's text files (incremental; git repo = tracked-only, plain folder = walked; `--global` = the `@global` tier; `--project` pins a key) |
| `thor init [<path>]` | set up a project: write a `.thor` marker + first ingest; also seeds the working contract (see `thor install`) if it is not there yet |
| `thor reproject <id> --project <key> \| --global` | reassign a fact's project scope (sync-safe) |
| `thor backfill-projects [--apply]` | attribute legacy memories from their import footer |
| `thor review-scope [--mark]` | list no-signal global memories to review (SessionStart nudges once/day) |
| `thor courier` / `thor session-start` | per-prompt recall hook (session-dedup, noise gate, live-file freshness) / SessionStart refresh + pinned `<thor-brief>` + setup cue; on `source: "compact"` it first prints the post-compaction advisory (persist-now nudge + the judgment-debt list of memory hits served this session to mark useful/noise) |
| `thor pin <id> \| --list` / `thor unpin <id>` | pin standing rules: their full body re-injects at every session start and right after a compaction |
| `thor mark <id> [--noise]` | record that a fact actually helped - or was noise here (local; one unified usage strength feeds the courier's promotion and consolidate's decay) |
| `thor warm` | pre-warm the semantic embedder (idempotent; for SessionStart) |
| `thor guard` / `thor stop-guard` | moment-of-action advisories (risk rulebook + first-touch file memories) / response advisories + a once-per-session capture nudge for unstored decisions/gotchas |
| `thor install` | write the hooks into settings.json; also seeds THOR's working contract once, as a pinned global note with the fixed id `thor-working-contract`, so an assistant is handed the rules for using the memory at every session start instead of being asked to read a file. Seeded only when that id has no events yet, so re-running never overwrites your edits and never re-pins it after you unpin it |
| `thor vectors build \| sync \| status` | semantic sidecar (feature `semantic`) |
| `thor embed-daemon` | warm embedder for the courier (feature `semantic`) |
| `thor export` / `restore` / `backup` | JSONL backup + verified restore |
| `thor ship` / `recv` / `status` | cross-machine log-shipping sync |
| `thor drain-inbox --inbox <file> \| --from <url>` | replay a replica's captured writes into the authority's log (see the capture inbox in [OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md)) |
| `thor fsck` | verify chain integrity, FTS projection and FTS index structure - exits 1 on any of them, so a cron job or release step can gate on it. Repair a damaged index with `thor fsck --rebuild-fts` (derived from the log; nothing is lost). Also reports facts whose footer got lost (content health: it names them and never fails the run) |
| `thor consolidate [--apply-dedup]` | metabolism report: duplicate twins, decay candidates, same-topic clusters, anchor coverage plus the typed facts that name a file but carry no anchor (proven-useful first, with the anchor to consider, already-expired facts excluded - an anchor on a silenced fact gates nothing), the mirror of that (anchor crowding: targets carrying more live facts than the guard serves per touch, so the surplus is dropped without a word, plus bare file names that match every file of that name anywhere - a floor on the real crowding, since the guard also matches path tails), plus three mechanical backlogs - report-shaped facts with no expiry, project scopes no `wegwijzer` pointer names, and facts a response rule already enforces but that lack the `guarded` tag (found by reading the rulebook's reminders for a cited memory id). Exit 1 when anything needs digesting; only the dedup pass is ever applied mechanically |
| `thor steward` | prepare a stewardship review: the consolidate report + the proven conservative rubric (eight points, including EXPIRE - reports expire, RULES never; lift a still-governing rule out of a report into its own never-expiring fact before letting the report expire) written to a file an agent session works through with the MCP tools (no writes itself) |
| `thor symbols` | (re)build the derived symbol sidecar (`thor-symbols.db`): which names every code chunk defines and calls - powers `where_used`/`impact` and a deliberate-recall ranking bonus; refreshed automatically by every ingest, including the one `thor init` runs, so you only need this command by hand for a store that was filled some other way (a shipped replica), or after deleting the sidecar |
| `thor daemon` / `thor ensure-daemon` | warm injection daemon: `/inject` + `/health` on the HTTP server, discovered via a flag file; the courier answers warm and falls back cold on any failure. **Recommended** - it holds the folded log + vector matrix resident, which is ~60% of per-prompt latency (349 -> 120 ms measured). Expect a few hundred MB of RAM; the repo has no measurement of this daemon's own footprint (the measured ~650 MB below is the *embedder* daemon). It is the same server as `thor mcp --http`, so the full MCP toolset - writes included - is mounted on that port with no auth: keep the bind on loopback. Wire it in with `thor install --with-daemon` (`ensure-daemon` is the SessionStart form) |
| `thor doctor` | one-line health per surface: store, semantic model + sidecars, injection daemon warm/cold, flags |
| `thor pre-compact` | retired no-op, kept so a stale `PreCompact` hook registration keeps exiting 0. Claude Code never delivers PreCompact stdout to the model (verified 2026-07-23), so the advisory moved to `thor session-start` on `source: "compact"`; a `thor install` re-run removes the old registration |
| `thor recall --rerank` | rescore the top hits with the local cross-encoder (feature `semantic` + downloaded reranker model; MCP recall takes `rerank: true`) |
| `thor mcp [--http <bind>]` | run as an MCP server (stdio or Streamable-HTTP) exposing the full stewardship toolset: recall (`kind:"memory"` filter, `detail:"index"` for a compact id list) / get / history / remember (typed, duplicate-refusing, optional `expires: YYYY-MM-DD` after which a fact stops surfacing on every serving surface, recall and the guard alike - history and `get` keep it in full; a later revise that carries no footer of its own keeps that date, and says so in its reply; a body that opens as a rule and also gets an `expires` is stored anyway, with a warning that reports expire, rules never) / revise (body and/or single metadata fields: `anchors`, `expires`, `tags`, `triggers`, `fact_type` - one field changes, the rest of the footer stays as it was) / retract / resolve / mark / pin / unpin / reproject / brief / outline (a file's signature map) / where_used / impact (symbol callers + change blast-radius on the derived sidecar). On a replica (capture-inbox mode) remember/revise/retract/mark queue, and resolve/reproject/pin/unpin are refused with a pointer to the authority |

## Build features

- default: pure lexical (bm25) - no ML runtime, no extra dependencies.
- `semantic`: adds the dense score-fusion recall layer (ONNX embedder, warm
  daemon, precomputed sidecar). Client-only; a server/deploy build can stay on the
  default and never pull the ONNX runtime.

## Layout

```
thor/
  src/            the Rust crate (event store, recall, ingest, guard, sync, mcp, courier)
  deploy/         Dockerfile + docker-compose template
  tools/          export_mimir.py - migration helper for mimir users
  *.example.json  guard rulebook templates (copy + fill in)
docs/             everything you are reading now (setup, features, benchmarks, ...)
```
