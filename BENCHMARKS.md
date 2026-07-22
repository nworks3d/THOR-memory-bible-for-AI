# THOR vs mimir - the honest picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.

This round (**2026-07-22**) re-measures every test fresh: **THOR v0.9.6**
against **mimir v0.15.0** (the current public release), production THOR on its
own live store. The method, the arms and the decision rules were
**pre-registered before any hit was generated**, and the round carries one
documented amendment (below). **It replaces the 2026-07-21 round completely.**
Claims retracted in earlier rounds stay listed as retracted.

Both stores had a hygiene pass first (THOR `fsck` green, vectors synced at
19,797/19,797; mimir's own indexer re-run over the same four repositories the
same day, then `embed` - 1,568 nodes; its frozen archive was backed up first
and restored from that backup after the round). Every test is scored by a
**3-judge median** (three sonnet judge lenses), blind, with a fresh seeded
arm-order permutation per question. One run, no re-rolls. Every comparison
carries an **exact two-sided sign test** on per-question wins; a cell where
the two sides differ on fewer than 6 questions gets no verdict at all ("-" in
the tables below).

The jury changed from the previous round (three sonnet lenses now), and jury
strictness moves absolute scores. **Do not compare absolute numbers across
rounds; the within-round gaps are the findings.** That sentence was written
into the pre-registration before scoring, and it cuts both ways.

![THOR vs mimir - coverage, quality, drift and speed](assets/benchmark.svg?v=20260722)

## What changed since the 2026-07-21 round

Three product changes and one measurement fix stand between the rounds:

- **THOR's deliberate arm now serves structure cards** - the harness prepends
  them exactly as MCP recall does. Not a harness trick: it is what the
  deployed surface serves, and it is the serving-form fix aimed at the
  code-structure loss published last round.
- **Drift gains a session arm**: courier + guard advisories together, the
  as-deployed reality since the action channel landed. The guard advisories
  come from blind-authored expected tool calls (written from the prompts only,
  never from the golds) replayed through the real guard paths.
- **The drift corpus is the cleaned n=59.** 15 retired golds were moved out on
  2026-07-21 - they pointed at deliberately retracted knowledge no channel can
  reach. The previous round ran n=73; never compare drift numbers across
  rounds without this note.
