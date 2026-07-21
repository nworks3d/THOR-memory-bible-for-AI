# THOR vs mimir - the honest picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.

This round (**2026-07-21**) re-measures every test fresh against **mimir
v0.15.0** (the current public release), with production THOR on its own live
store. **It replaces the 2026-07-14 round completely, and it retracts four
claims that round made.** THOR no longer leads any quality metric. What follows
is what the measurement actually says.

Both stores had a hygiene pass first (THOR `fsck` green + vectors synced at
19,170/19,170; mimir `doctor` green, its own indexer re-run over the same
repositories, then `embed`). Every test is scored by a **3-judge median** (three
distinct judge lenses), blind, with a fresh seeded side assignment per question.
One run, no re-rolls. New this round: every comparison carries an **exact sign
test** on per-question wins, so "ahead" and "measurably ahead" stop being the
same sentence.

![THOR vs mimir - coverage, quality, drift and speed](assets/benchmark.svg)

## What changed, and why you should trust this round more than the last

The previous round compared a live THOR against a mimir that had been frozen for
a week, on a corpus written from THOR's own store. Two fixes went in before
anything was measured:

- **mimir re-indexed the same repositories with its own chunker** (its code
  collections grew from 217 to 222 files and 2,456 to 2,516 chunks; the docs and
  the THOR source collections likewise), then re-embedded. The code asymmetry was
  12,246 of the 15,883 items separating the two stores, and it was the single
  biggest unfairness in the comparison. Closing it helped mimir.
- **mimir is given its best channel, not its weakest.** The drift test below
  reports mimir twice: its as-deployed hook, and its full recall over every
  project. An earlier version of this round compared THOR only against the hook
  and produced a flattering result that has been discarded.

What was *not* done, deliberately: THOR's 123 extra hand-written facts were not
copied into mimir. No question in any corpus asks about them, so adding them
would have given mimir no answering ability and only more to search through.

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

1. **As-deployed coverage** - the product as you actually run it.
2. **Same knowledge** - pure retrieval quality on an **equal corpus**: only
   facts that **both** systems have. This isolates ranking from coverage.

Both are blind-judged: each system returns its top hits, three independent judges
(different lenses) each score the set **0 / 0.5 / 1** for answer-presence, blind
to which system produced which. The reported score is the 3-judge median. The
question corpus references private project internals, so only aggregates are
published.

## Test 1 - As-deployed coverage (200 questions)

| category | THOR | mimir v0.15 | per-question | p |
|---|---:|---:|---|---:|
| doc-reference (n=40) | **82.5%** | 68.8% | 13 W / 6 L | 0.167 |
| decision (n=27) | **75.9%** | 68.5% | 5 W / 2 L | 0.453 |
| config how-to (n=17) | 82.4% | 82.4% | 2 W / 2 L | - |
| code-behavior (n=60) | 70.8% | 70.8% | 9 W / 9 L | 1.000 |
| gotcha (n=23) | 76.1% | 78.3% | 2 W / 3 L | - |
| code-structure (n=33) | 57.6% | **74.2%** | 2 W / 12 L | **0.013** |
| **overall (n=200)** | 73.2% | 72.5% | 33 W / 34 L | 1.000 |

**This is a tie.** THOR is 0.8 points ahead and *behind* on per-question wins;
133 of the 200 questions score identically. The previous round published
77.0% vs 70.5% and called it a lead - **that claim is retracted.**

One difference in this table survives a significance test, and it is not THOR's:
**code-structure**, where mimir wins 12 of the 14 questions the two disagree on.
That is the known open gap, and it is wider than the 63.6% vs 74.2% published
before. doc-reference is the largest THOR-favouring gap (+13.8) but does not
reach significance on n=40.

### Controlling for the frozen-vs-live asymmetry

mimir's copy of each answer is frozen; THOR's has kept being revised. Three cuts,
to see whether that flatters THOR:

| cut | THOR | mimir v0.15 | p |
|---|---:|---:|---:|
| gold text identical in both stores (n=27) | 92.6% | 77.8% | 0.344 |
| gold still live in THOR (n=156) | 81.4% | 81.7% | 0.678 |
| gold retracted in THOR since (n=44) | 44.3% | 39.8% | 0.607 |

The result is the opposite of the worry: THOR does *best* where the text is
byte-identical in both stores, and relatively worse on the 129 answers it has
revised since - its memory has moved on from the wording the questions were
written from, while mimir's is frozen at exactly that wording. If anything the
corpus tilts slightly toward mimir. None of the cuts is significant.

## Test 2 - Same knowledge (cuts over the judged Test 1 set)

Only questions whose source fact **both** stores verifiably hold.

