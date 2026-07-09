# THOR vs mimir - the honest picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.
Every test was re-measured fresh on 2026-07-09 (evening), on the post-improvement
build, with an independent blind jury - no number below is carried over from an
earlier run.

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
| code-structure | 53.0% | 53.0% |
| code-behavior | **70.8%** | 53.3% |
| doc-reference | **65.0%** | 53.8% |
| config how-to | **82.4%** | 76.5% |
| gotcha | 69.6% | **71.7%** |
| decision | **72.2%** | 61.1% |
| **overall (n=200)** | **67.8%** | **58.5%** |

THOR leads overall and in most categories; mimir edges the gotcha category and
ties code-structure. The earlier headline gap (67% vs 38% on the 504-set) was
mostly coverage over code-only questions - this balanced set is the fairer
picture, and THOR still leads by ~9 points.

## Test 2 - Same knowledge (118 facts both systems have)

The fair, apples-to-apples comparison: only questions whose source fact is a
dual-written memory or a doc chunk **both** stores hold.

**Overall (n=118): THOR 64.8% vs mimir 56.8%** - on the broad shared set THOR
leads by +8 points, thanks to score-fusion plus the query-routed class prior
(knowledge-phrased questions now give hand-written facts a small edge over the
wall of same-topic code chunks). **On the strictest cut - only dual-written
memories, where there is zero doubt both stores have the fact (n=53) - mimir
still wins, 94.3% vs 90.6%, but the gap closed from ~7.5 points to ~3.7.** Pure
memory recall over a small, clean set of hand-written notes remains mimir's home
turf; the ranking round moved THOR most of the way there.

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
| Project 1 | 87% | **97%** |
| Project 2 | **100%** | 0% |
| Project 3 | **97%** | 80% |
| **overall (n=45)** | **94.4%** | **58.9%** |

**THOR wins overall (94% vs 59%) and dominates where it uniquely holds the code** -
Project 2 has no mimir doc collection at all (its store holds only tangential
notes about the project), and on Project 3 THOR's full source beats mimir's
docs-only view. **mimir still wins Project 1, 97% vs 87%**: those questions lean
on hand-curated architecture/bring-up docs that mimir indexes as a clean
collection. The gap there closed from 26 points to 10 after the ranking round
(the class prior stops code chunks from crowding out the docs). This remains the
honest complementary strength: a curated doc collection can out-retrieve raw
source ingest on design questions.

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
| preventer surfaced (>=partial) | 50.7% | **60.3%** | 53.4% |
| clear catch (fully surfaced) | 19.2% | 39.7% | **43.8%** |

The honest reading: on prompt-only association THOR's deliberate recall surfaces
the preventer most often, but **mimir fully catches it more often than either
THOR channel** on this fresh, stricter jury - and THOR's as-deployed courier
pays a real price for its noise gates (the same gates that keep everyday
injection quiet filter out weakly-associated preventers). Prompt-association is
no longer THOR's primary drift mechanism, and this table deliberately does not
credit the channels built for exactly this window: **pinned rules** re-inject in
full after every compaction by construction, and the **file-touch guard**
surfaces file-naming constraints at the moment of action (8/8 on the committed
drift corpus - see `cargo run --example drift_eval`, the in-repo reproducible
measurement). Those channels bypass ranking instead of improving it, which is
why they are reported separately instead of blended into this table.

## Speed and cost

Full per-prompt cost (process start + recall), warm daemon, median of 20 runs,
same (long, realistic task-prompt) query set for both, same machine:

| | THOR | mimir |
|---|---:|---:|
| latency (warm, median) | **268 ms** | 505 ms |
| tokens injected / prompt (same set) | **~351** | ~845 |
| resident RAM | ~570 MB (semantic daemon) / **0** (bm25 default) | ~700 MB observed |

THOR is **~1.9x faster** per prompt on this set - a single native binary with no
wrapper process - while injecting **~2.4x fewer tokens** (three tight snippets
against mimir's longer bodies). Both are slower here than on the earlier
short-query set (83 vs 254 ms then): the drift task prompts are long, which
costs both systems; the ratio is what carries. THOR's default bm25 mode needs no
resident process at all; the optional semantic mode keeps a warm ~570 MB
embedder resident.

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
- **It ranks better on equal footing (score-fusion + class prior).** THOR fuses
  lexical bm25 with a dense multilingual embedding, and routes knowledge-phrased
  questions toward hand-written facts. That is the +8 points on the broad shared
  set in Test 2, where coverage is held equal - though on the strictest
  dual-written cut mimir still edges ahead (its home turf, gap now ~3.7 points).
- **It compensates for session drift by construction, not only by ranking.**
  Pinned rules come back in full after every compaction, and the file-touch
  guard fires at the moment of action (8/8 on the committed corpus). On pure
  prompt-association the fresh jury scores mimir's full-catch higher - see the
  drift table for the honest split.
- **It is faster and lighter to run.** ~1.9x lower per-prompt latency as a single
  binary, injecting ~2.4x fewer tokens; the default mode holds no resident process.
- **It never loses a write.** Every fact is an event in a hash-chained append-only
  log; a conflicting edit branches (both heads kept and surfaced) instead of
  overwriting, and `fsck` recomputes the chain so tampering is detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted, daemon
  down - each path falls back to bm25 and can never make recall worse.

## Honest weaknesses

- **On the strictest dual-written cut mimir wins (94.3% vs 90.6%).** Pure memory
  recall over a small, clean set of hand-written notes is mimir's home turf; the
  ranking round narrowed the gap (from ~7.5 to ~3.7 points) but did not close it.
- **On prompt-only drift association mimir's full-catch is higher (43.8% vs
  39.7% deliberate / 19.2% as-deployed).** THOR's noise gates cost catch on the
  auto-injection channel; its drift answer is structural (pins + file-touch
  guard), which this test deliberately does not blend in.
- **mimir wins the curated-docs project (97% vs 87%).** A clean, hand-curated
  doc collection still out-retrieves raw source ingest on design questions.
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
  runs; every number in this document comes from one fresh 2026-07-09 run.

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
  measured per scenario. Numbers are the measured aggregates.