- **The amendment.** The first scoring pass generated mimir's deliberate arms
  (Test 1, Test 3, and drift full recall) with its one-line-summary output,
  while `mimir recall --full` exists and prints full bodies. Judging the
  rival's summary form against THOR's snippet/full form is the same class of
  error as the drift headline discarded before the 2026-07-21 publication -
  this time in mimir's disfavour. The fix was documented in the
  pre-registration amendment BEFORE re-judging: those three arms were
  regenerated with `--full`, repackaged, and re-judged by the same three
  lenses under the same rubric. The first-pass scores are kept on file
  (`round_results_pass1.json`) for the record, not published. THOR's sides and
  their verdicts were not touched. A 7,000-character judging cap bound BOTH
  sides (it hit THOR on 47/59 deliberate and 24/59 session drift items, and
  mimir's full-body form on most items).

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

Both are blind-judged: each system returns its top hits, three independent
judges (distinct lenses) each score the set **0 / 1 / 2** for answer-presence
(no / partly / yes), blind to which system produced which. The reported score
is the 3-judge median, published as a percentage of the maximum. The question
corpus references private project internals, so only aggregates are published.

## Test 1 - As-deployed coverage (200 questions)

The 200 questions are all 152 shared-knowledge questions plus 48
category-stratified questions sampled (seeded RNG) from the remainder of the
re-anchored 460-question battery. The cell sizes therefore differ from the
previous round's table - another reason not to read across rounds.

| category | THOR | mimir v0.15 | per-question | p |
|---|---:|---:|---|---:|
| code-behavior (n=62) | 94.4% | 91.9% | 6 W / 4 L | 0.754 |
| code-structure (n=42) | 72.6% | 72.6% | 8 W / 8 L | 1.000 |
| config how-to (n=16) | 100.0% | 93.8% | 2 W / 0 L | - |
| decision (n=20) | 95.0% | 88.8% | 2 W / 0 L | - |
| doc-reference (n=41) | 91.5% | 80.5% | 9 W / 5 L | 0.424 |
| gotcha (n=19) | 89.5% | 84.2% | 1 W / 0 L | - |
| **overall (n=200)** | **89.2%** | 84.6% | 28 W / 17 L | 0.135 |

**This is a tie with a THOR edge that does not reach significance** (28 wins
to 17, p = 0.135). No single category gap is significant either.

The row that matters is **code-structure**. Last round's one significant mimir
win (57.6% vs 74.2%, p = 0.013) reads **72.6% vs 72.6%, 8 W / 8 L, p = 1.0** -
an exact tie - now that THOR's deliberate arm serves structure cards. That is
the serving-form diagnosis confirmed at the judged level: the gap is closed,
not reversed. Cross-round caveats apply in full (different battery selection,
different jury), so the honest sentence is "the gap is gone in this round's
within-round comparison", not "THOR gained 15 points".

The 2026-07-14 round published 77.0% vs 70.5% and called coverage a lead;
**that claim remains retracted** (the 2026-07-21 re-measure read it as a tie
at 73.2% vs 72.5%).

## Test 2 - Same knowledge (cuts over the judged Test 1 set)

Only questions whose source fact **both** stores verifiably hold.

| cut | THOR | mimir v0.15 | per-question | p |
|---|---:|---:|---|---:|
| strict dual-written (n=53) | 97.2% | 95.8% | 3 W / 2 L | - |
| broad shared (n=151) | 89.1% | 89.2% | 12 W / 13 L | 1.000 |

Both are ties, as in the previous round. The strict cut differs on only 5
questions - below the 6-discordant floor, so no verdict is reported however
the split looks. The 2026-07-14 claim that **THOR wins both same-knowledge
cuts remains retracted**; pure ranking over an equal corpus is not a measured
THOR advantage.

## Test 3 - Multi-project (three private project repos)

15 questions per project (45 total), written by an agent reading each repo
itself (ground truth, **not** either system's store). Both systems scoped to
the project; full bodies pulled and judged blind.

| project | THOR | mimir v0.15 |
|---|---:|---:|
| Project 1 | 93.3% | **100.0%** |
| Project 2 | 86.7% | **90.0%** |
| Project 3 | 93.3% | 93.3% |
| **overall (n=45)** | 91.1% | **94.4%** |

A tie with mimir nominally ahead (2 W / 4 L, p = 0.69, not significant) -
one question moves a 15-question project by 6.7 points, so read the split as
noisy. The 2026-07-14 round published 96.7% vs 95.6% in THOR's favour;
**that claim remains retracted.**

## Session drift compensation (59 scenarios, five channels)

This is what THOR is *for*: at the start of a fresh session, does memory
surface the one fact that stops the agent drifting into a mistake? The prompt
never names the constraint, so memory must connect the task to it on its own.
A channel that stays **silent scores zero** - the agent got nothing and drifts.

The corpus is the **cleaned n=59** (see above); the previous round's n=73
numbers are not comparable to this table.

Five channels, judged together, blind per scenario:

- **THOR session** - courier + guard advisories, the as-deployed channel since
  the action guard landed. The advisories come from blind-authored expected
  tool calls replayed through the real guard paths.
- **THOR courier** - the per-prompt auto-injection hook alone, scoped per
  scenario to its home project.
- **THOR deliberate recall** - fused recall over the drift prompt.
- **mimir `recall --all --full`** - its best channel: full recall over every
  project, full bodies. You must call it explicitly; it is not a hook.
- **mimir `recall-inject`** - its as-deployed hook.

| channel | judged score | >= partial | full catch | missed |
|---|---:|---:|---:|---:|
| THOR session (as-deployed) | **79.7%** | 81.4% | 78.0% | 11 |
| THOR courier | 75.4% | 78.0% | 72.9% | 13 |
| THOR deliberate recall | 70.3% | 72.9% | 67.8% | 16 |
| mimir `recall --all --full` (best case) | 64.4% | 66.1% | 62.7% | 20 |
| mimir `recall-inject` (as-deployed) | 7.6% | 15.3% | 0.0% | 50 |

Sign tests on the pairs that matter:

- **session vs mimir full: 11 W / 0 L, p = 0.001 - a significant THOR win**,
  and it is THOR's as-deployed channel against mimir's best explicit one.
- **courier vs mimir full: 10 W / 2 L, p = 0.039 - significant** even without
  the guard channel.
- deliberate vs mimir full: 9 W / 5 L, p = 0.42 - a tie. (Three drift items
  initially carried two of three lens judgments; the missing lens completed
  them before publication, which moved the courier row from 75.8/12 missed to
  75.4/13 and the deliberate row from 69.9 to 70.3 - no conclusion changed.)
- session vs courier: 3 W / 0 L - fewer than 6 discordant, no verdict.

**This reverses the 2026-07-21 finding**, where mimir's full recall beat
THOR's courier significantly (21 W / 9 L, p = 0.043). What stands between the
two results is the serving-form workstream: one serving stack, structure
cards, fair-share guard advisories, the prose doc-chunk lane, and the guard
channel itself (the session arm did not exist last round). The cross-round
caveats sit right here, next to the claim: the corpus was cleaned (73 -> 59),
the jury changed, and the judging cap changed - so the honest reading is two
rounds that each speak for themselves, not one number that moved by a known
amount. Within THIS round, blind and pre-registered, THOR's as-deployed
channel beats mimir's best channel 11 to 0.

Two more things said plainly:

- The session arm depends on blind-authored expected tool calls; **17 of 59
  scenarios carry none** (the prompt implies no file or command) and there the
  session arm equals courier by construction. Session is reported alongside
  courier, never silently substituted for it.
- mimir's own hook - the thing that runs unasked - misses 50 of 59 scenarios
  and produces zero full catches. The as-deployed comparison is not close, and
  it was not close last round either.

The mechanism is separately reproducible in-repo, no judge needed:
`cargo run --example drift_eval` replays a committed synthetic corpus through
the real courier and guard paths and scores catches *and* false fires under a
one-way noise ratchet.

## Speed and cost

Per-prompt cost, median of 20 over the canonical fixed prompt set, same
machine, one warm-up discarded, each channel timed alone and invoked as it is
actually deployed (THOR reads the hook JSON on stdin; mimir takes the prompt
as an argument):

| channel | median | p90 | empty |
|---|---:|---:|---:|
| THOR courier, inject daemon running | 146.4 ms | 167.2 ms | **0/20** |
| mimir `recall-inject` (as-deployed hook) | **40.4 ms** | 42.5 ms | 6/20 |
| mimir `recall --all` | 323.4 ms | 338.3 ms | 0/20 |

Two things are true at once:

- **mimir's hook is over three times faster** - and returns nothing on 6 of 20
  prompts. It is fast because it often serves nothing.
- **The mimir channel that competes on drift costs 323 ms** and is not a hook.
  THOR's courier answers every prompt in 146 ms.

And one thing that got honestly worse: **THOR's courier was 125 ms at 16.1k
events and is 146 ms at 19.8k.** The compute-bound growth is real (the whole
log is folded per query; the daemon keeps it resident but does not shrink it),
and it is the motivation for the planned materialized-heads work.

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

- **No THOR quality lead is significant.** The coverage edge (89.2 vs 84.6)
  reads p = 0.135; both same-knowledge cuts are ties; multi-project has mimir
  nominally ahead. The significant THOR results this round are the drift sign
  tests - real, but drift is one test, and last round it pointed the other
  way (see the reversal caveat below).
- **Code-structure: the historical loss, kept visible.** The 2026-07-21 round
  measured a significant mimir win (57.6% vs 74.2%, p = 0.013), wider than the
  63.6% vs 74.2% published before it, and the symbol-graph explanation
  published earlier for it did not survive scrutiny: on the older battery
  mimir answered these questions mostly from prose, not code.

  *Addendum (2026-07-21, shipped in v0.9.6):* the diagnosed serving-form half
  of the gap - THOR found the right file more often (81% vs 69% where the gold
  names one) yet answered with raw chunks where mimir answered in words - was
  addressed by structure cards: "who calls X" / "blast radius of X" questions
  get a prose card (definition, signature, callers per file, related memories)
  woven from the one store. Measured mechanically on a 40-question battery
  generated from the symbol sidecar: mean caller-file coverage 58% -> 99%,
  26 better / 0 worse - with the stated limit that the sidecar supplied both
  the gold and the card, so that measured serving form, not sidecar accuracy.

  *Resolution (2026-07-22):* in this round's judged battery, with the
  deliberate arm serving structure cards, the category is an **exact tie**:
  72.6% vs 72.6%, 8 W / 8 L, p = 1.0 (n=42). The loss is closed, not turned
  into a win - and the battery and jury changed across rounds, so this is a
  within-round reading, not a measured 15-point improvement.
- **The drift reversal carries cross-round caveats.** THOR's win over mimir's
  best channel (11 W / 0 L, p = 0.001 as-deployed; 10 W / 2 L, p = 0.039 for
  the courier alone) reverses last round's significant loss (p = 0.043). The
  serving-form workstream is the credible cause, but the corpus was cleaned
  (73 -> 59), the jury changed and the judging cap changed in between, so the
  reversal cannot be decomposed into "the code gained exactly this much".
- **Two discarded scoring passes are on the record, one per side.** Before the
  2026-07-21 publication, a drift run that gave mimir only its weakest channel
  (its hook) was discarded. Before THIS publication, a first pass that judged
  mimir's deliberate arms in their one-line-summary form was discarded and
  re-run with `--full` - the same error class, the other direction. Both are
  recorded because a benchmark that hides its own discarded runs is not
  honest.
- **A 7,000-character judging cap bound both sides.** It cut THOR's served
  text on 47/59 deliberate and 24/59 session drift items, and mimir's
  full-body form on most items. Judges scored what fit under the cap.
- **The session arm is partly constructed.** 17 of 59 drift scenarios carry no
  blind-authored expected call, and there session equals courier by
  construction. That is why courier is always published next to it.
- **Do not compare absolute numbers across rounds.** The jury changed (three
  sonnet lenses this round), the Test 1 battery is a fresh seeded selection,
  and the drift corpus was cleaned. Test 1 overall reads 89.2% here against
  73.2% last round - that is the jury and the battery moving, not the
  products. Read gaps within a round.
- **THOR is compute-bound and its latency grows with store size**: the courier
  read 125 ms at 16.1k events and reads 146 ms at 19.8k. The resident daemon
  removes the cold-start cost but not the growth; the planned
  materialized-heads work targets exactly this.
- **Maturity.** THOR is new; mimir is battle-tested in daily use and shipping
  at a high cadence (v0.15 added inference delegation and an FTS consistency
  check).
- **Measurement caveats.** One machine, LLM judging, a private corpus - these
  exact numbers are not independently reproducible from this repo (the drift
  *mechanism* is, via `examples/drift_eval.rs` and its committed synthetic
  corpus). The 460-derived questions were written from store content and may
  share vocabulary with their golds, which flatters lexical retrieval on both
  sides. n=45 and the per-category cells are small: one question moves a
  15-question project by 6.7 points.

## Method

- Pre-registered: the method, arms, jury, decision rules and the amendment
  were written down before scoring (`PREREGISTERED-2026-07-22.md` in the
  private eval workspace). The published numbers update to the round's results
  regardless of direction - that rule was fixed in advance.
- Harness: `thor/examples/hits_dump.rs` - the real production paths (fused
  `recall_fused_scoped` with structure cards prepended exactly as MCP recall
  serves them for deliberate recall, `courier::injection_for_hook_json`
  invoked per-scenario with `--cwd` for the drift courier's project scoping) -
  for THOR; the drift session arm adds guard advisories replayed through the
  real guard paths (`drift_eval --live --dump-texts`); `mimir recall --all`
  (with `--full` after the amendment) and `mimir recall-inject` for mimir,
  both against their own live stores after a hygiene pass.
- Invocation is matched per tool to how it is deployed. THOR's courier reads
  hook JSON from **stdin**; mimir takes the prompt as a **positional
  argument**. Feeding mimir stdin returns empty output and would manufacture a
  THOR win - a mistake caught once before in this project and guarded against
  since.
- Judging: every item blind - arms relabelled onto anonymous sides with a
  fresh seeded permutation per question (seed = 20260722), recorded in a key
  file joined only after scoring - scored **0 / 1 / 2** for answer-presence by
  a **3-judge median** (three sonnet judge lenses). One run, no re-rolls. A
  7,000-character cap applied to every side's served text.
- Amendment (documented before re-judging): mimir's deliberate arms were first
  generated in one-line-summary form; they were regenerated with `--full`,
  repackaged and re-judged by the same lenses under the same rubric and cap.
  The first pass is kept in `round_results_pass1.json` for the record, not
  published. THOR's sides and verdicts were not touched. The mimir hook arm
  stayed as generated - the hook's compact output IS its as-deployed form.
- Significance: exact two-sided sign test on the discordant per-question
  pairs. Where the two systems differ on fewer than 6 questions, no verdict is
  reported at all ("-") - below that the test cannot reach p <= 0.05 however
  lopsided the split.
- Test 1 = 200 questions (all 152 shared-knowledge + 48 category-stratified,
  seeded, from the re-anchored 460 battery). Test 2 = two cuts over the judged
  Test 1 medians (strict n=53, broad n=151). Test 3 = 45 questions, 15 per
  project, both systems scoped, full bodies. Drift = 59 fresh-session task
  prompts (cleaned corpus), five channels judged together per scenario,
  silence = 0.
- Speed: each channel's timed loop alone, one warm-up discarded, wall-clock
  median + p90 of 20 over the canonical fixed prompt set.
- No writes were made to THOR's store between hit generation and judging
  (`thor fsck` green, vectors synced). mimir's store was re-indexed and
  re-embedded **before** hit generation, not during, and its archive was
  restored from the pre-round backup afterwards.
