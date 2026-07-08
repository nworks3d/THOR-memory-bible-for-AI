# THOR vs mimir - the honest two-test picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.

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

## Test 1 - As-deployed coverage (500 questions)

What the agent actually gets injected, automatically, per prompt.

| category | THOR | mimir |
|---|---:|---:|
| code-structure | **79.7%** | 12.7% |
| code-behavior | **89.0%** | 21.2% |
| doc-reference | **84.8%** | 29.1% |
| config how-to | **91.2%** | 44.1% |
| gotcha | **88.9%** | 47.2% |
| decision | **90.8%** | 61.8% |
| **overall** | **86.1%** | **27.5%** |

THOR wins every category. mimir's low code scores are **not a ranking failure** -
they are coverage: on the business-code questions (386 of the 500) mimir's recall
scores 13.0% because it does not index source, while THOR scores 84.7%. That is
exactly THOR's "one local index over everything" design showing up as a result.

## Test 2 - Same knowledge (118 facts both systems have)

The fair, apples-to-apples comparison: only questions whose source fact is a
dual-written memory or a doc chunk **both** stores hold.

| category | THOR | mimir |
|---|---:|---:|
| code-structure | **80%** | 70% |
| code-behavior | **92%** | 59% |
| doc-reference | **88%** | 85% |
| config how-to | **96%** | 71% |
| gotcha | **92%** | 82% |
| decision | **92%** | 85% |
| **overall (n=118)** | **90.7%** | **75.0%** |

**Even on an equal corpus, THOR leads by +15.7 points.** On the strictest cut -
only dual-written memories, where there is zero doubt both stores have the fact
(n=53) - it is **94.3% vs 80.2%**. This is the semantic score-fusion layer
catching paraphrases that lexical-only search misses. It is consistent with an
independent, hand-curated 52-question set built to be answerable by both systems:
**85.6% vs 74.0%**.

## Session drift compensation (74 scenarios)

This is what THOR is *for*: at the start of a fresh session (empty context, just
after a compaction), does the automatic injection surface the one fact that stops
the agent drifting into a mistake? Each scenario is a realistic task where an agent
that has *forgotten* a gotcha or decision would violate it - and the prompt never
names the constraint, so the memory must connect the task to it on its own.

| metric | THOR | mimir |
|---|---:|---:|
| preventer surfaced (0-2 avg) | **74.7%** | 45.9% |
| clear catch (fully surfaced) | **54.8%** | 30.1% |
| on gotchas | **76.4%** | 48.6% |
| on decisions | **73.0%** | 43.2% |

At the drift moment THOR surfaces the drift-preventing fact **1.6x more often** and
fully catches it **1.8x more often**. Both systems inject the same budget (top-3),
so this is not a "more context" effect - it is better association between a task
and the constraint that governs it.

## Speed and cost

Full per-prompt cost (process start + recall), warm daemon, median of many runs,
same query set, same machine:

| | THOR | mimir |
|---|---:|---:|
| latency (warm) | **83 ms** (81-87) | 254 ms (252-266) |
| tokens injected / prompt | ~239 | ~212 |
| resident RAM | ~570 MB (semantic daemon) / **0** (bm25 default) | ~700 MB observed |

THOR is **~3.1x faster** per prompt - a single native binary with no wrapper
process - while injecting a **comparable** number of tokens. Its default bm25 mode
needs no resident process at all; the optional semantic mode keeps a warm ~570 MB
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
- **It ranks better on equal footing (score-fusion).** THOR fuses lexical bm25 with
  a dense multilingual embedding (`fused = bm_norm + LAMBDA*cos`), so a paraphrased
  question still finds the right fact. That is the +15.7 points in Test 2, where
  coverage is held equal.
- **It compensates for session drift.** The whole reason the tool exists: after a
  compaction the agent starts blank, and THOR's automatic injection puts the
  governing gotcha/decision back in front of it 1.6x more often than mimir.
- **It is faster and lighter to run.** ~3.1x lower per-prompt latency as a single
  binary; the default mode holds no resident process.
- **It never loses a write.** Every fact is an event in a hash-chained append-only
  log; a conflicting edit branches (both heads kept and surfaced) instead of
  overwriting, and `fsck` recomputes the chain so tampering is detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted, daemon
  down - each path falls back to bm25 and can never make recall worse.

## Honest weaknesses

- **Test 1's headline gap is coverage, not pure ranking.** THOR ingests source and
  mimir's recall does not; strip that and the honest same-knowledge lead is the
  ~16 points of Test 2, not 59.
- **No code-symbol graph.** For "which functions call X" mimir routes to a symbol
  graph; THOR chunks source directly (which is why it wins the code categories
  here) but has no graph queries.
- **Semantic mode has a cost.** It needs a ~235 MB model file plus a ~570 MB warm
  daemon (client-only, **off by default**; recall degrades cleanly to bm25).
- **Maturity.** THOR is new; mimir is battle-tested in daily use.
- **Measurement caveats.** One machine, a single judge per question, and a private
  corpus - so these exact numbers are not independently reproducible from this
  repo. The auto-generated 500-set has noisier per-question ground truth than the
  hand-curated 52-set (reported alongside it for exactly that reason).

## Method

- Harness: `thor/examples/recall_eval.rs` (the real `recall_fused` path) for THOR;
  `mimir recall --json` for mimir; a blind A/B judge pass over both.
- Latency: `thor courier` vs `mimir recall`, wall-clock, warm daemon, median.
- Test 1 = 500 auto-generated questions over THOR's store. Test 2 = the subset
  whose source fact both stores hold. Drift = 74 fresh-session task prompts built
  from the store's gotchas and decisions. Numbers are the measured aggregates.
