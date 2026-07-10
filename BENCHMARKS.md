# THOR vs mimir - the honest picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.
Every test was re-measured fresh on 2026-07-11, after the v4 serving/matching
round (identifier-aware trigger matching, footer-stripped serving, full-body
memories on the deliberate path, neighbor stitching, courier full-body typed
facts in an 8000-char budget, silence scenarios, a confusion table and a
one-way noise ratchet; the identifier matching and the confusion-table
discipline are idea adoptions from mimir's own rounds, credited in the README)
- against mimir's strongest opponent build to date:
unreleased upstream main commit `f98c7fd` (post-v0.13.0: an in-place
MatrixCache cache fix, a warm `/inject` daemon, a fast cold-path mode,
code-content indexing as `CodeChunk`, an identifier RRF leg, recency/type
priors), built from source, with code content indexed and docs re-indexed and
re-embedded over the same two main projects. Every test this round is scored
by a **3-judge median** (three distinct judge lenses), blind, with a seeded
relabel - no number below is carried over from an earlier run. No writes were
made to either store between hit generation and judging (verified with `thor
fsck` before and after).

![THOR vs mimir - coverage, quality, drift and speed](assets/benchmark.svg)

## Why two tests, not one

THOR and mimir make a different design choice about **code**, and a single number
would hide it:

- **THOR ingests your repositories** - source, docs and memories - into **one**
  append-only index that auto-recall searches on every prompt. For one real
  project THOR holds **2543 facts, of which 1532 are source-code chunks**.
- **mimir's `recall` serves memories and docs**; source code lived in a **separate
  code-symbol graph** (`graph` / `outline` / `peek`) you query explicitly - not
  auto-injected at a prompt. That was true as of the v3 round (2 marker nodes,
  0 source chunks for that same project); **this round mimir added
  code-content indexing** (`CodeChunk`, tree-sitter based) that now competes
  for ordinary `recall` slots too - see Test 1 below, where it flips the
  code-structure category.

Neither is wrong - they are different products. So we run **two separate, fair
tests**:

1. **As-deployed coverage** - the product as you actually run it. THOR's whole
   thesis is to replace *both* the repo knowledge *and* the memory tool, so its
   ingest is part of the measurement.
2. **Same knowledge** - pure retrieval quality on an **equal corpus**: only facts
   that **both** systems have. This isolates the ranking algorithm from coverage.

Both are blind-judged: each system returns its top hits, three independent
judges (different lenses) each score the set **0-2** for answer-presence (2 = a
hit clearly contains the answer, 1 = on-topic, 0 = miss), **blind** to which
system produced which (sets relabelled A/B with a seeded deterministic mapping
per question, scored on content alone) - the reported score is the 3-judge
median. The question corpus references private project internals, so only the
aggregate scores are published.

## Test 1 - As-deployed coverage (200 questions)

What the agent actually gets from a deliberate recall. The set is 118
shared-knowledge questions (facts both stores hold) plus 82 category-stratified
coverage questions - deliberately balanced toward mimir's home turf, unlike the
earlier 504-question set that was dominated by code-only questions.

| category | THOR | mimir |
|---|---:|---:|
| code-structure | 53.8% | **57.6%** |
| code-behavior | **70.8%** | 55.8% |
| doc-reference | **77.5%** | 60.0% |
| config how-to | **85.3%** | 70.6% |
| gotcha | **76.1%** | 71.7% |
| decision | **72.2%** | 61.1% |
| **overall (n=200)** | **71.4%** | 60.8% |

mimir now wins code-structure (57.6% vs 53.8%) - its new tree-sitter
code-chunk indexing (`CodeChunk`) folded into ordinary `recall` catches up on
symbol-shaped questions for the first time. THOR still leads every other
category and overall, by 10.6 points (71.4% vs 60.8%), a similar margin to the
prior round, though the category picture has shifted: config how-to is the
only category to clear an 80% bar this round (THOR 85.3%).

## Test 2 - Same knowledge (118 facts both systems have)

The fair, apples-to-apples comparison: only questions whose source fact is a
dual-written memory or a doc chunk **both** stores hold.

**Not re-measured this round.** No fresh Test 2 data was produced in the v4
round (this round's `final_summary.json` covers Test 1, Test 3 and drift
only); rather than invent a number or silently carry one forward as if it were
fresh, this section keeps the last measured result for reference:

**Previous result (v3 round) - overall (n=118): THOR 61.4% vs mimir 53.8%** -
on the broad shared set THOR led by +8 points, thanks to score-fusion plus the
query-routed class prior (knowledge-phrased questions give hand-written facts
a small edge over the wall of same-topic code chunks). **On the strictest cut
- only dual-written memories, where there is zero doubt both stores have the
fact (n=53) - mimir won, 94.3% vs 91.5%** - the gap had narrowed from ~7
points to ~3 after the v3 round (author-declared trigger tags plus heading
crumbs). Pure memory recall over a small, clean set of hand-written notes was
mimir's home turf, consistently across four fresh juries measured to date
(94.3-89.6 vs 91.5-82.1); THOR's breadth was the counterweight, not a
substitute. Treat all of the above as previous-round numbers, not part of this
round's fresh re-measurement.

## Test 3 - Multi-project (three private project repos seeded)

After ingesting three private project repos into THOR - Project 1, Project 2, and
Project 3 - each scoped and isolated, we asked whether both systems can answer real
questions about *each* project. 15 questions per project (45 total) were written by
an agent reading the repo itself (ground truth, **not** THOR's store), each with a
gold answer. Both systems were scoped to the project (THOR `--project <key>`, mimir by
the project's working dir); the top-5 retrieved chunks were pulled in full and judged
**blind by a 3-judge median**.

| project | THOR | mimir |
|---|---:|---:|
| Project 1 | 90.0% | **100.0%** |
| Project 2 | **100.0%** | 96.7% |
| Project 3 | 93.3% | 93.3% (tie) |
| **overall (n=45)** | 94.4% | **96.7%** |

**mimir retakes the overall lead this round, 96.7% vs 94.4%.** The swing is
almost entirely Project 3, where full source ingest used to be THOR's biggest
structural edge (a 16.6-point THOR lead last round, 93% vs 77%): mimir's new
code-content indexing (the same `CodeChunk` upgrade behind its Test 1
code-structure win) closes it completely - a dead-even 93.3% vs 93.3% this
round. mimir also widens its lead on Project 1's hand-curated docs (100.0% vs
90.0%, up from 97% vs 90%): hand-curated architecture/bring-up docs remain a
strong retrieval substrate on design questions. THOR keeps Project 2 (100.0%
vs 96.7%), a similar margin to before.

## Session drift compensation (73 scenarios, 3-way)

This is what THOR is *for*: at the start of a fresh session (empty context, just
after a compaction), does memory surface the one fact that stops the agent
drifting into a mistake? Each scenario is a realistic task where an agent that
has *forgotten* a gotcha or decision would violate it - the prompt never names
the constraint, so memory must connect the task to it on its own. Measured three
ways, with precise channel definitions: THOR's **courier** (the real as-deployed
auto-injection hook, this round scoped **per scenario** to that scenario's
actual home project, including its noise gates), THOR's **deliberate recall**
(the fused path over every project - what the MCP recall tool serves), and
mimir searching **every** project (`--all`, its best case).

| metric | THOR courier (scoped, as-deployed) | THOR recall (deliberate, fused) | mimir (--all) |
|---|---:|---:|---:|
| preventer surfaced (>=partial) | **74.0%** | 72.6% | 56.2% |
| clear catch (fully surfaced) | **47.9%** | **47.9%** | 42.5% |

**Correction, this round.** The courier channel was first generated with a
single *fixed* working directory (`The-AI-memory-bible`) for all 73 scenarios.
That is an instrument bug, not a courier limitation: the project-scoped
courier correctly excludes another project's facts when run from a different
project's directory, but 37 of the 73 scenarios have a different true home
project (mostly Project 3) and were measured from the wrong one. Under
that bug the courier scored only 35.6% surfaced. Regenerating per-scenario
with each scenario's actual home project fixed it, producing the 74.0% /
47.9% in the table above. **The same fixed-cwd bug was present, unnoticed, in
the courier numbers previously published in this document** (65.8% surfaced /
45.2% clear catch) - those were measured the same wrong way and understate the
as-deployed courier; read them as historical, not as its real ceiling.

On the corrected numbers, the as-deployed courier leads or ties its own
deliberate recall (74.0% vs 72.6% surfaced; 47.9% vs 47.9% clear catch, a tie)
and leads mimir's best case on both metrics (74.0% vs 56.2% surfaced; 47.9%
vs 42.5% clear catch). None of the three channels clears an 80% bar on either
metric this round. The channels built for the post-compaction window on top
of this - **pinned rules** (re-injected in full by construction) and the
**file-touch + command guards** (fired at the moment of action, with exact
author-declared `anchors`; 16/16 surfaced on the committed corpus) - are
reported via the in-repo reproducible measurement:
`cargo run --example drift_eval`. Those in-repo numbers are a separate,
committed synthetic corpus, were not part of this round's re-measurement, and
are unchanged.

