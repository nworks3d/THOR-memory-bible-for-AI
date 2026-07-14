# THOR vs mimir - the honest picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.

This round (**2026-07-14**) re-measures every test fresh against **mimir
v0.14.0** (the current public release), with production THOR on its own live
store. Both stores had a hygiene pass first (THOR `fsck` green + vectors
synced; mimir `doctor` green + `embed`). Every test is scored by a **3-judge
median** (three distinct judge lenses), blind, with a fresh random side
assignment per question - no number below is carried over from an earlier run.
Worth stating plainly up front: an LLM jury's strictness moves absolute numbers
between runs even with a 3-judge median, so read the *gaps within a round*, not
a single absolute against an older round.

![THOR vs mimir - coverage, quality, drift and speed](assets/benchmark.svg)

## Why two tests, not one

THOR and mimir make a different design choice about **code**, and a single
number would hide it:

- **THOR ingests your repositories** - source, docs and memories - into **one**
  append-only index that auto-recall searches on every prompt.
- **mimir's `recall` serves memories and docs**, and since v0.14 also indexes
  source as `CodeChunk` content that competes for ordinary `recall` slots; its
  distinct strength is a separate **code-symbol graph** (`graph`/`outline`/
  `peek`) you query explicitly.

Neither is wrong - they are different products. So we run **two fair tests**:

1. **As-deployed coverage** - the product as you actually run it. THOR's whole
   thesis is to replace *both* the repo knowledge *and* the memory tool, so its
   ingest is part of the measurement.
2. **Same knowledge** - pure retrieval quality on an **equal corpus**: only
   facts that **both** systems have. This isolates ranking from coverage.

Both are blind-judged: each system returns its top hits, three independent
judges (different lenses) each score the set **0-2** for answer-presence (2 = a
hit clearly contains the answer, 1 = on-topic, 0 = miss), **blind** to which
system produced which (sets relabelled A/B with a seeded deterministic mapping
per question). The reported score is the 3-judge median. The question corpus
references private project internals, so only the aggregate scores are
published.

## Test 1 - As-deployed coverage (200 questions)

What the agent actually gets from a deliberate recall, over the live store,
across six categories.

| category | THOR | mimir v0.14 |
|---|---:|---:|
| code-structure | 63.6% | **74.2%** |
| code-behavior | **78.3%** | 70.0% |
| doc-reference | **81.2%** | 70.0% |
| config how-to | **88.2%** | 79.4% |
| gotcha | 73.9% | 73.9% |
| decision | **79.6%** | 59.3% |
| **overall (n=200)** | **77.0%** | 70.5% |

**THOR leads overall (77.0% vs 70.5%, +6.5).** It wins four of the six
categories - decision by a wide margin (+20.3), doc-reference (+11.2), config
how-to (+8.8), code-behavior (+8.3) - ties gotcha, and loses one:
**code-structure (63.6% vs 74.2%)**, mimir's tree-sitter symbol retrieval, the
one category where its explicit code-symbol graph pays off in ordinary recall.
That single loss is the honest open gap and is called out again under
weaknesses below.

## Test 2 - Same knowledge (cuts over the judged Test 1 set)

The apples-to-apples comparison: only questions whose source fact **both**
stores verifiably hold.

- **strict dual-written cut (n=53)**: questions whose source is a live memory
  head both stores demonstrably carry - zero doubt both have the fact.
- **broad shared cut (n=152)**: strict plus questions whose source file is live
  in both stores (mimir indexes code files too since v0.14).

| cut | THOR | mimir v0.14 |
|---|---:|---:|
| strict dual-written (n=53) | **97.2%** | 94.3% |
| broad shared (n=152) | **88.8%** | 86.2% |

**THOR wins both same-knowledge cuts** - including the strict dual-written cut
(97.2% vs 94.3%), the cleanest equal-corpus comparison there is. Pure memory
recall over hand-written notes was mimir's home turf across several earlier
juries; THOR now leads it, and leads the broader shared cut too (88.8% vs
86.2%).

