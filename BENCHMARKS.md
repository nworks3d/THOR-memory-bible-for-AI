# THOR vs mimir - the honest picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.
Every test was re-measured fresh on 2026-07-10, after the v3 "metabolism"
round (consolidate, usage strength, trigger tags, guard anchors, doc crumbs),
with an independent blind jury - no number below is carried over from an
earlier run. Before measuring, BOTH stores got a hygiene pass: THOR's store was
cleaned with its own `thor consolidate` (dual-write twins and stale notes
retracted, decay list empty, fsck green), and mimir keeps the project
documentation it was previously given - so every number below is a fair fight,
not a coverage accident.

![THOR vs mimir - coverage, quality, drift and speed](assets/benchmark.svg)

## Why two tests, not one

THOR and mimir make a different design choice about **code**, and a single number
would hide it:

- **THOR ingests your repositories** - source, docs and memories - into **one**
  append-only index that auto-recall searches on every prompt. For one real
  project THOR holds **2543 facts, of which 1532 are source-code chunks**.
- **mimir's `recall` serves memories and docs**; source code lives in a **separate
  code-symbol graph** (`graph` / `outline` / `peek`) you query explicitly - it is
  **not** auto-injected at a prompt. For that same project mimir's store holds
  **2 marker nodes and 0 source chunks**.

Neither is wrong - they are different products. So we run **two separate, fair
tests**:

1. **As-deployed coverage** - the product as you actually run it. THOR's whole
   thesis is to replace *both* the repo knowledge *and* the memory tool, so its
   ingest is part of the measurement.
2. **Same knowledge** - pure retrieval quality on an **equal corpus**: only facts
   that **both** systems have. This isolates the ranking algorithm from coverage.

Both are blind-judged: each system returns its top hits, an independent judge
scores each set **0-2** for answer-presence (2 = a hit clearly contains the
answer, 1 = on-topic, 0 = miss), **blind** to which system produced which (sets
relabelled A/B, ids stripped, scored on content alone). The question corpus
references private project internals, so only the aggregate scores are published.

## Test 1 - As-deployed coverage (200 questions)

What the agent actually gets from a deliberate recall. The set is 118
shared-knowledge questions (facts both stores hold) plus 82 category-stratified
coverage questions - deliberately balanced toward mimir's home turf, unlike the
earlier 504-question set that was dominated by code-only questions.

| category | THOR | mimir |
|---|---:|---:|
| code-structure | **51.5%** | 45.5% |
| code-behavior | **69.2%** | 50.0% |
| doc-reference | **70.0%** | 60.0% |
| config how-to | 73.5% | **76.5%** |
| gotcha | 71.7% | **73.9%** |
| decision | **68.5%** | 57.4% |
| **overall (n=200)** | **67.0%** | **57.2%** |

THOR leads overall and in four of six categories; mimir edges the gotcha and
config-how-to categories. The earlier headline gap (67% vs 38% on the 504-set)
was mostly coverage over code-only questions - this balanced set is the fairer
picture, and THOR leads by ~10 points, the same margin across both same-day
juries.

## Test 2 - Same knowledge (118 facts both systems have)

The fair, apples-to-apples comparison: only questions whose source fact is a
dual-written memory or a doc chunk **both** stores hold.

**Overall (n=118): THOR 61.4% vs mimir 53.8%** - on the broad shared set THOR
leads by +8 points, thanks to score-fusion plus the query-routed class prior
(knowledge-phrased questions give hand-written facts a small edge over the wall
of same-topic code chunks). **On the strictest cut - only dual-written
memories, where there is zero doubt both stores have the fact (n=53) - mimir
still wins, 94.3% vs 91.5%** - though the gap narrowed from ~7 points to ~3
after the v3 round (author-declared trigger tags plus heading crumbs). Pure
memory recall over a small, clean set of hand-written notes remains mimir's
home turf, consistently across four fresh juries (94.3-89.6 vs 91.5-82.1);
THOR's breadth is the counterweight, not a substitute.

## Test 3 - Multi-project (three private project repos seeded)