| cut | THOR | mimir v0.15 | per-question | p |
|---|---:|---:|---|---:|
| strict dual-written (n=53) | 94.3% | **96.2%** | 3 W / 5 L | 0.727 |
| broad shared (n=152) | 81.2% | **83.9%** | 21 W / 28 L | 0.392 |

Both are ties; mimir is nominally ahead on both. The previous round published
97.2% vs 94.3% and 88.8% vs 86.2% and stated that **THOR wins both
same-knowledge cuts** - **that claim is retracted.** Pure ranking over an equal
corpus is not a THOR advantage.

## Test 3 - Multi-project (three private project repos)

15 questions per project (45 total), written by an agent reading each repo
itself (ground truth, **not** either system's store). Both systems scoped to the
project; top-5 chunks pulled in full and judged blind.

| project | THOR | mimir v0.15 |
|---|---:|---:|
| Project 1 | 93.3% | 93.3% |
| Project 2 | 86.7% | **93.3%** |
| Project 3 | 83.3% | **96.7%** |
| **overall (n=45)** | 87.8% | **94.4%** |

mimir is ahead by 6.7 points (2 W / 8 L, p = 0.109) - not significant on n=45,
where one question moves a project by 6.7 points. The previous round published
96.7% vs 95.6% in THOR's favour; **that claim is retracted too.**

## Session drift compensation (73 scenarios, 4-way)

This is what THOR is *for*: at the start of a fresh session, does memory surface
the one fact that stops the agent drifting into a mistake? The prompt never names
the constraint, so memory must connect the task to it on its own. A channel that
stays **silent scores zero** - the agent got nothing and drifts.

Four channels, judged **together, four-way blind** per scenario: THOR's
**courier** (the as-deployed auto-injection hook, scoped per scenario to its home
project), THOR's **deliberate recall**, mimir's **as-deployed hook**
(`recall-inject`), and mimir's **full recall over every project** (`recall
--all`, its best case).

| channel | preventer surfaced (>=partial) | clear catch (full) | missed outright |
|---|---:|---:|---:|
| mimir `recall --all` (best case) | **74.0%** | **49.3%** | 19 |
| THOR deliberate recall | 72.6% | 42.5% | 20 |
| THOR courier (as-deployed) | 67.1% | 37.0% | 24 |
| mimir `recall-inject` (as-deployed) | 37.0% | 5.5% | 46 |

Read this table twice, because the two comparisons in it point opposite ways:

- **Capability: mimir wins.** Its full recall beats THOR's courier on
  per-scenario comparison (21 W / 9 L, p = 0.043) and is level with THOR's
  deliberate channel (16 W / 10 L, p = 0.327). On the thing THOR was built for,
  mimir searching everything is at least as good.
- **As deployed: THOR wins, and not narrowly.** Against mimir's actual hook -
  the thing that runs by itself on every prompt - THOR's courier wins 37 to 6
  and its deliberate channel 41 to 4, both p < 0.0001. mimir's hook misses 46 of
  73 scenarios and returns nothing at all on 2.

The distinction that matters for a user: **mimir's winning channel is not a
hook.** You have to decide to call it. THOR's winning channel runs unasked on
every prompt. Whether that is worth the gap is a judgement about how you work,
not a number this benchmark can settle.

The mechanism is separately reproducible in-repo, no judge needed:
`cargo run --example drift_eval` replays a committed synthetic corpus through the
real courier and guard paths and scores catches *and* false fires under a one-way
noise ratchet.

## Speed and cost

Per-prompt cost, median of 20 over a fixed real-knowledge prompt set, same
machine, one warm-up discarded, each channel timed alone and invoked as it is
actually deployed (THOR reads the hook JSON on stdin; mimir takes the prompt as
an argument):

| channel | median | p90 | empty |
|---|---:|---:|---:|
| THOR courier, inject daemon running | 125 ms | 135 ms | **0/20** |
| mimir `recall-inject` (as-deployed hook) | **41 ms** | 44 ms | 6/20 |
| mimir `recall --all` (the channel that wins drift) | 334 ms | 346 ms | 0/20 |

Two things are true at once:

- **mimir's hook is three times faster** - and returns nothing on 6 of 20
  prompts. It is fast because it often serves nothing.
- **The mimir channel that actually wins the drift test costs 334 ms** and is
  not a hook. THOR's courier answers every prompt in 125 ms.

THOR is compute-bound and its cold latency grows with store size; the resident
cache removes that growth only while a daemon is up. See the weaknesses below.

## What each is built for (structural)

| property | THOR | mimir v0.15 |
|---|---|---|
| Unified auto-recall over code + docs + memory | **yes** (chunks source into recall) | recall = memory + docs + `CodeChunk` |
| Code-symbol graph (which functions call X) | derived `where_used`/`impact`/`outline` sidecar | **yes** (`graph`/`outline`/`peek`, first-class) |
| Lossless on conflict | branch-on-conflict (both heads kept, never a silent overwrite) | last-write-wins |
| Tamper-evident | hash-chained log + `fsck` (exits non-zero on damage) | - |
| Search-index integrity check | `fsck` verifies FTS structure | `doctor` verifies FTS/node consistency |
| Moment-of-action guard | **yes** (PreToolUse advisory + Stop-hook response guard) | context-window guard (transcript size) |
| Cross-machine sync | log-shipping (verbatim, hash-identical) | file/hub sync (last-write-wins) |
| Needs git | no | no |

## Honest weaknesses

- **THOR does not lead any quality metric in this round.** Coverage, same
  knowledge, multi-project and drift-capability are all ties or mimir wins. The
  single defensible THOR advantage here is operational: its automatic channel
  answers every prompt, in 125 ms, and never stays silent.
- **Code-structure is a significant loss (57.6% vs 74.2%, p = 0.013)** and it
  has widened since the previous round. The explanation published earlier - that
  mimir wins it from its symbol graph - did not survive scrutiny then and has not
  been re-tested now: on the older battery mimir answered these questions mostly
  from prose, not code. So the gap is real, significant, and its cause is still
  not established.
- **mimir's full recall beats THOR's courier on drift** (p = 0.043), the test
  THOR exists to win. THOR's answer is that its channel is a hook and mimir's is
  not; that is a real distinction, but it is not a retrieval-quality win.
- **An earlier version of this round measured drift against mimir's hook only**
  and reported THOR winning by a factor of three. That comparison gave mimir its
  weakest channel and the result was discarded before publication. It is recorded
  here because a benchmark that hides its own discarded runs is not honest.
- **Do not compare absolute numbers across rounds.** An LLM jury's strictness
  moves them even with a 3-judge median: THOR's courier reads 67.1% here against
  86.3% in the last round, while an A/B against the previous release proved the
  courier's drift output is byte-identical. The code did not change; the jury
  did. Read gaps within a round.
- **THOR is compute-bound and its latency grows with store size** (the whole log
  is folded per query). The resident cache removes that growth while a daemon is
  up; a bare per-prompt hook has nothing to keep, so the growth is deferred, not
  solved.
- **Maturity.** THOR is new; mimir is battle-tested in daily use and shipping at
  a high cadence (v0.15 added inference delegation and an FTS consistency check).
- **Measurement caveats.** One machine, LLM judging, a private corpus - these
  exact numbers are not independently reproducible from this repo (the drift
  *mechanism* is, via `examples/drift_eval.rs` and its committed synthetic
  corpus). n=45 and the per-category cells are small: one question moves a
  15-question project by 6.7 points.

## Method

- Harness: `thor/examples/hits_dump.rs` - the real production paths (fused
  `recall_fused_scoped` for deliberate recall, `courier::injection_for_hook_json`
  for as-deployed injection, invoked per-scenario with `--cwd` for the drift
  courier's project scoping) - for THOR; `mimir recall` and `mimir recall-inject`
  for mimir, both against their own live stores after a hygiene pass.
- Invocation is matched per tool to how it is deployed. THOR's courier reads hook
  JSON from **stdin**; mimir takes the prompt as a **positional argument**.
  Feeding mimir stdin returns empty output and would manufacture a THOR win - a
  mistake caught once before in this project, and guarded against here by
  checking that both sides return non-empty on every question (200/200 and
  200/200 on Test 1).
- Judging: every item blind - systems relabelled onto anonymous sides (A/B for
  the two-way tests, A/B/C/D for the four-way drift) with a fresh seeded
  assignment per item, recorded in map files the judges never see - scored
  0 / 0.5 / 1 for answer-presence by a **3-judge median** (three distinct
  lenses: literal, practical, adversarial). One run, no re-rolls.
- Significance: exact two-sided sign test on per-question wins. Where the two
  systems differ on fewer than 6 questions, no verdict is reported at all - below
  that the test cannot reach p <= 0.05 however lopsided the split.
- Test 1 = 200 questions (shared-knowledge + category-stratified). Test 2 = two
  cuts over the judged Test 1 medians. Test 3 = 45 questions, 15 per project,
  both systems scoped, top-5 full chunks. Drift = 73 fresh-session task prompts,
  four channels judged together per scenario, THOR's courier scoped per scenario
  to its home project.
- Speed: each channel's timed loop alone, one warm-up discarded, wall-clock
  median of 20 over a fixed real-knowledge prompt set.
- No writes were made to THOR's store between hit generation and judging
  (verified with `thor fsck`, all checks green). mimir's store was re-indexed and
  re-embedded **before** hit generation, not during, and restored to its
  pre-benchmark state afterwards.