## Test 3 - Multi-project (three private project repos seeded)

After ingesting three private project repos into THOR - Project 1, Project 2
and Project 3 - each scoped and isolated, we asked whether both systems can
answer real questions about *each* project. 15 questions per project (45 total)
were written by an agent reading the repo itself (ground truth, **not** THOR's
store), each with a gold answer. Both systems were scoped to the project; the
top-5 retrieved chunks were pulled in full and judged **blind by a 3-judge
median**.

| project | THOR | mimir v0.14 |
|---|---:|---:|
| Project 1 | 93.3% | 93.3% |
| Project 2 | 96.7% | **100.0%** |
| Project 3 | **100.0%** | 93.3% |
| **overall (n=45)** | **96.7%** | 95.6% |

**THOR edges the overall (96.7% vs 95.6%).** Per-project numbers on n=15 move
with a single question (6.7 points each), so treat the split as noisy and the
near-tie overall as the takeaway - both systems answer scoped project questions
very well.

## Session drift compensation (73 scenarios, 3-way)

This is what THOR is *for*: at the start of a fresh session (empty context,
just after a compaction), does memory surface the one fact that stops the agent
drifting into a mistake? Each scenario is a realistic task where an agent that
has *forgotten* a gotcha or decision would violate it - the prompt never names
the constraint, so memory must connect the task to it on its own. Measured
three ways, judged **together, three-way blind** (each scenario's three context
blocks shuffled onto anonymous sides): THOR's **courier** (the as-deployed
auto-injection hook, scoped per scenario to its home project), THOR's
**deliberate recall** (the fused path the MCP recall tool serves), and mimir
searching **every** project (`--all`, its best case).

| metric | THOR courier (scoped) | THOR recall (deliberate) | mimir (--all) |
|---|---:|---:|---:|
| preventer surfaced (>=partial) | **86.3%** | 80.8% | 74.0% |
| clear catch (fully surfaced) | **58.9%** | 56.2% | 50.7% |