After ingesting three private project repos into THOR - Project 1, Project 2, and
Project 3 - each scoped and isolated, we asked whether both systems can answer real
questions about *each* project. 15 questions per project (45 total) were written by
an agent reading the repo itself (ground truth, **not** THOR's store), each with a
gold answer. Both systems were scoped to the project (THOR `--project <key>`, mimir by
the project's working dir); the top-5 retrieved chunks were pulled in full and judged
**blind by a 3-judge majority** (the judge never knows which system is which).

| project | THOR | mimir |
|---|---:|---:|
| Project 1 | 90% | **97%** |
| Project 2 | **97%** | 93% |
| Project 3 | **93%** | 77% |
| **overall (n=45)** | **93.3%** | **88.9%** |

mimir keeps the documentation of Project 2 it was deliberately given one run
earlier (it had scored 0% there by absence - a coverage accident, not a
ranking result). On the level playing field THOR wins overall (93% vs 89%) and
takes Project 2 (97% vs 93%) and Project 3 (93% vs 77%, where full source
ingest beats a docs-only view). **mimir still edges Project 1, 97% vs 90%**:
hand-curated architecture/bring-up docs remain a strong retrieval substrate on
design questions (the gap was 26 points two improvement rounds ago).

## Session drift compensation (73 scenarios, 3-way)

This is what THOR is *for*: at the start of a fresh session (empty context, just
after a compaction), does memory surface the one fact that stops the agent
drifting into a mistake? Each scenario is a realistic task where an agent that
has *forgotten* a gotcha or decision would violate it - the prompt never names
the constraint, so memory must connect the task to it on its own. Measured three
ways, with precise channel definitions: THOR's **courier** (the real as-deployed
auto-injection hook, project-scoped, including its noise gates), THOR's
**deliberate recall** (the fused path over every project - what the MCP recall
tool serves), and mimir searching **every** project (`--all`, its best case).

| metric | THOR courier (as-deployed) | THOR recall (deliberate) | mimir (--all) |
|---|---:|---:|---:|
| preventer surfaced (>=partial) | **65.8%** | 63.0% | 53.4% |
| clear catch (fully surfaced) | **45.2%** | 41.1% | 41.1% |

The as-deployed courier leads BOTH metrics - two improvement rounds earlier it
caught only 19.2%, losing to mimir. It now even beats THOR's own deliberate
recall: author-declared **trigger tags** (a `fires-when` footer field stored
with the fact) let a constraint compete from below the relevance floor at
exactly the moment its task words appear. Absolute numbers moved down several
points for BOTH systems against the morning jury (strictness variance); the
relative picture held. The channels built for the post-compaction window on
top of this - **pinned rules** (re-injected in full by construction) and the
**file-touch + command guards** (fired at the moment of action, now with exact
author-declared `anchors`; 16/16 surfaced on the committed corpus) - are
reported via the in-repo reproducible measurement:
`cargo run --example drift_eval`.

## Speed and cost

Full per-prompt cost (process start + recall), warm daemon, median of 20 runs,
same (long, realistic task-prompt) query set for both, same machine:

| | THOR | mimir |
|---|---:|---:|
| latency (warm, median) | **163 ms** | 246 ms |
| tokens injected / prompt (same set) | **~435** | ~903 |
| resident RAM | ~570 MB (semantic daemon) / **0** (bm25 default) | ~700 MB observed |

THOR is **~1.5x faster** per prompt on this set - a single native binary with no
wrapper process - while injecting **~2.1x fewer tokens** (tight density-chosen
snippets against mimir's longer bodies). Absolute numbers move with prompt
length and machine load between runs (83-268 ms observed across sets); the
ratio is what carries. THOR's default bm25 mode needs no resident process at
all; the optional semantic mode keeps a warm ~570 MB embedder resident.

## What each is built for (structural)

| property | THOR | mimir |
|---|---|---|
| Unified auto-recall over code + docs + memory | **yes** (chunks source into recall) | no (recall = memory + docs) |
| Code-symbol graph (which functions call X) | no (by design) | **yes** (`graph`/`outline`/`peek`) |
| Lossless on conflict | branch-on-conflict (both heads kept, never a silent overwrite) | last-write-wins |
| Tamper-evident | hash-chained log + `fsck` | - |
| Moment-of-action guard | **yes** (PreToolUse advisory) | - |
| Cross-machine sync | log-shipping (verbatim, hash-identical) | hub sync |
| Needs git | no | no |

## Why THOR comes out ahead (and the mechanism)

- **It has the answer at all (coverage).** THOR chunks your repositories into the
  same index auto-recall searches, so a code question is answered automatically at
  the prompt. mimir keeps code in a separate graph you must call by hand - great
  for "which functions call X", but it never fires at a session boundary.
- **It ranks better on equal footing (score-fusion + class prior + density
  snippets).** THOR fuses lexical bm25 with a dense multilingual embedding,
  routes knowledge-phrased questions toward hand-written facts, and cuts its
  snippets where the query terms cluster. That is the +8 points on the broad
  shared set in Test 2, where coverage is held equal - though on the strictest
  dual-written cut mimir stays ahead (its home turf, now by ~3 points).
- **It compensates for session drift on every channel.** The as-deployed
  auto-injection surfaces the drift-preventing fact more often than mimir at
  its best (65.8% vs 53.4%) and fully catches it more often (45.2% vs 41.1%) -
  and the pins + file-touch/command guards (with exact author-declared anchors)
  cover the windows prompt-association cannot reach, by construction.
- **It is faster and lighter to run.** ~1.5x lower per-prompt latency as a single
  binary, injecting ~2.1x fewer tokens; the default mode holds no resident process.
- **It never loses a write.** Every fact is an event in a hash-chained append-only
  log; a conflicting edit branches (both heads kept and surfaced) instead of
  overwriting, and `fsck` recomputes the chain so tampering is detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted, daemon
  down - each path falls back to bm25 and can never make recall worse.

## Honest weaknesses

- **On the strictest dual-written cut mimir wins (94.3% vs 91.5%).** Pure memory
  recall over a small, clean set of hand-written notes is mimir's home turf,
  consistently across four fresh juries - though the gap narrowed from ~7 to ~3
  points after the v3 round.
- **mimir edges the gotcha (73.9% vs 71.7%) and config-how-to (76.5% vs 73.5%)
  categories, and the curated-docs project (97% vs 90%).** A clean,
  hand-curated doc collection remains a strong retrieval substrate on design
  questions.
- **No code-symbol graph.** For "which functions call X" mimir routes to a symbol
  graph; THOR chunks source directly (which is why it wins the code categories
  here) but has no graph queries.
- **Semantic mode has a cost.** It needs a ~235 MB model file plus a ~570 MB warm
  daemon (client-only, **off by default**; recall degrades cleanly to bm25).
- **Maturity.** THOR is new; mimir is battle-tested in daily use.
- **Measurement caveats.** One machine, LLM judging (blind; Test 3 by a 3-judge
  majority, Tests 1-2 and drift single-judge), and a private corpus - so these
  exact numbers are not independently reproducible from this repo (the drift
  mechanism IS reproducible in-repo via `examples/drift_eval.rs` and its
  committed synthetic corpus). Jury strictness moves absolute numbers between
  runs (the dual-written cut swung 82-92% for THOR across four juries; the
  drift metrics moved several points for BOTH systems between two same-day
  juries); every number in this document comes from one fresh 2026-07-10 run
  after the v3 round, measured after a `thor consolidate` hygiene pass with no
  store writes between hit generation and judging.

## Method

- Harness: `thor/examples/hits_dump.rs` - the real production paths (fused
  `recall_fused_scoped` for deliberate recall, `courier::injection_for_hook_json`
  for as-deployed injection) - for THOR; `mimir recall --json` for mimir.
- Judging: every item blind (systems relabelled A/B(/C) with a seeded random
  mapping per question, ids stripped), scored 0-2 for answer-presence by an
  independent LLM jury; Test 3 by a 3-judge majority (median).
- Latency: `thor courier` vs `mimir recall`, wall-clock, warm daemon, median of
  20, same prompts for both.
- Test 1 = 200 questions (118 shared-knowledge + 82 category-stratified) over
  THOR's store. Test 2 = the 118 whose source fact both stores hold, with the
  53-question strict dual-written cut. Test 3 = 45 questions (15 per project)
  written by an agent reading each repo (ground truth, not THOR's store), both
  systems scoped to the project, top-5 full chunks. Drift = 73 fresh-session
  task prompts built from the store's gotchas and decisions, three channels
  measured per scenario; scenarios without a home project run PROJECTLESS on
  both systems (a neutral working directory). Before measuring: a store-hygiene
  pass (dedup, retract, rescope, fsck) and no store writes between hit
  generation and judging. Numbers are the measured aggregates.