## Speed and cost

Full per-prompt cost (process start + recall), median of 20 runs, same 20
(long, realistic task-prompt) prompts fed to every channel, same machine:

| | THOR courier | mimir cold (as-deployed default) | mimir warm (opt-in daemon) |
|---|---:|---:|---:|
| latency (median) | 234 ms | 564 ms | **66 ms** |
| tokens injected / prompt | 579 | **236** | not measured here |

THOR is **~2.4x faster** than mimir's as-deployed default path (the
`hook_recall.ps1` PowerShell hook - PowerShell's own process-start cost is
included, since it is a genuine part of that as-deployed latency) - but
mimir's **opt-in warm daemon** (a background process serving `/inject`, single
best-effort memory only, floor-gated, **not** the same `recall -n 8` + dedup
algorithm as the cold path) is itself **~3.5x faster than THOR**. On tokens,
THOR injects **~2.5x more** tokens than mimir's cold path on this sample - the
reverse of the headline this document used to carry.

Investigated, not papered over: this round's smaller 20-prompt sample leans
heavily on Project 3 scenarios, and several of those prompts surface
long `[global]`-tier THOR facts (in scope regardless of cwd) rather than short
project-local snippets, which pulls THOR's average up. The old "~1.5x faster /
~2.1x fewer tokens" headline does not hold on this sample and is retired.
Absolute numbers move with prompt set and machine load between runs (this
document has now measured THOR between 83 and 268 ms depending on the set) -
treat ratios, not any single absolute number, as the takeaway, and treat this
round's ratios as a fresh, honestly sample-dependent measurement rather than a
refutation of the old figures.

