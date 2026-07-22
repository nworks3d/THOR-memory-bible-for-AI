# What similar projects can teach THOR

> Research round 2026-07-10/11. Nine memory systems for AI agents were studied in
> depth — each diffed against THOR's shipped capabilities (per README +
> AI-BOOST-ROADMAP, all roadmap items closed 2026-07-09) and against the five
> **measured weaknesses** in BENCHMARKS.md "Honest weaknesses". Per-project
> chapters with evidence links follow the synthesis. Projects covered:
> [mimir](https://github.com/MakerViking/mimir) (the benchmark rival, re-examined
> at v0.14.0),
> [claude-mem](https://github.com/thedotmack/claude-mem),
> [agentmemory](https://github.com/rohitg00/agentmemory),
> [ai-memory-mcp](https://github.com/alphaonedev/ai-memory-mcp),
> [codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp),
> [Mem0](https://github.com/mem0ai/mem0),
> [Letta/MemGPT](https://github.com/letta-ai/letta),
> [Zep/Graphiti](https://github.com/getzep/graphiti),
> [cognee](https://github.com/topoteretes/cognee) + MemPalace.

## The headline findings

**1. THOR's core bets are independently validated.** Across all nine projects,
nobody else combines THOR's four differentiators: (a) a tamper-evident,
lossless history (hash-chained log, branch-on-conflict, CAS revise) — every
rival either mutates rows in place, soft-deletes, or lets an LLM silently
overwrite facts; (b) per-prompt *push* injection with a noise gate and session
ledger — every rival is pull-only or injects only at session start; (c) a
unified code+docs+memories index — rivals do conversation memory *or* code
intelligence, not both; (d) an LLM-free deterministic hot path. Two independent
confirmations stand out: agentmemory's ablation shows BM25 86.2% → +9pp from
adding dense vectors (exactly THOR's fusion design), and **Mem0 abandoned
LLM-decided UPDATE/DELETE in April 2026 as lossy — converging on THOR's
append-only model**. Mimir v0.14 (released 2026-07-10, branch name literally
`feat/thor-response-code-recall-latency-drift`) copies THOR's courier design:
per-prompt hook, trigger tags, session injection ledger, file anchors.

**2. The field converges on five mechanisms THOR does not have.** Each maps
onto a measured THOR weakness or a known failure mode, and each has a
lossless-compatible, mostly-LLM-free adoption path detailed in the chapters.

## Ranked adoption candidates

Ranked by (measured-weakness impact × evidence strength) / effort. "Weakness"
references BENCHMARKS.md Honest weaknesses; effort S/M/L as estimated in the
chapters.

### Tier 1 — direct answers to measured losses

**R1. `outline` + `peek` MCP tools (mimir #2) — effort S.** Mimir's
benchmark-winning token-economy feature: a file's tree-sitter signature map
(88% tokens saved) and one symbol's body (~90% saved). THOR already has
symbol-boundary chunks in the index — `outline` is "emit signatures of chunks
for path X", `peek` is "return one chunk's body". Attacks the token-economy
loss (THOR injects 679 tokens vs mimir cold 236) from the read-path side and
is the single highest value-per-effort item found in this round.

**R2. Derived symbol-graph sidecar: `where_used`/`impact` (mimir #1,
codebase-memory-mcp #1/#2) — effort M.** THOR's one standing benchmark loss
(code-structure 50.0% vs 57.6%; multi-project now lost outright) is exactly
what a call/import edge table closes. Key evidence from codebase-memory-mcp:
pure tree-sitter extraction without LSP scores "Good (75–89%)" on the very
languages that matter — good enough for "which functions call X" questions.
THOR's ingest already runs tree-sitter; add `symbol` + `sym_edge` tables as a
**derived, rebuildable sidecar outside the hash-chained log** (never
authoritative → fsck/export/sync untouched), name-based resolution like
mimir's, two tools (`where_used`, plus `impact <files>` feeding the file-touch
guard a symbol-level blast radius: "you edited pack_blocks; 7 callers in 3
files"). Skip Cypher/LSP entirely — cbm's own paper shows the token wins come
from fixed tools, not the query language.

**R3. Warm daemon / `/inject` route on the existing HTTP server (mimir #3) —
effort M.** Mimir's v0.14 warm path went ~254ms → single-digit ms by keeping
the engine (embedder + vector cache) resident; THOR's courier median is 253ms,
over its own 250ms guardrail. THOR already ships a Streamable-HTTP server —
add an `/inject` route, have the courier hook try HTTP first, fall back to the
current CLI path when the daemon is down (bm25-only fast mode). This also
frees latency budget for R5's rerank steps.

**R4. Injection/token diet: compact recall format + progressive disclosure
(ai-memory-mcp #5, claude-mem #3) — effort S.** Two composable pieces: (a) a
header-once tabular rendering (TOON-style, measured 61–79% smaller than JSON)
for multi-hit courier injections and `brief`; (b) `recall(detail:"index")`
returning `id | first-line | kind | score | age` lines (~50–100 tokens) with
`get` as the full-fetch layer — claude-mem measures ~10× savings on
agent-initiated search. Directly attacks the 679-token injection number.

### Tier 2 — recall-quality wins with strong evidence, all cheap

**R5. Retrieval hardening trio (ai-memory-mcp #1, mimir #7, Mem0 #6) — effort
S each.** (a) *FTS5 query ladder*: relax an AND-ish query through
content-words-OR → prefix-wildcard rungs only when results are thin — raises
the recall floor (their pure-FTS5 LongMemEval R@5: 97.0%) while THOR's
term-coverage rerank keeps junk out; distinguishes "THOR has nothing" from
"query too strict" at the noise gate. (b) *Identifier leg*: exact-match
identifier-shaped fragments (`Foo::bar`, snake/camel) as a third fusion input —
mimir added this precisely because BM25 stemming and embeddings both fuzz
identifiers apart; with code chunks in the index this is a common query class.
(c) *Calibrated BM25 sigmoid* before fusion (midpoint/steepness from query
term count, semantic-threshold gating) — tested normalization replacing
min-max, also fixes the "all-junk pool scores 1.0" artifact.

**R6. Usage/recency signals + expiry: the anti-stale package (Mem0 #3/#4,
agentmemory #2, ai-memory-mcp #3, Graphiti #1) — effort S.** Three
lossless-compatible pieces: (a) `access_count`/`accessed_at` bumped on every
recall/injection (THOR's injection ledger already records serves — a
served-but-never-marked signal can even be a *negative* prior), folded as a
mild 0.3–1.5× multiplier — the automatic complement to the manual `mark`; (b)
an optional `expires` field on `remember`/`revise` for known-temporary facts
("pin to v1.9 until upstream fix") — recall filters expired heads, history
keeps everything, `show_expired` overrides; (c) longer-term, Graphiti's
bi-temporal model (event-time `valid_at`/`invalid_at` next to the existing
transaction-time chain) as the principled fix for the stale-fact failure mode —
start with the two nullable columns and heuristic date extraction, skip the
LLM. Rank-time decay only, never eviction — THOR stays lossless.

**R7. Episodic safety net under the curated store (Letta #3/#4, claude-mem #2,
agentmemory #3, MemPalace #1, mimir #8) — effort S+M.** The single most
convergent theme across rivals: what nobody `remember`ed should not be
unrecoverable. Three graduated pieces, cheapest first: (a) **PreCompact
"persist now" advisory** (Letta's memory-pressure warning, fire-once flag):
Claude Code has a PreCompact hook; emit "context is about to compact — persist
durable decisions via remember", cross-checked against the session ledger to
name what was surfaced-but-never-captured. Zero LLM, effort S, and it patches
THOR's one *reactive-only* drift gap (pins recover after compaction; nothing
fires before). (b) **Session summary fact at SessionEnd** injected 1–3 deep at
next SessionStart under the existing ledger/noise-gate machinery — kills the
"agent restarts the plan from scratch" cross-session drift claude-mem and
agentmemory both target. (c) **Optional verbatim episode tier**: Stop-hook
ingest of the transcript JSONL into a low-ranked `episode` kind, excluded from
auto-recall by default, reachable via `recall kind:episode` with date filters —
the safety net (MemPalace proves this works verbatim with zero LLM; Letta's
`conversation_search` shows the query surface: role + date filters).

### Tier 3 — structural ideas worth one design round each

**R8. Sleep-time steward: make `consolidate` act (Letta #1, mimir #5, cognee
#3) — effort M.** `thor consolidate` finds duplicate twins, decay candidates
and clusters, but only dedup ever executes; Letta shows the loop-closer: a
scheduled/Stop-triggered headless agent pass (`claude -p`) fed the consolidate
report, armed only with THOR's own MCP stewardship tools
(revise/retract/resolve/reproject/pin) and a sleeptime-style prompt
("be selective"). Every action lands in the hash-chained log — THOR's substrate
makes this *safer* than Letta's own (auditable, revertible), a genuine
differentiator no rival can match. Rate-limit per-N-sessions.

**R9. Typed links + entity anchors (mimir #4, Mem0 #2, Graphiti #3, cognee #1)
— effort M.** A `link` event type in the append-only log
(`{src, dst_entity_or_symbol, rel}` — links become revisable/retractable,
which mimir's mutable links are not), plus a heuristic entity table (file
paths, symbols, crate names, env keys, quoted spans — no LLM needed in the
coding domain) as a third fusion boost and 1-hop score-damped expansion behind
the noise gate. Gives "the decision that explains this function" in one hop
and sharpens contradiction/dedup grouping. Adopt after R2 (symbols exist then).

**R10. Fixed-budget pin blocks (Letta #2) — effort S/M.** Pins are an
open-ended set today; pin sets grow monotonically and dilute the SessionStart
injection. Named per-project blocks with a char budget (writes that exceed it
are refused with "compress or demote") make prioritization the agent's
problem and give injection a hard ceiling. Versioned as events in the chain.

**R11. Tool-surface gating + skills (ai-memory-mcp #4, agentmemory #5) —
effort S.** (a) `--profile core/full`: advertise only
recall/remember/get/mark by default with a `capabilities` tool to expand —
THOR's ~13 tool schemas cost every session's context; plus a CI gate on
serialized schema token size. (b) Ship `.claude/skills/` (thor-handoff,
thor-resolve-diverged, thor-recap) so stewardship workflow lives in one place
instead of each agent's judgment.

**R12. Public-dataset benchmark harness (agentmemory #6, MemPalace #4) —
effort S/M.** A LongMemEval R@k runner over THOR's recall API, in CI. THOR's
judged mimir duels measure the right thing but aren't independently
reproducible (BENCHMARKS.md says so itself); a public number makes fusion
tuning falsifiable and is the marketing answer to every rival quoting 95–98%
R@5 on the same dataset. Their scores are session-level retrieval (coarser
than THOR's chunk-level task), so report both granularities honestly.

### Explicitly rejected after study

- **LLM in the hot path** (Mem0 extraction/criteria-scoring, cognee cognify,
  Graphiti's per-episode extraction chain): incompatible with THOR's
  deterministic sub-250ms courier; every adoption above keeps LLM work in
  explicit, rate-limited background passes.
- **Auto-eviction / TTL deletion** (agentmemory, ai-memory-mcp tiers): decay
  belongs at rank time; THOR is lossless by thesis.
- **A graph query language** (codebase-memory-mcp's Cypher): their own paper
  shows the token wins come from fixed tools; `outline`/`where_used`/`impact`
  capture ~all agent value at ~5% of the cost.
- **Blocking LLM compression per tool call** (claude-mem "Endless Mode",
  60–90s/call): watch, don't copy; the cheap variant (pointer injection for
  oversized tool outputs) can ride R7's episode tier.
- **LLM-decided UPDATE/DELETE** (Mem0 classic): Mem0 itself retreated;
  THOR's CAS revise/retract + DIVERGED branching is the stronger model.

## Where this leaves the rivalry with mimir

Mimir v0.14 closed most of THOR's former structural leads (code-content
indexing, per-prompt injection, trigger tags, session ledger, remote MCP) —
the moat is now **losslessness + stewardship depth + drift defenses**, which
mimir cannot copy without abandoning its mutable-row storage model. The four
mimir features THOR has not answered are R1 (outline/peek), R2 (symbol graph),
R3 (warm daemon) and its `person` memory type (S — add to the fact_type enum,
treat person names as implicit triggers). R1–R3 are precisely THOR's three
measured benchmark losses; landing Tier 1 flips the scoreboard on every
category mimir currently wins, while R6–R8 extend the moat where mimir
structurally cannot follow.

---

*Method note: each chapter below was researched against the project's actual
README/docs/source/changelogs (July 2026 state) by an independent research
pass, with THOR's capability list supplied so the diff is against what THOR
ships today, not its README marketing. Effort ratings are the chapter
authors' estimates against THOR's Rust+SQLite+hooks architecture. Claims
worth acting on (benchmark numbers, mechanism details) should be re-verified
against the cited sources before implementation.*

---

# Per-project research chapters

## mimir (current state)

### Overview + what changed recently (releases/commits up to mid-2026)

Mimir (MakerViking/mimir, Rust 95%, SQLite WAL+FTS5, MIT/Apache-2.0, 17 releases, ~118 commits) is a local-first unified memory for AI coding agents — a merger of three prior tools (OpenBrain, QMD, Graphify) into one SQLite graph where *everything is a node*: memories, doc chunks, files, code symbols, projects, tags. Six memory types (`gotcha/decision/insight/idea/note/person`), hybrid BM25 + local ONNX embeddings (bge-small) fused via RRF, tree-sitter code graph across 13 languages, and MCP tools `recall/remember/get/link/graph/mark/status/outline/peek/forget/consolidate/supersede`.

It is shipping at very high cadence — the entire public history is June 12 → July 10, 2026, and it is explicitly benchmark-dueling THOR (commit merge branch **`feat/thor-response-code-recall-latency-drift`**, July 10):

- **v0.14.0 (2026-07-10, today)** — the "THOR response" release: **code-content indexing** (source chunked on tree-sitter symbol boundaries; function *bodies* searchable as `Kind::CodeChunk` — direct answer to THOR's unified repo ingest); warm recall ~254ms → **single-digit ms** via in-place vector-matrix cache patching (~258ms → ~6ms on a 97k-node store); third **identifier RRF leg**; gated **auto-rerank**; **context guard** (pause/handoff); **trigger tags** (`--fires-when`) + **guard anchors** (`--anchor`) + per-session `injection_log` dedup ledger; +C++/Kotlin/Swift/PHP; alternative embedder (granite-embedding-small-r2) and int8 reranker.
- **v0.13.0 (07-03)** — remote **Streamable-HTTP MCP**; forget/consolidate/supersede exposed over MCP (soft-delete only; hard delete stays CLI).
- **v0.12–v0.9 (06-18→26)** — WAL concurrency fixes, C#/SQL languages, project-scoped cross-machine sync (`.mimir` marker, portable keys), zero-init project detection.
- **v0.8.0 (06-16)** — the token-economy release: **outline/peek**, token-savings ledger, `mimir run` command-output filtering, rules packs, optional Anthropic API proxy with prompt-cache breakpoints.

Note the pattern: v0.14's per-prompt auto-recall hook, trigger phrases, session ledger, and file-edit anchors are mimir **copying THOR's courier-hook design**, while its code-content indexing copies THOR's symbol-boundary repo ingest. The rivalry is bidirectional; the list below is what mimir has that THOR still hasn't answered.

### Mechanisms THOR lacks

**1. Code-symbol graph with call/import edges (`callers`/`impact`/`path`/`hubs`)** — Tree-sitter extracts symbols *and* call/import edges across 13 languages into the same SQLite store; queries: `mimir graph callers resolve_ref`, `mimir graph impact $(git diff --name-only)` (change blast radius), `path`, `hubs`, plus an HTML viz. Code-graph refresh is 0.55s on a 2,495-file TS repo (360× faster than its predecessor Graphify); graph queries hit 24µs at 500k nodes. Evidence: README Code Graph section; v0.5–v0.11 changelog. Why for THOR: THOR chunks code on symbol boundaries but stores no *relations* — it can find a function, not answer "what breaks if I change this." Adoption: THOR already runs tree-sitter for chunk boundaries; add an `edges(src_symbol, dst_symbol, kind)` table populated in the same parse pass (imports + same-file/same-crate call resolution first — cross-file resolution can be name-based, mimir's is too), recursive-CTE queries for callers/impact. Effort: **M** for imports+name-resolved calls, L for precise resolution.

**2. outline/peek token economy** — `outline` returns a file/dir's tree-sitter signature map; `peek` returns one symbol's body. Measured with a single tokenizer (tiktoken o200k_base): outline 88% saved across a 50-file crate (134,925→16,068 tokens; one file 8,196→99 = 99%), peek ~90% avg (1,741→94 = 95%). Evidence: docs/benchmarks.md. This is mimir's benchmark-winning feature: it converts memory infrastructure into a *read-path* saving on every task, not just recall quality. Adoption: THOR already has symbol-chunked code in its index — `outline` is "emit signatures of all chunks for path X," `peek` is "return one chunk's body"; expose both as MCP tools + note them in tool descriptions so agents prefer them over Read. Effort: **S**. Highest value-per-effort item on this list.

**3. Warm daemon + local `/inject` endpoint** — `mimir daemon` (alias for `mimir mcp --http`) keeps one engine (embedder + vector cache) resident; the UserPromptSubmit hook POSTs to `http://127.0.0.1:8077/inject` (~7ms warm; THOR's July benchmark measured ~62ms end-to-end hook) instead of cold-starting (~230–240ms full hybrid cold; hook falls back to `cold_mode="fast"` BM25+identifiers ~5–6ms when the daemon is down). `mimir doctor` does a 1s warm-check GET. Evidence: README daemon/hooks section; commits c6f0841, c1da6d1. Why: THOR's per-prompt courier pays process spawn + index open + embed cold-start every prompt; on a big fused index that dominates latency and forces corner-cutting in ranking. Adoption: THOR already has a Streamable-HTTP MCP server — add a `/inject` route on it, have the courier hook try HTTP first and fall back to the current CLI path; keep SQLite as the single writer (WAL handles hook-reader concurrency). Effort: **M**.

**4. `link`/`graph` — explicit typed relations, memories ↔ memories ↔ code** — `mimir link m:ABC123 my_function --rel about` creates a first-class edge between a memory and a code symbol (or another memory); linked items surface together in recall, and `graph` traverses them. Combined with the code graph this yields "the decision that explains this function" in one hop. Evidence: README Link/Graph section. Why: THOR's only relations are revision-chain (parent_rev) and tags; it cannot represent "this gotcha is *about* that module" or "this decision supersedes that idea across entities," so recall can't expand along meaning. Adoption: a `links` event type in THOR's append-only log (`{src_entity, dst_entity_or_symbol, rel}` — fits the hash chain naturally, links become revisable/retractable), 1-hop expansion in recall with a score discount, `link` MCP tool. Effort: **M**.

**5. `consolidate` — scheduled LLM-free memory hygiene** — weekly pass that (a) dedups near-identical memories, (b) flags contradictions, (c) distills clusters into summaries, (d) archives dead memories via typed half-life decay — never destructive, with `--dry-run`, now callable over MCP. Evidence: README Self-Learning/Consolidate; v0.13 changelog. Why: THOR's revise/retract/resolve are *manual*; nothing prevents slow store rot, and the courier's noise gate treats the symptom not the cause. Adoption: THOR already computes near-dup similarity at `remember` time — run the same comparator store-wide on a schedule; contradictions = high-sim pairs flagged as `[DIVERGED]`-style pending resolution (reusing THOR's existing branch/resolve machinery is actually a *better* substrate for this than mimir has); archival = decay flag, not deletion, preserving the log. Effort: **M**. [NB: THOR shipped `thor consolidate` (report + dedup apply) recently; the gap is contradiction-flagging + decay-archival + schedule.]

**6. `person` memory type** — a sixth type for facts about collaborators ("Alice owns the deploy pipeline," "Bob prefers PRs under 300 lines"), participating in tags/links/recall like any memory. Why: THOR's fact_types (gotcha/decision/preference) have no people axis; team knowledge either gets mistyped or lost, and person facts want different scoping (global, slow decay) and different trigger conditions (name mentions). Adoption: add `person` to THOR's fact_type enum + treat person-name tokens as implicit trigger words in the courier's query router. Effort: **S**.

**7. Identifier RRF leg + gated auto-rerank** — v0.14 adds a *third* retrieval leg: `search_hybrid` extracts identifier-shaped fragments (`MatrixCache::ensure`) that BM25 stemming and embeddings both "fuzz apart," and exact-matches them before RRF fusion. Separately, cross-encoder reranking auto-engages only when models are warm (`[rerank] auto = "warm"`) since it costs ~84ms/candidate. Evidence: CHANGELOG v0.14.0, commit 5d56c71. Why: THOR's bm25+dense two-leg fusion has exactly this weakness on code-lookup queries — and with code chunks in the index, identifier queries are common. Adoption: tokenize identifier-shaped query fragments (`::`, `.`, snake/camel heuristics), run an unstemmed exact-match FTS5 query as a third RRF input. Effort: **S**.

**8. Context guard with session handoff** — hook estimates context fullness from transcript byte size (no JSONL parsing), nudges once per +10pp band past a threshold (default 45%); `handoff` mode additionally instructs the agent to save a `session-handoff`-tagged memory before `/clear`, then auto-restores the latest handoff at the next SessionStart. Evidence: README context guard section; commit 4642811. Why: THOR re-pins after compaction (recovery) but does nothing *pre-emptive* — handoff turns context exhaustion into a deliberate, lossless transition. Adoption: extend THOR's courier hook with the same byte-size estimate; the handoff memory is just a tagged `remember` + a SessionStart pin-like injection, all existing machinery. Effort: **S/M**.

Also notable but lower priority: `mimir run` command-output filtering + token-savings ledger (73–100% saved on build/install chatter — orthogonal to memory but part of why mimir wins token benchmarks), the Anthropic API proxy with prompt-cache breakpoints, and typed half-life decay in ranking (`recency_alpha=0.012` on decaying kinds — THOR's query-routed ranking could absorb this cheaply). The `[[wikilink]]` convention appears in usage docs but has no changelog footprint; links are primarily made via the `link` tool.

### What THOR already does better

- **Provenance and losslessness**: hash-chained append-only event log with branching + explicit `resolve` of diverged heads; mimir has mutable rows with soft-delete and `supersede` — no tamper-evident history, no divergence model, and its sync is last-write-wins (can silently drop a concurrent edit), vs THOR's authoritative log-shipping.
- **Revision stewardship depth**: `revise/retract/resolve/history/reproject` as first-class MCP operations; mimir's only lifecycle verbs are `forget` and `supersede` (and consolidate's automated flags have no revision trail).
- **Per-prompt injection sophistication**: THOR's courier does query-routed ranking, multi-fact typed injection, noise gate, session ledger, and live-file freshness; mimir's auto-recall injects a *single* memory above a relevance floor and only gained trigger phrases/session ledger/anchors *this week* (v0.14, copying THOR).
- **File-touch guard is automatic**: THOR guards on any file touch; mimir's anchors are author-declared per memory (`--anchor`, max 8 patterns) — coverage depends on capture-time foresight.
- **Post-compaction re-pinning**: THOR restores pins after compaction; mimir's handoff restore only fires at SessionStart.
- **Code-content indexing priority**: THOR's unified repo+docs+memories fused index with symbol-boundary chunking predates mimir's v0.14 `Kind::CodeChunk` (shipped July 9–10 as an explicit "thor-response").
- **Capture discipline**: THOR's capture nudge + PreToolUse guard actively drive the write path; mimir relies on agent norms ("search before capture") in its MCP instructions.
- **Remote MCP parity, THOR first**: mimir only gained Streamable-HTTP remote MCP on July 3 (v0.13).

---

## codebase-memory-mcp

### Overview
codebase-memory-mcp (DeusData) is a single static **C binary** MCP server that indexes a repo into a persistent **SQLite-backed knowledge graph** (nodes: Project/Package/File/Module/Class/Function/Method/Interface/Route…; edges: CALLS, IMPORTS, INHERITS, IMPLEMENTS, HTTP_CALLS, DATA_FLOWS, SIMILAR_TO) using vendored tree-sitter grammars for 158 languages plus an optional hybrid-LSP semantic pass for 11. It exposes 14 MCP tools (search_graph, trace_path, detect_changes, a read-only openCypher subset, get_code_snippet, get_architecture, manage_adr…) with sub-ms query latency; the headline "99.2% fewer tokens" is a single 5-query anecdote (~3.4K vs ~412K tokens), while its paper (arXiv:2603.27277, 31 repos) claims the more sober **10× fewer tokens, 2.1× fewer tool calls, 83% answer quality**. It is a *derived, disposable structural cache* — no history, no provenance, no prose memory.

### Mechanisms THOR lacks

**1. Tree-sitter symbol/call graph with BFS traversal (`trace_path`)** — Multi-pass pipeline: pass 1 extracts definitions, call sites, and imports from tree-sitter ASTs; pass 2 (optional LSP) resolves types/inheritance; symbols get qualified names `<project>.<path_parts>.<name>` addressable by `search_graph` (label + regex name pattern + file glob + degree filters) and `trace_path(function_name, direction=inbound|outbound, depth 1–5)` at <10ms. Evidence: https://github.com/DeusData/codebase-memory-mcp (README, "Graph Schema" / tools table). **This is exactly THOR's one benchmark loss to mimir**, and cbm's tier data is the key evidence that a sidecar can close it *without* LSP: pure tree-sitter extraction scores "Good (75–89%)" for Rust/Python/TS/Go — enough for "which functions call X" benchmark questions, which reward recall of call sites, not type-perfect resolution. **Cheapest version for THOR:** THOR's ingest dispatcher already runs tree-sitter for symbol-boundary chunking, so definitions are already in hand. Add per-language call-expression queries, store two derived tables (`symbol(qname, kind, sig, file, span, chunk_id)`, `sym_edge(caller, callee, kind)`) keyed off existing chunk rows, resolve callees by project-scoped name (imports-aware only where trivial), rebuild per-file on the same incremental path. Expose exactly two tools: `outline(path)` and `where_used(symbol, depth≤3)`. Critically: keep it a **derived sidecar outside the hash-chained event log** (like cbm's cache: rebuildable, never authoritative) so fsck/export/log-shipping are untouched. Skip Cypher, LSP, HTTP_CALLS entirely. Effort: **M** (call-site queries per top-6 languages + name resolution + 2 tools).

**2. `detect_changes` — git diff → affected symbols → blast radius** — Maps uncommitted diff hunks to enclosing symbols, then walks inbound CALLS edges to classify risk ("blast radius with risk classification"). Evidence: https://github.com/DeusData/codebase-memory-mcp (tools table). For THOR this upgrades the file-touch drift guard from file-level to symbol-level: "you edited `pack_blocks`; 7 callers across 3 files depend on it." Adoption: THOR already stores chunk line spans; intersect diff line ranges with symbol spans from mechanism #1, then a 1–2-hop reverse-edge SQL walk; surface via the existing PreToolUse/file-touch hook. Effort: **S** (given #1).

**3. PreToolUse Grep/Glob interception with graph-context injection** — Its Claude Code install ships a **structurally non-blocking** hook (exit 0 on every failure path; deliberately never gates `Read` because "gating Read breaks the read-before-edit invariant"): when a Grep/Glob search token matches indexed symbols, it injects structured symbol context as `additionalContext` alongside the native results. Evidence: https://github.com/DeusData/codebase-memory-mcp (README, agent-integration section). This is one of its two real token-economy levers — it converts grep fan-out (the ~412K-token failure mode) into one injected answer, while never breaking the agent. THOR already owns a PreToolUse advisory guard and per-prompt injection plumbing; adding "grep pattern ∩ symbol table → inject `where_used`/definition locations" is a small extension and would let THOR's recall hook win *searches*, not just prompts. Effort: **S** (given #1).

**4. Committable compressed graph artifact + git-polling watcher for freshness** — Freshness is two-layered: (a) a background watcher does **git polling with adaptive intervals** and re-indexes only changed files (`auto_watch`, default true); (b) a team-shareable `.codebase-memory/graph.db.zst` snapshot (zstd, 8–13:1) with a two-tier write policy — `zstd -9` + index-strip + `VACUUM INTO` on explicit index, `zstd -3` for low-latency watcher writes — and on clone, `index_repository` **imports the artifact then incrementally indexes only the local diff**, with an auto-added `.gitattributes merge=ours` to duck binary merge conflicts. Evidence: https://github.com/DeusData/codebase-memory-mcp (README, auto-sync + team artifact sections). THOR's hook-driven per-prompt freshness is arguably the better default (no daemon), but the *bootstrap-then-incremental artifact* is worth stealing for the sidecar specifically: because the symbol graph is derived and log-independent, THOR could ship/commit a compressed sidecar snapshot so teammates and CI skip the cold-index cost — something THOR's log-shipping (which moves authoritative history, not caches) doesn't cover. Effort: **M**.

**5. `SIMILAR_TO` MinHash+LSH near-clone edges** — MinHash signatures per code block, LSH banding for candidate pairs, Jaccard-scored edges; also powers dead-code/duplication analysis (~150ms whole-repo). Evidence: https://github.com/DeusData/codebase-memory-mcp (README, edge types). Two THOR uses: strengthen `remember`'s near-duplicate refusal beyond lexical/dense similarity (MinHash is cheap, exact-threshold, embedding-free), and dedupe code chunks across branches/vendored copies before they pollute recall ranking. Adoption: MinHash over existing chunk token streams at ingest, one LSH bucket table in SQLite. Effort: **S–M**; lowest priority of the five.

*(Its Cypher engine is impressive but the wrong buy for THOR: a fixed `outline`/`where_used` surface captures ~all benchmark value at ~5% of the cost; a query language is effort L with marginal agent benefit — the paper's own token wins come from the fixed tools, not Cypher.)*

**On the token-economy claims:** the mechanism is (1) qualified-name addressing — `get_code_snippet` returns only the symbol body, never a file; (2) structural queries return compact node/edge lists; (3) the hook pre-empts grep fan-out. The validated number is 10×, not 99%; THOR's symbol-boundary chunking already gets partway there, and mechanisms #1+#3 would capture most of the rest.

### What THOR already does better
- **Lossless, auditable memory:** hash-chained append-only log, revise/retract/history, branch-on-conflict + resolve, fsck, export/restore. cbm's graph is a rebuildable cache with zero provenance or history; its team-merge story is literally `merge=ours` (silently discard one side).
- **One index for code + docs + memories** with unified bm25+dense fusion recall; cbm is code-structure-only (ADR CRUD is a bolt-on key-value feature, not searchable prose memory, and it has no embeddings-based recall of natural language at all).
- **Relevance machinery:** query-routed ranking, session ledger, noise gate, `mark` feedback loop, live-file freshness tags. cbm has no feedback signal and no per-prompt injection — the agent must know to ask.
- **Stewardship + scoping:** pin/unpin standing rules, reproject, project isolation + global tier, brief. cbm's nearest equivalent is `list_projects`/`delete_project`.
- **Behavioral drift defenses** beyond search interception: SessionStart/post-compaction pins, capture nudge, advisory PreToolUse guard.
- **Cross-machine sync with integrity guarantees** (log-shipping over a verifiable chain) vs. shipping an opaque binary snapshot.
- **Rust + durable file-backed SQLite** vs. an ~88%-C codebase parsing untrusted input, with in-memory-SQLite indexing whose persistence path is a cache directory.

---

## claude-mem

### Overview
claude-mem is a TypeScript/Bun plugin for Claude Code (with adapters for Cursor, Windsurf, Codex, OpenClaw, Gemini, OpenCode) that wires five lifecycle hooks (SessionStart, UserPromptSubmit, PostToolUse, Stop, SessionEnd) to a locally-running Bun HTTP "worker service" backed by SQLite+FTS5 plus a ChromaDB vector store. Memories are made passively: every tool invocation's input/output (~1k–10k tokens) is shipped to an SDK Agent (Claude Agent SDK) that compresses it into a ~500-token "observation" (title, narrative, type, facts, content-hash) stored in SQLite, embedded per-facet into Chroma (`obs_{id}_narrative`, `obs_{id}_fact_0`, …), and broadcast to a live web-viewer UI over SSE; at SessionEnd an LLM-generated session summary (learned/completed) is stored, and new sessions get recent-session context injected at SessionStart plus per-prompt semantic retrieval via `/api/context/semantic`.

### Mechanisms THOR lacks

**1. Passive LLM-compressed observation capture (PostToolUse → SDK Agent pipeline)** — Every tool result triggers PostToolUse, which enqueues the tool I/O to the worker; a background SDK Agent (Claude Agent SDK) rewrites it into a compact structured observation (title + narrative + type + facts), deduped by `SHA256(memory_session_id + title + narrative)[:16]` in a 30s window, stored in SQLite and embedded into Chroma. Nothing depends on the agent or the user remembering to capture. Evidence: https://raw.githubusercontent.com/thedotmack/claude-mem/main/docs/architecture-overview.md, https://github.com/thedotmack/claude-mem. Why it helps THOR: THOR's capture path is active (typed `remember` + a once-per-session Stop nudge), so decisions/gotchas that the agent never verbalizes are lost — this is the single biggest recall-coverage gap, and it directly feeds THOR's drift metrics (facts that were never captured can't be re-injected). Adoption sketch: a PostToolUse hook appending raw tool events to a `staging_events` SQLite table (cheap, no LLM in the hot path), plus an async/batched summarizer (shell out to `claude -p` with haiku, or batch at SessionEnd) emitting candidate typed facts through the existing `remember` path so near-dup refusal, hash chain, and project scoping all still apply. Effort: L (M if batch-only at SessionEnd).

**2. Session summaries + episodic recap at SessionStart** — SessionEnd generates an LLM summary of the whole session (a `summaries` table with learned/completed fields); SessionStart injects summaries of the last ~10 sessions, so a fresh session opens knowing "what we were doing and where we left off." Evidence: https://raw.githubusercontent.com/thedotmack/claude-mem/main/docs/architecture-overview.md; reviews at https://byteiota.com/claude-mem-auto-context-for-claude-code-sessions/. Why it helps THOR: THOR re-injects pinned standing rules at SessionStart but has no episodic continuity — cross-session task drift ("agent restarts the plan from scratch") is exactly the failure mode session recaps prevent, and it's cheap, bounded context. Adoption sketch: SessionEnd hook summarizes the transcript (transcript path is in hook input) into a `kind=session_summary` fact; courier/SessionStart injects the most recent 1–3 summaries for the current project under the existing ledger + noise-gate machinery so they rotate out once stale. Effort: M.

**3. Progressive-disclosure retrieval (index → timeline → full fetch)** — The MCP surface is deliberately layered: `search` returns ~50–100-token compact index lines (IDs + titles + type/date filters), `timeline` returns chronological neighborhood, `get_observations` batch-fetches full ~500–1,000-token bodies only for filtered IDs; claimed ~10x token savings by filtering before fetching. Evidence: https://github.com/thedotmack/claude-mem (README "3-layer token-efficient workflow"), https://raw.githubusercontent.com/thedotmack/claude-mem/main/docs/api.md. Why it helps THOR: THOR's `recall` returns full fact bodies; on broad queries that's the main context-economy cost of stewardship. A compact mode makes agent-initiated recall much cheaper and pairs naturally with THOR's existing `get`. Adoption sketch: add `detail: "index"|"full"` to `recall` returning `id | title/first-line | kind | score | age`; `get` already serves as layer 3. Effort: S.

**4. Cross-memory timeline (temporal navigation)** — `timeline` reconstructs "what happened around this observation" chronologically across sessions, independent of relevance ranking — episodic recall by time, not similarity. Evidence: https://github.com/thedotmack/claude-mem (search tools table), https://raw.githubusercontent.com/thedotmack/claude-mem/main/docs/api.md. Why it helps THOR: THOR has per-entity `history` but no way to answer "what else did we learn the day we made this decision" — useful for post-hoc divergence resolution (which head came from which work stream) and for debugging why a regression appeared. Adoption sketch: THOR's append-only event log is already a totally-ordered timeline; expose `timeline(anchor_id, window)` as a thin query over event timestamps/sequence numbers. Effort: S.

**5. Live memory viewer (SSE web UI)** — The worker serves a web UI streaming observations in real time as they're generated (Server-Sent Events broadcast on each pipeline write), giving users continuous visibility into what's being remembered — a major trust/adoption factor cited in every review. Evidence: https://github.com/thedotmack/claude-mem, https://raw.githubusercontent.com/thedotmack/claude-mem/main/docs/architecture-overview.md. Why it helps THOR: indirect but real — users who can see captures/injections tune pins and triggers instead of disabling hooks; also makes the injection ledger and noise gate auditable ("why didn't X get injected"). Adoption sketch: THOR already has a Streamable-HTTP server; add one static HTML page + an SSE/poll endpoint tailing the event log and courier injection ledger. Effort: M.

**6. Mid-session observation substitution ("Endless Mode", beta)** — PreToolUse records `tool_use_id`; the PostToolUse save hook *blocks* (110s timeout) until the SDK Agent finishes compressing, then injects the ~500-token observation so Claude works from compressed observations instead of re-reading raw outputs — claimed ~20x more tool uses per context window, at 60–90s added latency per tool call. Evidence: https://docs.claude-mem.ai/endless-mode (indexed; site blocks proxy fetch), design summarized at https://daily.dev/posts/jlu8e9uop. Why for THOR: the biggest context-economy idea here, and it targets in-session drift (compaction) at the root; but the latency cost is severe and it needs an LLM in the synchronous path — contrary to THOR's deterministic-hot-path design. Adoption sketch: only the cheap half — THOR's PostToolUse hook could inject a one-line pointer ("stored as <id>, recall on demand") for oversized tool outputs rather than blocking on LLM compression. Effort: L (full), S (pointer variant). Flagging as watch-not-copy.

Skipped as trivia/parity: `<private>` tags (THOR could add a strip filter in an afternoon), hybrid FTS5+vector search (THOR already has bm25+dense fusion, with strictly more ranking machinery), feedback signals table (parity with THOR's `mark`, and THOR actually feeds it into a ranking prior), multi-editor adapters (portability play, orthogonal to memory quality).

### What THOR already does better

- **Integrity and versioning**: hash-chained append-only log, CAS `revise`/`retract`, divergence with both heads kept + `resolve`, `fsck`, JSONL export/restore. claude-mem is a mutable SQLite with `PATCH`/`DELETE` endpoints — no tamper evidence, no revision history, no conflict model at all.
- **Multi-machine story**: THOR's log-shipping sync is peer replication of one owner's memory; claude-mem's "server mode" is a hosted team API with API keys — different problem, and its local mode is single-machine.
- **Corpus breadth**: THOR indexes repos + docs + memories in one index with symbol-boundary chunking and project isolation + global tier. claude-mem indexes only session observations — it cannot answer "where is X implemented" from the memory system.
- **Ranking**: query-routed ranking, source-class priors, term-coverage/proximity rerank, reserved constraint slots, author-declared trigger words, `mark`-driven prior. claude-mem is plain hybrid FTS5+Chroma similarity.
- **Injection hygiene / context economy at injection time**: per-session ledger (no repeats), rotation, noise gate (inject nothing on weak match), [refreshed]/[stale?] freshness tags. claude-mem injects last-N session summaries unconditionally every SessionStart — a fixed token tax and repeated content.
- **Drift-specific defenses claude-mem has nothing like**: pin re-injection immediately after compaction, file-touch guard on first edit, PreToolUse risk rulebooks.
- **Capture quality control**: typed facts with atomic semantic near-dup refusal vs claude-mem's 30-second exact-hash dedup window; and THOR capture costs zero LLM tokens vs an LLM call per tool use.
- **Footprint/reliability**: one Rust binary + SQLite vs Bun + Node 20 + uv/Python + Chroma + a supervised background worker with restart/backoff logic; several reviews cite worker-death and latency (60–90s/tool in Endless Mode) as claude-mem's chief failure modes.
- **Evaluation**: THOR ships a reproducible drift eval and judged benchmarks in-repo; claude-mem's "~10x token savings" and "20x session length" claims are unbenchmarked marketing numbers.

Sources: [claude-mem README](https://github.com/thedotmack/claude-mem), [architecture-overview.md](https://raw.githubusercontent.com/thedotmack/claude-mem/main/docs/architecture-overview.md), [api.md](https://raw.githubusercontent.com/thedotmack/claude-mem/main/docs/api.md), [Endless Mode docs](https://docs.claude-mem.ai/endless-mode), [byteiota review](https://byteiota.com/claude-mem-auto-context-for-claude-code-sessions/), [daily.dev summary](https://daily.dev/posts/jlu8e9uop), [apidog guide](https://apidog.com/blog/how-to-use-claude-mem/).

---

## agentmemory

### Overview
agentmemory (TypeScript, Apache-2.0, ~5.8k stars) is a background memory server built on the "iii engine" (worker/function/trigger primitives) that runs as a local daemon (REST on :3111, streams :3112, viewer :3113) and plugs into 20+ agents via MCP stdio plus 12 lifecycle hooks. Storage is SQLite + an in-memory vector index (local `all-MiniLM-L6-v2` embeddings by default, no API key). Memories are made **automatically**: PostToolUse hooks capture every tool call (name/input/output) as a raw observation (SHA-256 dedup, 5-min window; secret/`<private>` stripping), then an optional LLM pass compresses observations through four tiers — working → episodic (session summaries) → semantic (extracted facts) → procedural (workflows) — with Ebbinghaus-curve decay and auto-eviction; retrieval is triple-stream RRF fusion (BM25 + vectors + knowledge-graph traversal), session-diversified (max 3 results/session).

**How it earns 95.2% R@5 on LongMemEval-S** (important caveats): the number is *retrieval recall*, not end-to-end QA — per question it builds a fresh index over ~48 sessions and checks whether gold *session IDs* appear in top-5. BM25 alone (Porter stemming + synonym expansion) already scores 86.2%; adding MiniLM vectors gives +9pp → 95.2% (R@10 98.6%, MRR 88.2%). Weakest category: single-session-preference (83.3%). So the score is earned by a well-tuned hybrid retriever, not by the consolidation/graph/skills machinery — and it validates THOR's bm25+dense fusion design rather than beating it. Evidence: https://github.com/rohitg00/agentmemory/blob/main/benchmark/LONGMEMEVAL.md, https://github.com/rohitg00/agentmemory/blob/main/benchmark/COMPARISON.md ("only agentmemory's 95.2% is our own measured result... every other number here is the vendor's published claim, on a different benchmark").

### Mechanisms THOR lacks

**1. Zero-effort auto-capture with LLM consolidation (working→episodic→semantic→procedural)** — PostToolUse/PostToolUseFailure hooks record every tool call and failure as raw observations (hash-dedup + privacy filter); on Stop, an LLM (opt-in, any provider incl. Ollama) compresses the session into a summary and extracts facts/patterns; recurring multi-step patterns consolidate into procedural workflows. Evidence: https://github.com/rohitg00/agentmemory (README, "Memory Capture Pipeline"). Why it matters for THOR: THOR's recall ceiling is what got stored — the Stop-hook capture *nudge* (once/session) depends on agent compliance, so decisions and gotchas are silently lost, which is exactly the drift THOR fights. Auto-capture makes memory the default. Adoption: a THOR `Stop`/`PostToolUseFailure` hook appends raw observations to a separate `working` SQLite table (outside the hash-chained log, so losslessness is untouched); a `thor consolidate` pass (one LLM call, offline-capable) emits typed candidate facts that go through the existing `remember` path (near-dup refusal, tags, provenance) — either auto-stored with a `distilled` tag or as a review queue surfaced by `brief`. Effort: **L** (M without the LLM step — even raw failure-capture into file-history is useful).

**2. Decay + access-strengthening in ranking** — Ebbinghaus-curve decay; "frequently accessed memories strengthen. Stale memories auto-evict." Evidence: https://github.com/rohitg00/agentmemory#readme. Why: THOR has the positive half (`mark` → ranking prior) but nothing pushes *down* aging, never-helpful facts, so they keep competing for courier slots — pure context-economy loss. Adoption: add an exponential time-decay term (reset/boosted by `mark` and injection-use from the ledger) to the fused score; **do not evict** — THOR is lossless, so decay only at rank time, and let `brief` list heavily-decayed facts as retirement candidates for explicit `retract`. Effort: **S**.

**3. Episodic session layer: session summaries, `memory_timeline`, `memory_file_history`, `/recap`, `/handoff`** — Stop-hook writes a per-session narrative; timeline replays observations chronologically; file-history answers "what happened to this file across sessions." Evidence: https://github.com/rohitg00/agentmemory#readme (tools list + skills). Why: THOR's `history` is per-entity; it cannot answer "what did we do last Tuesday" or hand off a session to a colleague/fresh agent — a real cross-session drift gap, and it would supercharge THOR's existing file-touch guard (surface not just facts *naming* a file but the file's event trail). Adoption: `session` fact kind written at SessionEnd (hook already exists), keyed by session id + files touched (harvestable from hook payloads THOR already sees); `thor timeline`/`thor handoff` as MCP tools reading it. Effort: **M**.

**4. Entity knowledge graph as a third fusion stream** — opt-in (`GRAPH_EXTRACTION_ENABLED`): LLM extracts entities and directed relations ("uses," "depends-on," "conflicts-with") during consolidation; query-time entity matching routes to nearest nodes, BFS expands, and graph results join BM25+vector in RRF. Evidence: https://github.com/rohitg00/agentmemory#readme. Why: catches multi-hop/synonym recall that term-coverage reranking misses ("auth service" → fact stored under "keycloak gateway"). Note: this is a *memory-entity* graph, not the code-symbol graph THOR deliberately rejects — but it's also the least-proven mechanism (their own benchmark win came from BM25+vectors alone, graph off). Adoption: `entities`/`edges` tables populated by the consolidation pass from item 1, one-hop expansion feeding extra candidates into the existing fusion. Effort: **L**; recommend deferring — lowest evidence-per-effort here.

**5. Skills layer: action skills + source-generated reference skills** — beyond the 53 MCP tools, 15 skills teach agents *when/how* to use memory: 8 invocable actions (`/recall`, `/remember`, `/recap`, `/handoff`, `/forget`, `/commit-context`, ...) and 7 reference skills (tool tables, hooks, config, architecture) "generated from source, so they never drift," installed across 50+ agents via `npx skills add`. Evidence: https://github.com/rohitg00/agentmemory#readme. Why: tool descriptions alone underdetermine stewardship behavior; skills encode the workflow (search-before-capture, when to resolve DIVERGED) once instead of relying on each agent's judgment — cheap adoption/consistency win, and skills are portable to non-Claude agents. Adoption: ship `.claude/skills/` in the THOR repo (`/thor-recall`, `/thor-handoff`, `/thor-resolve-diverged`) plus a `thor gen-skills` subcommand emitting reference docs from the Rust tool registry. Effort: **S**.

**6. Standard-benchmark retrieval harness (LongMemEval-S)** — reproducible scripts that build a per-question index and score R@5/R@10/MRR against a public ICLR 2025 dataset. Evidence: https://github.com/rohitg00/agentmemory/tree/main/benchmark, /eval/README.md. Why: THOR's drift eval and mimir comparison are in-repo/judged; a public-dataset retrieval number makes THOR's fusion tuning falsifiable and externally comparable (and their data shows exactly where to look: preference-type questions are the hard bucket). Adoption: a `bench/longmemeval` runner that ingests each question's sessions as memories via THOR's ingest and scores recall — pure harness code. Effort: **S/M**.

### What THOR already does better
- **Per-prompt recall injection.** agentmemory injects context at SessionStart (≤2000 tokens) and re-injects at PreCompact; between those, memory only flows if the agent calls a tool. THOR's courier runs on *every* prompt with a noise gate, per-session injection ledger (no repeats, rotation), query routing, trigger words, and freshness tags — far better mid-session drift control and context economy.
- **Integrity and provenance.** Hash-chained append-only log, fsck, CAS revise/retract, divergence branching with explicit resolve, export/restore. agentmemory *auto-evicts* decayed memories and its "contradictions are detected and resolved" is LLM-mediated with no verifiable history; its dedup is exact SHA-256 within a 5-minute window vs THOR's semantic near-duplicate refusal at write time.
- **Unified code+docs+memory index.** Symbol-boundary chunking of repos into the same index as memories, with deterministic project isolation (`<project>:<path>#<n>`) + global tier and `reproject`. agentmemory indexes conversational/tool observations only — it has `memory_file_history` but no repo ingest, so "recall the actual code" isn't in scope.
- **Operational footprint.** One Rust binary + local SQLite vs a Node/iii-engine daemon holding three ports (3111/3112/3113) with an in-memory vector index; THOR's remote Streamable-HTTP server is opt-in, not required.
- **Sync with verifiable history.** Log-shipping preserves the hash chain across machines; agentmemory's `memory_mesh_sync` is shared-secret P2P state sync with no tamper-evident trail.
- **Advisory PreToolUse.** agentmemory's PreToolUse hook *captures* what's about to be touched; THOR's actively *advises* via risk rulebooks and the file-touch guard — a prevention mechanism agentmemory lacks entirely.
- **Retrieval core: roughly equivalent.** Both are BM25+dense fusion; their benchmark (+9pp from vectors over 86.2% BM25) is independent evidence for THOR's dense score-fusion path — worth making dense fusion the default (local MiniLM-class model, no API) rather than optional. Their RRF(k=60) vs THOR's query-routed rerank/priors are different fusion flavors, not a capability gap.

---

## ai-memory-mcp

### Overview
ai-memory (alphaonedev/ai-memory-mcp, Apache-2.0, Rust) is a local-first memory layer exposed four ways: MCP stdio server, HTTP REST daemon (92 route registrations / 78 unique paths on `127.0.0.1:9077`), a ~87-subcommand CLI, and curator/sync daemons — all over SQLite FTS5 (PostgreSQL+Apache AGE as an alternative backend since v0.7.0). Memories live in three TTL tiers (short 6h / mid 7d / long permanent) with auto-promotion, and at v0.9.0 it has grown heavyweight extras: 100 MCP tools at full profile, Ed25519 write attestation required by default, agent-coordination DAGs, and typed Goal/Plan/Step lifecycle memories. Its headline 97.8% R@5 on LongMemEval-S is real but should be read carefully: it ingests **one memory per session** (title = all user messages concatenated), so the retrieval task is "find the right session among ~50" — coarser than THOR's chunk-level recall — and the 97.8% smart-tier number was measured with Gemma 3 4B (a Gemma-4 re-run is an open "honesty item").

### Mechanisms THOR lacks

**1. Multi-strategy FTS5 query ladder (how pure FTS5 hits 97.0% R@5)** — Not one FTS query but a fallback cascade of four sanitizers over the same question: (a) quote-every-token OR query, (b) stopword-stripped content-words OR (70+ stopword list), (c) prefix wildcards `"tok"*` for tokens ≥3 chars, (d) adjacent-bigram phrase queries `"w1 w2" OR "w2 w3"`. Primary results keep their ranking; later strategies only fire when results < limit and only append unseen ids; fetch `k*3` then dedup. Evidence: benchmarks/longmemeval/harness_blazing.py. No decomposition, no temporal routing — the win is pure recall-floor raising: an AND-ish bm25 query that whiffs gets progressively relaxed instead of returning nothing. For THOR this directly attacks the courier's noise-gate false-negative case (weak match → inject nothing) by distinguishing "genuinely nothing" from "query too strict". Adoption: in recall.rs/courier.rs, wrap the existing FTS5 MATCH in a 3-rung ladder (current query → content-words OR → prefix), short-circuiting when the top score clears the noise gate; keep THOR's term-coverage/proximity rerank as the final arbiter so OR-relaxation can't smuggle junk past the gate. Effort: **S**.

**2. Local-LLM query expansion ("smart tier", 97.0% → 97.8%)** — Embarrassingly simple: prompt a local Ollama model (`gemma3:4b`, temp 0.3, 80-token cap) with *"Generate 8-15 search keywords/synonyms for this question. Output ONLY comma-separated keywords."*, then concatenate expansion onto the original question and run **one** OR-sanitized FTS5 MATCH (no score fusion; fallback to the unexpanded query if results are thin). Shipped as an MCP tool (src/mcp/tools/expand_query.rs) and CLI `expand` so the benchmark exercises the production path. Evidence: benchmarks/longmemeval/harness_99.py. Worth +0.8pp for zero cloud calls; for THOR it plugs the vocabulary-mismatch gap that bm25+dense fusion only partially covers (author wrote "psql pooler gotcha", user asks about "pgbouncer"). Adoption: THOR already has embed_daemon.rs talking to a local model runtime — add an optional expansion call in the courier before FTS, gated by a latency budget (their smart tier runs ~12 q/s ≈ 80ms/query) and cache expansions per session in the ledger. Author-declared "triggers" already cover the curated version of this; expansion is the automatic version. Effort: **M**.

**3. Implicit usage/recency signals in the score (six-factor formula)** — Ranking is `score = -bm25 + priority*0.5 + min(access_count,50)*0.1 + confidence*2.0 + tier_boost(+3/+1/0) + 1/(1+days*0.1)`, where `access_count` increments on every recall hit, recall extends TTL, and 5+ accesses auto-promote mid→long. Evidence: README "Six-Factor Scoring" + benchmarks/longmemeval/methodology.md. THOR's `mark` is an *explicit* helped-me prior the agent must remember to call; access-count and recency decay are *free* signals captured on every recall. Adoption: add an `access_count` column bumped in recall.rs (or a served-count derived from the courier's injection ledger, which THOR uniquely already has — served-but-never-marked could even be a *negative* prior), plus a mild `1/(1+days*k)` term fused with the existing mark prior; keep weights small so bm25 dominates. Effort: **S**.

**4. `--profile core/full` tool gating + `memory_capabilities` bootstrap + token-budget CI gate** — Default MCP surface is 7 tools; `--profile full` advertises all 100. An always-on `memory_capabilities` tool reports active tier, loaded models, per-tool `callable_now`, and can expand families at runtime (`--include-schema`), so the agent discovers rarely-needed tools on demand instead of paying their schema tokens every session. CI enforces a per-tool ceiling of 1500 cl100k tokens and an 11,000-token full-profile hard ceiling. Evidence: README (Interface Surfaces / Required CI Gates). THOR's ~13 tools cost schema tokens in *every* Claude session; realistically only `recall/remember/get/mark` are hot — `history/reproject/resolve/fsck`-class stewardship tools could be gated. Adoption: a `--profile` flag in mcp.rs choosing the advertised tool set, a tiny `capabilities` tool listing the rest, plus a CI test asserting serialized tool-schema token counts against a checked-in budget. Effort: **S**.

**5. TOON compact response format** — Recall/search/list responses default to "Token-Oriented Object Notation": field names declared once as a pipe-delimited header, values as rows; `toon_compact` is 79% smaller than the JSON equivalent (626 vs 1600 bytes for 3 memories), `toon` 61% with all fields. Evidence: docs/USER_GUIDE.md (TOON section). THOR's courier injects into every prompt, so injection token cost is a direct tax on context; a header-once tabular rendering for multi-hit recall and `brief` output is nearly free savings. Adoption: a formatter behind a `format` param; keep full bodies for `get`. Effort: **S**.

**6. First-class REST API with MCP parity** — Versioned CRUD+search HTTP surface (`GET/POST /api/v1/memories`, `GET /api/v1/search`, `POST /api/v1/recall`, `/memories/bulk`, `/consolidate`) with optional TLS/mTLS/API-key, documented per-endpoint with curl examples. Evidence: docs/API_REFERENCE.md. THOR's remote surface is Streamable-HTTP MCP only — fine for agents, awkward for cron jobs, editors, and shell one-liners. Adoption: mount a thin `/api/v1/{recall,remember,get}` JSON layer on the existing HTTP server, delegating to the same handlers as mcp.rs. Effort: **M** (small surface; auth is the real work).

### What THOR already does better
- **Integrity and history**: hash-chained append-only event log, `fsck`, CAS on revise/retract, DIVERGED branching with explicit `resolve`. ai-memory mutates rows in place (`PUT /memories/{id}`, upsert-on-(title,namespace)); its Ed25519 attestation authenticates *writers* but preserves no revision history and has no tamper-evidence on past state.
- **Unified code+docs+memory index**: symbol-boundary chunking of repos, incremental ingest, project isolation with a global tier. ai-memory stores only conversational/agent memories; it cannot answer "where is this implemented" at all.
- **Push, not pull**: per-prompt courier with score fusion, injection ledger (no repeats, rotation), noise gate, and live-file freshness tags. ai-memory is entirely pull — the agent must think to call `memory_recall`; nothing fires automatically per prompt.
- **Drift defenses with no counterpart**: pin re-injection at SessionStart *and* post-compaction, file-touch guard, Stop-hook capture nudge, PreToolUse risk rulebooks. ai-memory's TTL tiers actively *expire* memories instead of defending against context drift.
- **Capture hygiene**: typed `remember` with atomic near-duplicate *refusal* vs. their silent upsert-by-title, which can clobber a distinct fact sharing a title.
- **Focused surface**: ~13 curated tools vs. 100 (their own 11k-token CI ceiling exists because the surface bloated); THOR needs the gating idea, not the inventory.
- **Honest benchmark granularity**: THOR's judged evals run at chunk level against a real competitor (mimir) in-repo; ai-memory's 97.8% is session-level retrieval (title = concatenated user messages) — a materially easier task — and its smart-tier number is pinned to a since-replaced model (self-acknowledged in ROADMAP §11.4.A).

---

## Mem0

### Overview
Mem0 (OSS, Apache-2.0, Python + TypeScript) is an LLM-driven memory layer: `Memory.add()` runs conversation messages through an LLM extraction prompt to produce discrete facts, which are embedded and persisted to a pluggable vector store (Qdrant/Chroma/pgvector/FAISS/+20 more) with a SQLite side-DB for change history and recent messages. The famous two-phase pipeline (extract facts → second LLM call decides ADD/UPDATE/DELETE/NONE against the top-10 similar existing memories, per the ECAI 2025 paper arxiv.org/abs/2504.19413: +26% over OpenAI memory on LOCOMO, 91% lower p95 latency, >90% token savings vs full-context) was **replaced in April 2026 (v2/V3 pipeline)** by single-pass ADD-only extraction plus spaCy-based entity linking and multi-signal retrieval (semantic + sigmoid-normalized BM25 + entity boost); the external graph store (Neo4j/Memgraph/Kuzu, "Mem0g") was likewise dropped from OSS in favor of the built-in entity graph. Retrieval is app-invoked search, not injection — there is no hook layer.

### Mechanisms THOR lacks

**1. Automatic fact extraction from conversation (the extract→update pipeline)** — Classic pipeline: LLM call #1 (`FACT_RETRIEVAL_PROMPT`) turns the last messages into candidate facts; each fact is embedded, top-10 similar memories fetched, and LLM call #2 (`DEFAULT_UPDATE_MEMORY_PROMPT`, still shipped in mem0/configs/prompts.py) emits per-fact ADD ("new information not present"), UPDATE ("already present but totally different"), DELETE ("contradicts existing memory"), or NONE. The 2026 V3 pipeline (README, "New Memory Algorithm (April 2026)") collapses this to one LLM call with `ADDITIVE_EXTRACTION_PROMPT`: existing memories + last-20 messages + observation date are injected for dedup and `linked_memory_ids`; no UPDATE/DELETE; relative dates ("last week") are rewritten to absolute dates. This claims +21 pts on LoCoMo. **Why for THOR:** manual/nudged capture is THOR's biggest miss-rate risk — facts stated mid-session evaporate at compaction. **Adoption:** NEEDS an LLM; no good pure heuristic exists for extraction. Fit for THOR: a background/Stop-hook pass where the *agent* (already in the loop, zero extra infra) is handed the additive-extraction prompt with `recall` results as the dedup context, proposing `remember` calls the user can veto — keeping the hot path LLM-free. Notably mem0 abandoning LLM-decided UPDATE/DELETE as lossy vindicates THOR's append-only revise/retract model; adopt the *extraction* half only. Effort **M**.

**2. Entity linking + entity-boosted retrieval (graph memory successor)** — Fully heuristic, no LLM: mem0/utils/entity_extraction.py uses spaCy to pull PROPER (capitalized sequences), QUOTED, TOPIC (noun compounds), IDENTIFIER entities from each memory; unique entities are embedded into a parallel entity collection storing `linked_memory_ids`, deduped by cosine ≥ 0.95. At search, query entities are matched against the entity store and linked memories get up to +0.5 boost fused with semantic and BM25 in `score_and_rank()` (mem0/utils/scoring.py), with an adaptive divisor per active signal. Docs: docs.mem0.ai/platform/features/graph-memory (co-occurrence graph, no typed edges — the old LLM triple-extraction Mem0g bought only ~2% in the paper). **Why for THOR:** entity-centric queries ("everything about the FTS5 tokenizer", "what do we know about service X") currently depend on BM25 term overlap; an entity table linking facts↔code-chunks across the unified index gives cheap multi-hop recall. **Adoption:** perfect fit — a Rust NER/noun-phrase pass (or capitalization+quoted-span heuristics + THOR's existing trigger-word machinery generalized) writing an `entities(entity, memory_id)` FTS5 table; boost as a third fusion signal. Effort **M**.

**3. Retrieval-reinforcement memory decay** — Per docs.mem0.ai/platform/features/memory-decay: each memory keeps last-retrieved time + access history (capped at 20 touches); search widens the pool to `top_k×3` (floor 50), multiplies each score by a 0.3×–1.5× recency-of-use factor (just accessed ≈1.5×, idle months ≈0.3× floor — never filters out), re-sorts, then fire-and-forget increments access history on returned memories; legacy memories fall back to `updated_at` as one touch. Platform-only today, but purely heuristic. **Why for THOR:** THOR's `mark` is a manual helpfulness signal; automatic use-frequency bias would demote stale decisions in crowded projects without deleting anything — directly strengthens the noise gate and the injection budget. **Adoption:** trivially local — `accessed_at`/`access_count` in the projection (or as ledger events so it stays audit-clean), multiplied into the existing score fusion; injection-ledger hits can double as touches. Effort **S**.

**4. Expiration dates (hide, don't delete)** — In OSS since v2: `add(..., expiration_date="YYYY-MM-DD")`; `search()`/`get_all()` skip expired records (evaluated in UTC, date-inclusive, malformed dates fail open, `show_expired=True` to override), `get(id)` still returns them, clearing the date restores visibility (docs.mem0.ai/platform/features/memory-expiration; `_normalize_expiration_date`/`_is_expired` in mem0/memory/main.py). **Why for THOR:** coding memories are full of known-temporary facts ("pin to v1.9 until upstream fix", "flag X during migration") that currently need a manual retract someone forgets. **Adoption:** ideal fit for append-only — an optional `expires` field on `remember`/`revise`; recall and the auto-recall hook filter expired heads; history keeps everything. Zero LLM. Effort **S**.

**5. Criteria retrieval (weighted custom relevance scoring)** — Platform-only: project-level criteria, each `{name, description, weight}`; at search time an LLM scores every semantically-retrieved candidate against each criterion description and results are re-ranked by the weighted sum, redefining "relevance" per application (docs.mem0.ai/platform/features/criteria-retrieval). **Why for THOR:** THOR's query-routed ranking is fixed logic; declarative, user-tunable ranking dimensions (e.g. weight "safety-critical gotcha" over "style preference" for edit-heavy prompts) would let projects tune injection without forking rank code. **Adoption:** as-designed it NEEDS an LLM per search — unacceptable in THOR's hot path. Two substitutes: (a) score at *write* time (agent assigns criterion scores during `remember`, stored as facts' metadata, applied as cheap multipliers at recall) or (b) a local cross-encoder (mem0's own OSS `SentenceTransformerReranker`, `cross-encoder/ms-marco-MiniLM-L-6-v2`, in mem0/reranker/) scoring query×memory pairs offline-downloaded, ~20ms on CPU for a short candidate list. Effort **M**.

**6. Calibrated BM25 normalization for score fusion** — Small but directly liftable: `get_bm25_params()` picks sigmoid `(midpoint, steepness)` from lemmatized query term count (≤3 terms → (5.0, 0.7) … >15 → (12.0, 0.5)), squashes unbounded BM25 into [0,1] before additive fusion, and gates on the *semantic* threshold before combining so keyword noise can't resurrect irrelevant hits (mem0/utils/scoring.py; lemmatization of both stored text and query in mem0/utils/lemmatization.py). **Why for THOR:** raw FTS5 BM25 scores are corpus- and query-length-dependent; naive min-max or rank fusion misweights short queries — this is a concrete, tested calibration for THOR's bm25+dense fusion. Pure math, no LLM. Effort **S**.

### What THOR already does better
- **Integrity and portability:** hash-chained append-only log + fsck + export/restore + log shipping. Mem0's audit trail is a plain mutable SQLite `history` table (no chaining, no verification), and its classic DELETE really deleted vectors; mem0 has no sync story in OSS at all.
- **Conflict semantics:** explicit divergence (branch on conflict, `resolve` with CAS, `[DIVERGED]` surfacing). Mem0's classic pipeline let an LLM silently overwrite/delete on perceived contradiction — and mem0 itself retreated to ADD-only in 2026, converging on THOR's accumulation model but *without* any conflict/heads representation, so contradictory facts now just coexist unranked.
- **Injection layer:** mem0 is pull-only (app calls `search()`); THOR's per-prompt auto-recall hook with reserved constraint slot, session injection ledger, noise gate, trigger words, and drift hooks (SessionStart/post-compaction pins, file-touch guard, PreToolUse advisory, Stop-hook nudge) has no mem0 equivalent.
- **Code-aware unified index:** repos + docs + memories in one index with live-file freshness and project isolation + global tier; mem0 indexes conversation-extracted facts only (user/agent/run scoping, no code awareness).
- **Typed constraints:** first-class gotcha/decision/preference with duplicate refusal, revise/retract lifecycle; mem0 facts are untyped strings (categories/immutability are paid-platform features).
- **Deterministic, LLM-free hot path:** every mem0 `add` costs at least one cloud LLM call and is non-deterministic (they raise `LLMError` on parse failure); THOR's write and recall paths are reproducible and offline. OSS mem0 also gates temporal reasoning and decay behind the paid platform.

---

## Letta / MemGPT

### Overview
Letta (formerly MemGPT) is an agent *runtime* built around the MemGPT idea: treat the LLM context window like an OS treats RAM — a small, always-visible "core memory" that the agent edits with tools, backed by unbounded "external memory" tiers (archival passages with embeddings, and full recall of conversation history) that the agent pages data in from via search tools. Its signature additions over vanilla RAG are agent-driven memory self-editing, an explicit memory-pressure/eviction pipeline, and *sleep-time agents* — background agents that share the primary agent's memory blocks and reorganize them asynchronously.

### Mechanisms THOR lacks

**1. Sleep-time memory reorganization (background "memory steward" agent)** — A sleeptime agent is attached to the primary agent as a group; every N turns (`sleeptime_agent_frequency`, checked as `turns_counter % frequency == 0`) it is fired asynchronously with a transcript delta — all messages between `last_processed_message_id` and the current response. It sees the primary agent's memory blocks rendered with line numbers and edits them with `memory_replace` / `memory_insert` / `memory_rethink` (full-block rewrite), then calls `memory_finish_edits`. Its system prompt is explicitly a consolidation policy: integrate new facts, delete outdated/redundant ones, convert relative dates to absolute, "be selective — not every observation warrants an edit." Evidence: letta/groups/sleeptime_multi_agent_v2.py, letta/prompts/system_prompts/sleeptime_v2.py, docs.letta.com/guides/agents/architectures/sleeptime/, paper arXiv:2504.13171. Why it helps THOR: `thor consolidate` already *finds* duplicate twins, decay candidates, and same-topic clusters, but nothing ever acts on them except mechanical dedup — the store's entropy is bounded only by how often a human asks the agent to clean up. Sleep-time closes that loop and moves consolidation cost off the interactive path. Adoption: this genuinely needs LLM calls, but not an agent runtime — a THOR-shaped substitute is a scheduled/Stop-hook-triggered headless pass (`claude -p` or a background task) that feeds the `thor consolidate` report plus fact bodies to the agent with only THOR MCP tools (`revise`/`retract`/`resolve`/`reproject`/`pin`) and a sleeptime_v2-style prompt; every change lands in the hash-chained log, so the steward is fully auditable and revertible — something Letta itself cannot offer. Rate-limit it (per-N-sessions, like Letta's frequency knob) to control cost. Effort: **M**.

**2. Size-limited, labeled core-memory blocks** — A `Block` has `label`, `value`, `limit` (char budget), `description` (tells the agent what belongs in the block), and `read_only`. Blocks are compiled into every prompt; writes that would exceed `limit` fail, forcing the agent to compress or demote content to archival. Blocks can be shared between agents by ID (this is how the sleeptime agent edits the primary's memory). Evidence: letta/schemas/block.py, docs.letta.com/guides/agents/memory-blocks/. Why it helps THOR: THOR's pins are an open-ended *set* of individually pinned facts; there is no budget pressure, so pin sets grow monotonically and dilute the SessionStart injection. A fixed-budget block ("project charter", "current working set") makes prioritization the agent's problem and gives injection a hard token ceiling. Adoption: pure layer work, no runtime needed — add named per-project blocks in SQLite (versioned as events in the existing chain), render them in the SessionStart/post-compaction injection ahead of ranked recall, and expose `block_write`/`block_append` MCP tools that refuse over-budget writes with "over limit by N chars: compress or move detail to a regular memory." Effort: **S/M**.

**3. Memory-pressure warning before eviction** — In letta/agent.py: when `current_total_tokens > summarizer_settings.memory_warning_threshold * context_window`, Letta injects a one-shot token-limit warning message into the conversation (guarded by an `agent_alerted_about_memory_pressure` flag so it fires once per pressure episode) telling the agent history will soon be trimmed — i.e., an explicit *"persist what matters now"* signal before eviction. Only on actual overflow does `summarize_messages_inplace` run; the Summarizer then evicts a configurable fraction of oldest messages, replacing them with an LLM-written recursive summary while the originals stay queryable in recall storage. Evidence: letta/agent.py, letta/services/summarizer/summarizer.py, MemGPT paper arXiv:2310.08560. Why it helps THOR: THOR's drift hooks are *reactive* — pins are re-injected after Claude Code compaction, but any un-captured decision made mid-session dies in the compactor. The valuable design element is the pre-eviction signal with a fire-once flag. Adoption: trivial fit — a PreCompact hook (Claude Code supports it) emitting an advisory: "context is about to be compacted; persist durable decisions/gotchas via `remember` now," cross-checked against the session injection ledger so it can name what was surfaced-but-never-captured. No LLM calls, no runtime. Effort: **S**.

**4. Recall memory: the full interaction history as a searchable tier** — Every message ever exchanged is persisted; the context window is just a compiled view over DB state. `conversation_search(query, roles, start_date, end_date, limit)` does hybrid text+semantic search over all past messages with role and ISO-8601 date filters, so "what did we decide last Tuesday" is answerable even if nobody wrote a memory. Evidence: letta/functions/function_sets/base.py. Why it helps THOR: THOR is curated-facts-plus-index; anything the agent didn't `remember` is gone. An episodic tier is the safety net under the capture nudge. Adoption: no runtime needed — Stop hook (or scheduled job) ingests Claude Code's session transcript JSONL into a separate low-ranked `episode` kind in the existing index; excluded from auto-recall by default (noise gate) but reachable via `recall kind:episode` with date filters. Storage-cheap in SQLite; keep episodes out of the hash chain (they're raw log, not stewarded facts). Effort: **M**.

**5. Graduated memory-edit tool grammar with line-numbered views** — Sleeptime editing tools distinguish surgical edits from rewrites: `memory_replace(label, old_string, new_string)` (exact-match, docstring warns against replacing long spans), `memory_insert(label, new_string, insert_line)`, and `memory_rethink(label, new_memory)` reserved for "large sweeping changes"; blocks are shown with line numbers that the tools actively reject if echoed back, preventing a whole class of malformed edits. Evidence: letta/functions/function_sets/base.py. Why it helps THOR: THOR's `revise` is whole-fact replacement; for longer facts (or the blocks from #2) whole-body rewrites by the agent are where silent information loss happens, and each rewrite bloats the event log. Adoption: add a patch-style `revise` variant (old_string/new_string against the current head, applied server-side, still producing a normal revision event) — mostly valuable if #2 lands. Effort: **S**.

### What THOR already does better

- **Auditability and integrity**: hash-chained append-only log with branch/resolve, fsck, export/restore. Letta memory blocks are mutable DB rows — a sleeptime agent can destroy information with no diff, no history, no undo.
- **Local-first, zero-infrastructure**: Rust+SQLite layer inside an existing harness vs. Letta's server + Postgres + embedding service + its own agent runtime you must adopt wholesale.
- **Retrieval machinery**: bm25+dense fusion, query-routed ranking, reserved constraint-fact slot, trigger words, noise gate, live-file freshness, and a session injection ledger preventing repeat injections. Letta's `archival_memory_search` is single-mode vector search with tag/date filters (default page size 5) — no fusion, no injection dedup, no typed-constraint slot.
- **Write-time hygiene**: typed `remember` with near-duplicate refusal; Letta's `archival_memory_insert` inserts duplicates freely and relies on later sleep-time cleanup.
- **Code-native scope model**: unified repo+docs+memories index with project isolation plus a global tier; Letta's file/source blocks are open-file views, not a ranked cross-corpus index.
- **Drift control at the harness level**: pinned standing rules re-injected at SessionStart *and* post-compaction, file-touch guard, PreToolUse advisory guard, Stop-hook capture nudge — Letta has no equivalent because it assumes it *is* the runtime and its blocks never leave context.

---

## Zep / Graphiti

### Overview
Graphiti (the OSS engine behind Zep) ingests raw **episodes** (messages/JSON/text) and uses an LLM with structured output to extract entity nodes and relationship edges ("facts" as triplets), deduplicating both against the existing graph on every incremental update — no batch recompute. The graph is three-tiered (episodes → semantic entities/edges → communities) and **bi-temporal**: every edge carries `valid_at`/`invalid_at` (when the fact was true in the world) plus `created_at`/`expired_at` (when the system learned/retired it), so contradicted facts are *invalidated, not deleted*, and both "what's true now" and "what was true at time T" are queryable. The paper reports 94.8% on DMR (vs. MemGPT 93.4%) and up to +18.5% accuracy / ~90% latency reduction on LongMemEval (arxiv.org/abs/2501.13956).

### Mechanisms THOR lacks

**1. Bi-temporal validity intervals (`valid_at`/`invalid_at` + `created_at`/`expired_at`)** — Each edge stores two timelines: event time (when the fact held in reality) and transaction time (when the system recorded/expired it). `valid_at` is LLM-extracted from episode text or defaulted to episode time; `invalid_at` is set when a later contradicting fact arrives; `expired_at` marks system-side retirement. Evidence: graphiti README, edge_operations.py. *Why for THOR:* this is the direct fix for THOR's known failure mode — a fact the user forgot to revise currently injects as fully current; with intervals, recall can rank/annotate by validity (`[expired]`, `[valid as of 2026-03]`) instead of relying on explicit `revise`/`retract`. THOR's revision chain already gives transaction time; only event time is missing. *Adoption without cloud LLM:* two nullable columns on the fact head + a `[expired]` recall filter is pure schema work; `valid_at` defaults to write time; heuristic extraction (date/version regexes, "since/until/as of" patterns, chrono-style parsing) covers much of coding-agent text. LLM is load-bearing only for inferring implicit validity from free prose — acceptable to skip. **Effort: S** (schema+recall) to **M** (extraction heuristics).

**2. Automatic contradiction detection with edge invalidation** — On every new edge, `resolve_extracted_edge()` fetches semantically related existing edges (same endpoints), fast-paths exact matches, then asks the LLM for `contradicted_facts` indices; `resolve_edge_contradictions()` sets `expired_at = now` and `invalid_at = new edge's valid_at` on the losers. Old facts survive for point-in-time queries. Evidence: edge_operations.py, dedupe_edges.py prompt. *Why for THOR:* today invalidation is 100% manual; if the agent/user misses it, the stale head keeps winning recall forever. Even a conservative auto-flag would cut stale injections measurably. *Adoption without cloud LLM:* at `remember` time, reuse the existing bm25+dense recall to find near-neighbors, then: (a) **typed constraint facts** with the same key but different value → deterministic contradiction, auto-supersede; (b) same subject slot + numeric/version/path mismatch or negation pattern → tag the old head `[contradicted?]` and surface it in `brief`/recall rather than hard-invalidating — this maps cleanly onto THOR's existing DIVERGED+resolve flow (create a branch, let the agent resolve). A small local NLI cross-encoder (ONNX, ~20M params) can score contradiction for free-text pairs. LLM is load-bearing only for subtle prose contradictions. **Effort: M.**

**3. Entity layer with resolution (facts anchored to deduplicated nodes)** — Graphiti extracts entities per episode, resolves them against existing nodes (embedding + fulltext candidates, LLM dedup with structured output), and maintains evolving per-entity summaries; facts hang off stable entity identities, enabling "everything known about X" retrieval and multi-hop traversal. *Why for THOR:* contradiction detection (#2) and stale-fact grouping both get far more precise when facts share an entity anchor — "these 4 facts are all about `auth.rs`/`jwt_secret`" beats pure text similarity. *Adoption without cloud LLM:* in the coding domain entities are mostly syntactic — file paths, symbols (tree-sitter/ctags), crate/package names, env/config keys, URLs — extractable deterministically; dedup by normalized-name + embedding threshold. THOR already has typed constraint facts; this generalizes the key side. LLM load-bearing only for abstract concepts in prose memories. **Effort: L.**

**4. Graph/frequency-aware reranking: episode-mentions, node-distance, MMR** — Beyond RRF over BM25+cosine (which THOR has), Graphiti ships rerankers: **episode_mentions** (edges mentioned by more episodes rank higher — automatic salience), **node_distance** (rerank by graph hops from a center node, i.e., contextual proximity), and **MMR** for diversity in a fixed budget. Evidence: search_config_recipes.py. *Why for THOR:* per-prompt injection has a tight token budget behind a noise gate; MMR stops near-duplicate facts crowding it out, and a "file-proximity" analog of node_distance (boost facts co-occurring with files touched this session — THOR already tracks file touches and a session ledger) makes recall context-following. Episode-mentions is the automatic version of THOR's manual `mark`: count how often a fact was recalled-and-marked/co-retrieved and fold it into ranking. *Adoption:* all pure math/SQL, zero LLM. **Effort: S.**

**5. Community detection with dynamic label propagation** — Entities are clustered via label propagation (chosen over Leiden specifically because new nodes can be assigned incrementally to their neighbors' majority community, deferring full recomputes); each community gets a hierarchically LLM-summarized `CommunityNode` searchable as a coarse retrieval tier. Evidence: community_operations.py, arxiv.org/abs/2501.13956. *Why for THOR:* gives `brief` a real map ("12 facts about the migration system, 3 diverged") and a place to spot stale clusters (community whose underlying files all changed → sweep candidates). *Adoption without cloud LLM:* label propagation over a fact/entity co-occurrence graph is trivial locally; summaries are the honest gap — the LLM is load-bearing there. Substitute: show top-N representative fact titles + shared tags per cluster, or let the *host coding agent* write the summary during a `brief`/consolidation turn (LLM already present, but off the ingest hot path). **Effort: M** (clusters) / **L** (summaries).

### What THOR already does better
- **Lossless, verifiable provenance**: hash-chained append-only log with CAS revise, branch-on-conflict (DIVERGED) and explicit `resolve`. Graphiti's mutations (dedup, invalidation, summary rewrites) are LLM-driven and nondeterministic — a wrong invalidation is silent, with no cryptographic chain or human-in-the-loop conflict state.
- **No LLM in the hot path**: Graphiti needs multiple structured-output LLM calls per episode (extract nodes, dedup nodes, extract edges, dedup+contradict, timestamps, summaries); THOR's ingest and recall are deterministic, offline, and cheap.
- **Local-first ops**: single SQLite file with export/restore and cross-machine sync vs. a Neo4j/FalkorDB/Neptune server plus embedder + LLM service dependencies.
- **Delivery into the agent loop**: per-prompt auto-recall with query routing, trigger words, noise gate, session ledger, and drift hooks (SessionStart/post-compaction pins, file-touch guard, PreToolUse guard). Zep/Graphiti is a retrieval API the agent must remember to call.
- **Working-tree freshness**: `[refreshed]`/`[stale?]` tags tied to actual file state — Graphiti has no linkage between facts and a live codebase.
- **Scoping**: project isolation + global tier and `reproject`; Graphiti's group_id namespacing is flat by comparison, with no repo/doc/memory unified ingest.

---

## cognee

### Overview
Cognee (topoteretes/cognee, ~27.5k stars, Python) is an ECL memory engine: `add` extracts documents/code into typed Data objects, `cognify` runs an LLM pipeline (`classify_documents → extract_chunks_from_documents → extract_graph_and_summarize → add_data_points`) that turns chunks into an entity/relation knowledge graph plus embeddings, and load persists both into pluggable graph/vector backends (SQLite+LanceDB+Kuzu locally, or a single Postgres/pgvector instance, or Neo4j/Qdrant/etc.). Retrieval is a registry of ~20 strategies (17 `SearchType`s including GRAPH_COMPLETION, TRIPLET_COMPLETION, TEMPORAL, CODING_RULES, CHUNKS_LEXICAL/BM25) with auto-routing, and a `memify`/improve pipeline enriches memory after the fact.

### Mechanisms THOR lacks

1. **Deterministic graph-expansion retrieval** — cognee seeds retrieval with vector hits, then walks graph edges to pull in neighboring nodes/triplets (graph_completion_context_extension_retriever.py, triplet_retriever.py). THOR's bm25+dense recall only returns what lexically/semantically matches the query; it cannot answer "what else is connected to this fact." THOR already has latent edges that need no LLM: revision chains, same-file/same-symbol provenance, tag co-occurrence, session-ledger co-recall. Materialize those into an SQLite `edges` table and do 1-hop expansion (score-damped) after the normal ranked recall, behind the existing noise gate. Effort: **M**.

2. **Temporal pipeline + time-aware search routing** — `temporal_cognify=True` swaps in `extract_events_and_timestamps → extract_knowledge_graph_from_events`, storing events with time anchors, queried via `SearchType.TEMPORAL`. THOR's hash-chained log has perfect timestamps but its query router doesn't exploit them: "what did we decide last week" ranks no differently than any query. Add deterministic time-window parsing ("yesterday", "since v2", ISO dates) to the query router as a recall filter/boost over the event log — no LLM needed since THOR's provenance is already exact. Effort: **S**.

3. **Offline rule distillation (memify coding-rule associations)** — `add_rule_associations()` LLM-extracts durable developer rules from ingested content, stores them as `Rule`/`RuleSet` nodes with `rule_associated_from` edges back to the exact source chunks, retrievable via coding_rules_retriever (cognee/tasks/codingagents). THOR's pins are entirely user/agent-initiated; recurring conventions buried across many memories never get promoted. Adopt as an explicitly-invoked batch command (`thor distill`) using a local model (Ollama) that proposes pin candidates with provenance links to source facts — human confirms, hot path stays LLM-free. Effort: **M**.

4. **Hierarchical summary layer as a searchable tier** — `extract_graph_and_summarize` stores per-chunk/per-document summaries as first-class indexed objects, searchable via `SearchType.SUMMARIES`/`GRAPH_SUMMARY_COMPLETION`. THOR recalls at chunk/fact granularity only; broad questions ("how does auth work here") match scattered fragments instead of one coarse unit. Add optional batch summarization at file/module boundaries (symbol chunker already knows the boundaries), indexed in the same bm25+dense store with a `tier=summary` facet the query router can prefer for broad queries. Effort: **M**.

5. **Schema-constrained extraction (custom graph_model + ontology)** — cognify accepts a caller-supplied Pydantic `graph_model` and an `ontology_config.ontology_resolver`, so extraction is validated against a domain schema rather than free-form. THOR's typed constraint facts (gotcha/decision/preference) are a flat 3-type taxonomy. A user-extensible fact-type schema (fields + validation per type, e.g. `api_contract{endpoint, invariant}`) would make constraint injection more precise without any graph machinery. Effort: **S/M**.

## MemPalace

### Overview
MemPalace (github.com/MemPalace/mempalace, Python 3.9+, MIT) is a local-first agent memory that stores conversation history **verbatim** — no summarization or extraction by default — organized as a "palace": wings (people/projects) → rooms (topics) → drawers (original text), over pluggable backends (ChromaDB default; SQLite, Qdrant, Milvus, pgvector) with fully local embeddings (embedding-gemma-300m or all-MiniLM-L6-v2). It ships 35 MCP tools, auto-save hooks for Claude Code/Cursor/Codex, a SQLite temporal knowledge graph, and a committed benchmark harness (96.6% R@5 raw / 98.4% hybrid on LongMemEval with zero LLM calls).

### Mechanisms THOR lacks

1. **Verbatim transcript capture with pre-compaction hooks + idempotent sweep** — hooks save conversation turns periodically and *before context compression*; `mempalace sweep` retroactively converts existing transcripts into per-message drawers, resume-safe and idempotent (hooks/ dir). THOR's capture nudge yields curated facts, meaning the exact wording of a decision or error message is lost once the session compacts. Add a PreCompact/Stop hook writing raw turns into a separate `verbatim` SQLite table (append-only, so it can even join the hash chain), indexed by the existing bm25+dense pipeline under a low-priority tier the noise gate keeps out unless the query demands exact recall. Effort: **S/M**.

2. **Topic-level scoping below project (rooms)** — searches are scoped to wing→room rather than run against a flat corpus, cutting false positives structurally instead of by ranking alone. THOR scopes at project + global only; a big project's memories are one flat pool. A lightweight `topic` facet (assigned at remember-time or inferred from file paths/tags) that the query router can filter on would sharpen per-prompt recall precision. Effort: **S**.

3. **Relation validity windows with invalidate/timeline queries** — a SQLite-backed temporal entity-relationship graph supporting add, query, **invalidate**, and **timeline** operations, so "X was true from A until B" is queryable. THOR's revise/retract supersedes facts but recall sees only current heads; "what did we believe in March" requires manual history spelunking. THOR already stores everything needed — expose `valid_from/valid_to` derived from the revision chain and add an as-of-time recall mode. Effort: **S/M**.

4. **Committed, reproducible benchmark harness** — `benchmarks/` contains LongMemEval (500 q), LoCoMo (1,986 q), ConvoMem, and MemBench harnesses with reproduction commands in benchmarks/BENCHMARKS.md and result files checked in. THOR has no public-dataset way to measure whether a ranking/noise-gate change helps or regresses auto-recall. Port LongMemEval-style R@k evaluation against THOR's recall API plus a THOR-specific injected-context precision metric; run in CI. Effort: **M**.

### What THOR already does better
- **Tamper-evident lossless history**: hash-chained append-only log with branching-on-conflict, resolve, export/restore, and cross-machine sync — cognee has mutable graph state, MemPalace has verbatim text but no integrity chain or conflict model.
- **Zero LLM in the hot path, guaranteed**: cognee's cognify and most of its best retrievers require LLM calls per ingest/query; MemPalace's top-tier accuracy needs LLM reranking. THOR's per-prompt recall is fully deterministic, fast, and offline.
- **Code-native indexing**: symbol-boundary chunking, unified repos+docs+memories in one bm25+dense index, incremental updates, live-file freshness. Cognee's coding support is rule extraction, not symbol-aware code search; MemPalace is conversation-centric with no code chunker.
- **Push, not pull**: per-prompt auto-recall with query routing, trigger words, session ledger, and a noise gate injects memory without the agent asking; both others are predominantly MCP-pull.
- **Active drift defense**: pins, file-touch guard, PreToolUse guard, capture nudge — neither project intervenes in the agent's action loop.
- **Operational footprint**: one Rust binary + SQLite vs. cognee's Python + graph/vector DB stack and MemPalace's Python + ChromaDB + model downloads.
- **Memory stewardship lifecycle**: typed revise/retract/resolve/mark/reproject with duplicate refusal at remember-time; cognee's forget is dataset-level, MemPalace's model is accumulate-and-search.

---