**THOR leads both drift metrics decisively** - the as-deployed courier surfaces
the preventing fact 86.3% of the time (vs mimir's best case 74.0%) and fully
catches it 58.9% (vs 50.7%). This is the product's core purpose, and the
channel built for it wins. The mechanism is separately reproducible in-repo,
no judge needed: `cargo run --example drift_eval` replays a committed synthetic
corpus through the real courier and guard paths and scores catches *and* false
fires under a one-way noise ratchet.

## Speed and cost

Per-prompt cost, median of 20 over a fixed real-knowledge prompt set, same
machine, each side invoked as it is actually deployed (THOR reads the hook JSON
on stdin; mimir takes the prompt as an argument):

| channel | median | empty |
|---|---:|---:|
| THOR courier (recall + inject) | 230 ms | 3/20 |
| mimir `recall-inject` (as-deployed hook) | 31 ms | 10/20 |
| mimir `recall` (full recall) | 299 ms | 0/20 |

Two things are true at once, and both matter:

- **mimir's as-deployed hook is much faster (~31 ms vs ~230 ms)** - but it
  serves at most a *single* floor-gated memory and returns nothing on half the
  prompts (10/20 empty here). It is fast because it serves less.
- **On a like-for-like full recall, THOR is faster** (`mimir recall` 299 ms vs
  THOR's 230 ms). THOR's courier injects a full real-recall block on every
  prompt it has a hit for; its 3/20 empty is the noise gate staying silent on a
  weak match, by design.

So the honest trade is real: mimir wins raw hook latency by injecting one
floor-gated memory (or nothing half the time); THOR spends more time to inject
a full recall, and doing the same full recall THOR is faster. THOR's warm
inject daemon saves only ~15 ms because the cost is the per-query work - folding
the whole event log and scanning the vector matrix - not process start. A
sandbox prototype that holds those resident (materialized heads + resident
vector matrix) cut recall latency **213 -> 75 ms (65%, byte-identical hits)**;
it is **deliberately not shipped** - a resident cache adds a stale-cache
correctness surface, and THOR's rule is **quality over speed**.

## What each is built for (structural)

| property | THOR | mimir v0.14 |
|---|---|---|
| Unified auto-recall over code + docs + memory | **yes** (chunks source into recall) | recall = memory + docs + `CodeChunk` |
| Code-symbol graph (which functions call X) | derived `where_used`/`impact`/`outline` sidecar | **yes** (`graph`/`outline`/`peek`, first-class) |
| Lossless on conflict | branch-on-conflict (both heads kept, never a silent overwrite) | last-write-wins |
| Tamper-evident | hash-chained log + `fsck` | - |
| Moment-of-action guard | **yes** (PreToolUse advisory + Stop-hook response guard) | context-window guard (transcript size) |
| Cross-machine sync | log-shipping (verbatim, hash-identical) | file/hub sync (last-write-wins) |
| Needs git | no | no |

## Honest weaknesses

- **Code-structure is THOR's one category loss (63.6% vs 74.2%).** mimir's
  first-class tree-sitter symbol graph retrieves structure ("which function is
  where / calls what") better than THOR's chunked source. This is the honest
  open gap and the clearest next quality target.
- **On raw hook latency mimir's as-deployed hook is much faster (~31 ms vs
  ~230 ms)** - but by serving a single floor-gated memory and nothing on half
  the prompts (10/20 empty). On a like-for-like full recall THOR is faster
  (230 vs 299 ms); the trade is real either way.
- **THOR is compute-bound and its latency grows with store size** (the whole
  log is folded per query). A resident cache would fix it (proven 65% in a
  sandbox prototype) but is not shipped, by the quality-over-speed rule above.
- **Maturity.** THOR is new; mimir is battle-tested in daily use and shipping
  at a high cadence (v0.14 added a fast cold-mode, centralized sync, and an
  opt-in GPU build).
- **Measurement caveats.** One machine, LLM judging, a private corpus - so
  these exact numbers are not independently reproducible from this repo (the
  drift mechanism IS reproducible in-repo via `examples/drift_eval.rs` and its
  committed synthetic corpus). An LLM jury's strictness moves absolute numbers
  between runs even with a 3-judge median; read the gaps within a round.

## Method

- Harness: `thor/examples/hits_dump.rs` - the real production paths (fused
  `recall_fused_scoped` for deliberate recall, `courier::injection_for_hook_json`
  for as-deployed injection, invoked per-scenario with `--cwd` for the drift
  courier's project scoping) - for THOR; `mimir recall --full --json` for mimir
  v0.14, both against their own live stores after a hygiene pass.
- Judging: every item blind - systems relabelled onto anonymous sides (A/B for
  the 2-way tests, A/B/C for the 3-way drift) with a fresh cryptographically-
  seeded random assignment per item, recorded in per-test map files the judges
  never see - scored 0 / 0.5 / 1 for answer-presence per side by a **3-judge
  median** (three distinct judge lenses); one run, no re-rolls.
- Test 1 = 200 questions (shared-knowledge + category-stratified) over THOR's
  store. Test 2 = two cuts over the judged Test 1 medians (strict dual-written
  n=53; broad shared n=152). Test 3 = 45 questions (15 per project) written by
  an agent reading each repo (ground truth, not THOR's store), both systems
  scoped to the project, top-5 full chunks. Drift = 73 fresh-session task
  prompts, three channels judged together three-way blind per scenario, courier
  scoped per-scenario to its actual home project.
- Speed: `thor courier` (cold, and with the inject daemon warm) vs `mimir
  recall` (real recall) and `mimir recall-inject` (as-deployed hook), wall-clock
  median of 20 over a fixed real-knowledge prompt set, each channel's timed loop
  alone with a discarded warm-up.
- No writes were made to either store between hit generation and judging
  (verified with `thor fsck`, all checks green). Production THOR measured
  against mimir v0.14.0. Numbers are the measured aggregates.