Resident RAM was not re-measured this round; the last measured figures were
~570 MB (THOR, semantic daemon) / 0 (THOR, bm25 default) and ~700 MB observed
for mimir. mimir's warm daemon here is opt-in (off by default); its cold path
runs a fast mode that skips the embedder, though on this sample the
PowerShell hook's own process-start cost dominates its latency regardless.

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
  snippets where the query terms cluster. *(Test 2, not re-measured this
  round; previous round: +8 points on the broad shared set, though on the
  strictest dual-written cut mimir stayed ahead by ~3 points - see Test 2
  above.)*
- **It compensates for session drift on every channel.** The as-deployed
  auto-injection, correctly per-scenario project-scoped this round, surfaces
  the drift-preventing fact more often than mimir at its best (74.0% vs 56.2%)
  and fully catches it more often (47.9% vs 42.5%) - and the pins +
  file-touch/command guards (with exact author-declared anchors) cover the
  windows prompt-association cannot reach, by construction.
- **It is faster than mimir's default path.** ~2.4x lower latency than mimir's
  as-deployed cold hook (234 ms vs 564 ms) on this round's sample - though
  mimir's opt-in warm daemon is faster still (66 ms), and on this sample THOR
  injects more tokens than mimir's cold path, not fewer; see Speed and cost
  above for the honest full picture.
- **It never loses a write.** Every fact is an event in a hash-chained append-only
  log; a conflicting edit branches (both heads kept and surfaced) instead of
  overwriting, and `fsck` recomputes the chain so tampering is detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted, daemon
  down - each path falls back to bm25 and can never make recall worse.

## Honest weaknesses

- **mimir retakes the overall multi-project lead (96.7% vs 94.4%).** Its new
  code-content indexing closed what used to be THOR's biggest structural edge
  (Project 3 went from a 16.6-point THOR lead to a dead-even tie) and widened
  mimir's lead on the curated-docs project (100.0% vs 90.0%).
- **mimir now wins code-structure on Test 1 (57.6% vs 53.8%).** Its own
  tree-sitter code-chunk indexing (`CodeChunk`), folded into ordinary
  `recall`, catches up on symbol-shaped questions - the one Test 1 category
  THOR does not lead this round.
- **Only one v4 target was reached.** Of this round's categories, only
  config-how-to cleared an 80% bar on Test 1 (THOR 85.3%); every other Test 1
  category, and every drift metric on all three channels (courier / deliberate
  / mimir, both surfaced and clear-catch), stayed below 80%.
- **On this round's speed sample, mimir's opt-in warm daemon beats THOR's
  latency (66 ms vs 234 ms), and THOR injects more tokens than mimir's cold
  path (579 vs 236)** - the reverse of the headline this document used to
  carry. See Speed and cost above for the honest full picture and why (a
  smaller, differently-composed 20-prompt sample this round).
- **On the strictest dual-written cut mimir won (94.3% vs 91.5%), as of the
  last time Test 2 was measured (v3 round; not re-measured this round - see
  Test 2 above).** Pure memory recall over a small, clean set of hand-written
  notes was mimir's home turf across four fresh juries measured to date.
- **No code-symbol graph.** For "which functions call X" mimir routes to a symbol
  graph; THOR chunks source directly but has no graph queries.
- **Semantic mode has a cost.** It needs a ~235 MB model file plus a warm
  embed-daemon resident (client-only, **off by default**; recall degrades
  cleanly to bm25). This round's speed measurement above ran with the semantic
  daemon resident and does not isolate its cost from bm25-only latency.
- **Maturity.** THOR is new; mimir is battle-tested in daily use.
- **Measurement caveats.** One machine, LLM judging, and a private corpus - so
  these exact numbers are not independently reproducible from this repo (the
  drift mechanism IS reproducible in-repo via `examples/drift_eval.rs` and its
  committed synthetic corpus, unaffected by this round's numbers above). This
  round every re-measured test (Test 1, Test 3, drift) is scored by a 3-judge
  median across three distinct judge lenses, blind with a seeded relabel; Test
  2 was not re-measured this round. Jury strictness and prompt-set composition
  move absolute numbers between runs - the drift correction and the speed
  reversal above are two concrete examples this round. THOR pinned at commit
  `80d001c`, mimir at unreleased main commit `f98c7fd` built from source, no
  store writes between hit generation and judging.

## Method

- Harness: `thor/examples/hits_dump.rs` - the real production paths (fused
  `recall_fused_scoped` for deliberate recall, `courier::injection_for_hook_json`
  for as-deployed injection, this round also invoked per-scenario with
  `--cwd` for the drift courier's corrected project scoping) - for THOR;
  `mimir recall --json` for mimir.
- Judging: every item blind - systems relabelled A/B(/C) with a seeded
  deterministic mapping per question id (`sha256(id) % 2` for the 2-way
  tests, `% 6` for the 3-way drift permutation) - scored 0-2 for
  answer-presence by a **3-judge median** (three distinct judge lenses) for
  every test this round. Hit text is **not** id-stripped: system-revealing
  markers (`m:`/`d:`/`c:` prefixes, bare THOR ULIDs, chunk ids, and THOR's
  `[project: X]` courier tag) remain in the raw text judges see; blinding is
  by relabeling, not by redaction.
- Latency: `thor courier` (production hook) vs mimir's production
  `hook_recall.ps1` (cold, as-deployed default) and mimir's opt-in warm
  `/inject` daemon, wall-clock, median of 20, same 20 prompts for every
  channel.
- Test 1 = 200 questions (118 shared-knowledge + 82 category-stratified) over
  THOR's store. Test 2 = the 118 whose source fact both stores hold, with the
  53-question strict dual-written cut (not re-measured this round). Test 3 =
  45 questions (15 per project) written by an agent reading each repo (ground
  truth, not THOR's store), both systems scoped to the project, top-5 full
  chunks. Drift = 73 fresh-session task prompts (74 raw scenarios, one
  duplicate seq deduped) built from the store's gotchas and decisions, three
  channels measured per scenario, courier scoped per-scenario to its actual
  home project this round (see the correction note above); scenarios without a
  home project run PROJECTLESS on both systems (a neutral working directory).
  No writes were made to either store between hit generation and judging
  (verified with `thor fsck`). THOR pinned at commit `80d001c`; mimir at
  unreleased upstream main commit `f98c7fd`, built from source. Numbers are
  the measured aggregates.
