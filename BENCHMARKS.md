# THOR vs mimir - the honest picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.
Every test was re-measured fresh in the **V5 round** (THOR commit `b03c920`),
the third full measurement round after the two v4-era rounds. What changed on
THOR's side since the previous round: a store-wide fires-when rewrite to
**bilingual task vocabulary** (the measured lesson: single-language triggers
never fire on prompts in the other language), footer repairs plus a
write-guard that refuses malformed footers at the MCP layer, full-body serving
for all hand-written memories, a same-file sibling vote in fused scoring,
author-declared **acronym** triggers that authorize on one exact token plus
conservative prefix-stemming from 5 chars, a conservative decay review (280
candidates: 216 kept, 61 retyped, 1 retracted), and a **warm inject daemon**
(idea credit: mimir) with a proven byte-identical warm==cold decision path.
The measurement instruments also became metabolism-proof this round:
**content-addressed gold scoring** next to entity ids, and the Test 2 shared
subset rebuilt on the current id space. The opponent is mimir's strongest
build to date, unchanged: unreleased upstream main commit `f98c7fd`
(post-v0.13.0: a warm `/inject` daemon, a fast cold-path mode, code-content
indexing as `CodeChunk`, an identifier RRF leg, recency/type priors), built
from source, its index and embeddings refreshed right before the run. Every
test is scored by a **3-judge median** (three distinct judge lenses), blind,
with a fresh random side assignment - no number below is carried over from an
earlier run. Worth stating plainly: jury strictness moves absolute numbers
between runs even with a 3-judge median - **this round THOR's Test 1 overall
moved down (68.5% -> 63.8%) while mimir's moved up (59.8% -> 64.0%)** on the
identical corpus; the per-test detail below tells the real story. No writes
were made to either store between hit generation and judging (verified with
`thor fsck` before and after).

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
| code-structure | 42.4% | **63.6%** |
| code-behavior | 60.0% | **62.5%** |
| doc-reference | **70.0%** | 66.2% |
| config how-to | **79.4%** | 76.5% |
| gotcha | **71.7%** | 65.2% |
| decision | **72.2%** | 55.6% |
| **overall (n=200)** | 63.8% | **64.0%** |

**A statistical tie overall (63.8% vs 64.0%), and the first round mimir's
overall is not behind.** The split is clean: THOR leads every knowledge-shaped
category (decision +16.6, gotcha +6.5, doc-reference +3.8, config how-to
+2.9), mimir leads both code categories, code-structure now decisively (63.6%
vs 42.4% - its tree-sitter `CodeChunk` indexing plus this round's fresh
re-index of THOR's own changed source tree; THOR's dependency-free
symbol-boundary chunker and the new same-file sibling vote did not close that
gap on the judged score). Note the corpus includes 44 questions whose source
docs were deliberately removed from the repo since the corpus was authored -
both systems score near zero on those, which drags both overalls down
equally; see Test 2 for the cuts that exclude them. No category clears an 80%
bar this run (config how-to closest again at 79.4%).

**V6 code-round addendum (same evening).** The two code categories were
diagnosed A-B style (93 questions, mechanical gold-coverage on the real fused
channel) and re-judged by a fresh category-scoped 3-lens blind mini-jury after
two targeted fixes: (1) **serving parity** - the deliberate path now serves a
found chunk's FULL body instead of a 500-char query window (mimir was always
judged on full bodies; the window frequently cut the answer out of the chunk
THOR had already found), and (2) a **path-affinity ranking bonus** - a query
that names a file by its stem ("the Fleet page", "the event store") lifts that
file's chunks (the path sits once in a chunk footer, so bm25 barely felt it;
deliberate path only - on the courier pool it measurably displaced drift
preventers and stays off). Re-judged result on the identical 93 items:
**code-behavior THOR 71.7% vs mimir 57.5%** (was 60.0 vs 62.5 - the category
flipped) and **code-structure THOR 50.0% vs mimir 57.6%** (was 42.4 vs 63.6 -
gap halved, still a mimir win). A pre-registered per-symbol/nested chunking
A-B on a store clone came out net NEGATIVE (smaller chunks win
structure-shaped questions but lose behavior context) and was rejected, same
verdict as the earlier round's chunker attempt; the remaining structure gap is
the symbol-graph route (see SIMILAR-PROJECTS.md R2), not chunk-shuffling.
Note: 4 of the 33 structure items reference repo-removed sources on which both
systems score zero, capping the reachable maximum at ~88%.

## Test 2 - Same knowledge (cuts over the judged Test 1 set)

The fair, apples-to-apples comparison: only questions whose source fact
**both** stores verifiably hold. **Re-measured this round** - the shared-ids
subset was rebuilt on the current id space (the old subset predated a round
of store metabolism and could no longer be mapped without guessing):

- **strict dual-written cut (n=53)**: questions whose source is a live THOR
  memory head carrying the one-time-import provenance marker - zero doubt
  both stores have the fact. All 53 historical dual-written facts survived
  store metabolism at the entity level.
- **broad shared cut (n=152)**: strict + questions whose source FILE is live
  in both stores (verified read-only against mimir's own file index; grew
  from the old n=118 because mimir now indexes code files too). The 44
  questions whose source docs were removed from the repo are excluded - that
  knowledge was deliberately dropped, in both stores' measurement sense.

| cut | THOR | mimir |
|---|---:|---:|
| strict dual-written (n=53) | **96.2%** | 93.4% |
| broad shared (n=152) | 77.0% | **81.2%** |

**THOR wins the strict dual-written cut for the first time (96.2% vs 93.4%).**
Pure memory recall over the clean set of hand-written notes was mimir's home
turf across four earlier juries (it won 94.3 vs 91.5 the last time this was
measured); the V5 serving and trigger work (full-body memories, bilingual
task-vocabulary fires-when, footer repairs) flipped it. mimir takes the broad
shared cut (81.2% vs 77.0%), which is two-thirds code/doc-chunk questions -
consistent with its Test 1 code-category lead. The cut definitions changed
this round (rebuilt subset, code files included in "shared"), so compare cuts
within this round, not against the old n=118 number.

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
| Project 1 | 90.0% | **93.3%** |
| Project 2 | **100.0%** | 86.7% |
| Project 3 | 86.7% | **96.7%** |
| **overall (n=45)** | 92.2% | 92.2% (tie) |

**A dead tie overall (92.2% vs 92.2%); mimir's previous-round lead (98.9% vs
92.2%) is gone.** THOR holds its 100.0% on Project 2 (last round a 100-100
tie; mimir slipped to 86.7% there), mimir keeps Project 1 and Project 3.
Per-project numbers on n=15 move with a single question (6.7 points each), so
treat the project split as noisy and the overall tie as the takeaway.

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
| preventer surfaced (>=partial) | **72.1%** | 62.5% | 58.9% |
| clear catch (fully surfaced) | **55.9%** | 48.6% | 47.9% |

New this round: the three channels are judged **together, three-way blind**
(each scenario's three context blocks shuffled onto anonymous sides A/B/C), so
the columns are directly comparable within one jury pass. **The as-deployed
courier posts its best drift numbers to date - 72.1% surfaced / 55.9% clear
catch (previous round 69.9 / 50.7)** - despite this jury being stricter on
Test 1; the V5 trigger work (bilingual task vocabulary, acronym
authorization, prefix-stemming) is the round's targeted change behind it.
mimir also improved measurably (58.9 / 47.9, from 50.7 / 39.7). The courier
now beats THOR's own deliberate recall on both metrics - the author-declared
fires-when gates fire on task prompts that score-only ranking misses. (n
detail: a handful of items lack a full 3-lens score on one side - effective n
is 68/72/73 for courier/deliberate/mimir respectively.)

**Correction note (earlier same-day round; still applies to older published
numbers).** In the first 2026-07-11 round the courier channel was initially
generated with a single *fixed* working directory (`The-AI-memory-bible`) for
all 73 scenarios. That was an instrument bug, not a courier limitation: 37 of
the 73 scenarios have a different true home project (mostly Project 3), and
the project-scoped courier - correctly - excluded their preventer facts when
run from the wrong directory. Under that bug the courier scored only 35.6%
surfaced. **The same fixed-cwd bug was present, unnoticed, in the courier
numbers this document published before 2026-07-11** (65.8% surfaced / 45.2%
clear catch) - those understate the as-deployed courier; read them as
historical. This round's measurement used the corrected per-scenario home
scoping from the start (home mapping taken from the corpus metadata, two
per-home harness invocations, 0/73 courier-silent).

None of the three channels clears an 80% bar on either metric this round. The
channels built for the post-compaction window on top of this - **pinned
rules** (re-injected in full by construction) and the **file-touch + command
guards** (fired at the moment of action, with exact author-declared `anchors`;
16/16 surfaced on the committed corpus) - are reported via the in-repo
reproducible measurement: `cargo run --example drift_eval` (courier catch
35/46 with noise 1 on the committed frozen corpus at this commit; the one-way
noise ratchet held throughout the V5 round). The live replay instrument also
reports a **content-surfaced** metric now (one served hit carrying >= 50% of
the gold's key terms), because entity-id matching mechanically undercounts
after store metabolism - ids stay reported for continuity.

## Speed and cost

Full per-prompt cost (process start + recall), median of 20 runs, same machine.
The prompt set is now a **canonical fixed set of 20 prompts** (the first 20
unique drift scenarios by sequence order), documented and reused verbatim
between rounds so speed numbers are cross-run comparable from here on:

| | THOR cold | THOR warm (opt-in daemon, NEW) | mimir cold (as-deployed default) | mimir warm (opt-in daemon) |
|---|---:|---:|---:|---:|
| latency (median) | 206.8 ms | 192.7 ms | 580.7 ms | **38.9 ms** |
| tokens injected / prompt (avg, chars/4) | 784 | 715 | 237 | **32** |
| empty injections | 0/20 | 0/20 | 0/20 | 10/20 |

New this round: THOR has its own **opt-in warm inject daemon** (`thor daemon`
+ a SessionStart `ensure-daemon` hook; idea credit mimir, independent
reimplementation). It is honest about what it buys: the warm path answers the
IDENTICAL decision through the same gates and ledger (byte-parity proven by
unit test and live A/B) and saves the process/store/model startup, **not** the
recall itself - so 206.8 -> 192.7 ms on this steady-state set, with the real
win on a session's first prompt when nothing is warm yet. THOR's cold median
also dropped back **under its 250 ms guardrail** (253 -> 206.8 ms).

THOR cold is **~2.8x faster** than mimir's as-deployed default path (the
`hook_recall.ps1` PowerShell hook; PowerShell's process-start cost included,
as it is genuine as-deployed latency). mimir's opt-in warm daemon remains by
far the fastest and cheapest channel (38.9 ms, ~32 tokens) with the same
structural caveat as before, more pronounced this run: it serves a single
floor-gated best-effort memory, and **10 of the 20 prompts got an empty
injection** (5 of 20 the previous round; its index changed in between - its
own re-index of fresh content - reported as measured). Faster and cheaper
because it serves less. On tokens, THOR injects **~3.3x more** than mimir's
cold path on this set - the reverse of the headline this document once
carried stays retired.

Measurement caveat: the store is live, and the cold pass's injections shift
the session-ledger/usage state that the warm pass then sees, so the two THOR
columns' served chars differ slightly; the byte-parity claim is proven at
equal state, not by this table. Absolute numbers move with machine load
between runs - treat ratios, not any single absolute number, as the takeaway.

Resident RAM was not re-measured this round; the last measured figures were
~570 MB (THOR, semantic daemon) / 0 (THOR, bm25 default) and ~700 MB observed
for mimir. mimir's warm daemon here is opt-in (off by default); its cold path
runs a fast mode that skips the embedder, though on this set the PowerShell
hook's own process-start cost dominates its latency regardless.

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

## Where each wins (and the mechanism)

The overall Test 1 number is a tie this round; the structure underneath it is
not. What THOR wins, and why:

- **Knowledge-shaped recall.** Every hand-written-knowledge category on Test 1
  (decision +16.6, gotcha +6.5, doc-reference +3.8, config how-to +2.9) and -
  for the first time - **the strict dual-written cut (96.2% vs 93.4%)**, the
  cleanest equal-corpus comparison there is. Mechanism: typed facts served
  full-body, author-declared bilingual fires-when triggers, score-fusion with
  a knowledge-vs-code class prior.
- **Session-drift compensation, the product's core purpose.** The as-deployed
  courier beats mimir's best case on both metrics (72.1% vs 58.9% surfaced,
  55.9% vs 47.9% clear catch) at its own all-time high - and the pins +
  file-touch/command guards (exact author-declared anchors) cover the windows
  prompt-association cannot reach, by construction.
- **As-deployed latency.** ~2.8x faster than mimir's default cold hook (206.8
  vs 580.7 ms), back under its own 250 ms guardrail, with an opt-in warm
  daemon (192.7 ms) that provably serves the identical decision.
- **It never loses a write.** Every fact is an event in a hash-chained
  append-only log; a conflicting edit branches (both heads kept and surfaced)
  instead of overwriting, and `fsck` recomputes the chain so tampering is
  detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted,
  daemon down - each path falls back to bm25 and can never make recall worse.

What mimir wins, and why: **both code categories on Test 1** (code-structure
decisively, 63.6% vs 42.4%) and with them **the broad shared cut (81.2% vs
77.0%)** - its tree-sitter `CodeChunk` indexing produces better-ranked code
answers than THOR's dependency-free chunker; and **the warm-daemon latency
class** (38.9 ms), by serving a single floor-gated memory instead of a full
injection block.

## Honest weaknesses

- **Test 1 overall is no longer a THOR lead - it is a tie (63.8% vs 64.0%),
  and the movement was asymmetric**: on the identical corpus THOR moved down
  (68.5 -> 63.8) while mimir moved up (59.8 -> 64.0). Jury strictness
  explains between-run level shifts, not a sign flip in the gap; mimir's
  fresh re-index (including THOR's own fast-moving source tree) is the
  plausible driver, and the honest reading is that mimir's code-content
  indexing keeps compounding.
- **mimir still wins code-structure (57.6% vs 50.0% on the V6 re-judge; was
  63.6% vs 42.4%).** The V6 serving-parity + path-affinity round halved the
  gap and flipped code-behavior outright, but structure-shaped questions
  ("what is the shape of X's state") keep favoring mimir's symbol-level
  retrieval; the chunking A-B that targeted it came out net negative and was
  rejected. The symbol-graph sidecar (SIMILAR-PROJECTS.md, R2) is the open
  route.
- **mimir wins the broad shared cut (81.2% vs 77.0%)** - two-thirds of that
  cut is code/doc-chunk questions, the same weakness as above on an equal
  corpus.
- **The "80%-everywhere" goal still does not stand: 0 of 8 v4 gates.** No
  Test 1 category clears 80% (config how-to closest at 79.4%) and no drift
  metric does either (courier surfaced 72.1% is the closest any channel has
  come).
- **On the canonical speed set, mimir's opt-in warm daemon beats THOR's
  latency class outright (38.9 vs 192.7 ms warm)** - by serving a single
  floor-gated memory (10 of 20 prompts empty this run) instead of a full
  block; and THOR injects ~3.3x more tokens than mimir's cold path. Ratio of
  value to tokens is a real open question, not spin.
- **Three drift golds remain honest misses**: deep-drift scenarios whose
  prompts deliberately share no vocabulary with the fact (documented in the
  eval corpus notes); lexical triggers cannot bridge them and the semantic
  leg does not yet either. They were left as gaps rather than trigger-stuffed
  (the no-overfit rule: triggers stay body-derived).
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
  committed synthetic corpus). This round every test - including the rebuilt
  Test 2 cuts and the drift three-way - is scored by a 3-judge median across
  three distinct judge lenses, blind with a fresh random side assignment,
  ONE run, no re-rolls. Jury strictness moves absolute numbers between runs
  even with a 3-judge median - this round's asymmetric Test 1 movement is
  the latest concrete example. THOR pinned at commit `b03c920`, mimir
  unchanged at unreleased main commit `f98c7fd` built from source, its index
  and embeddings refreshed before the run; no store writes between hit
  generation and judging (verified with `thor fsck`).

## Method

- Harness: `thor/examples/hits_dump.rs` - the real production paths (fused
  `recall_fused_scoped` for deliberate recall, `courier::injection_for_hook_json`
  for as-deployed injection, this round also invoked per-scenario with
  `--cwd` for the drift courier's corrected project scoping) - for THOR;
  `mimir recall --json` for mimir.
- Judging: every item blind - systems relabelled onto anonymous sides (A/B
  for the 2-way tests, A/B/C for the 3-way drift) with a fresh
  cryptographically-seeded random assignment per item, recorded in per-test
  map files the judges never see - scored 0 / 0.5 / 1 for answer-presence per
  side by a **3-judge median** (three distinct judge lenses:
  strict-evidence, practical-agent, gold-coverage); ONE run, no re-rolls.
  Hit text is **not** id-stripped: system-revealing markers (`m:`/`d:`/`c:`
  prefixes, bare THOR ULIDs, chunk ids, and THOR's `[project: X]` courier
  tag) remain in the raw text judges see; blinding is by relabeling, not by
  redaction.
- Latency: four channels on the canonical fixed 20-prompt set, wall-clock,
  median of 20, each channel's timed loop alone with a discarded warm-up and
  fresh per-run session ids: `thor courier` with the inject daemon stopped
  (cold) and running (warm), mimir's production `hook_recall.ps1` (cold,
  as-deployed default), and mimir's opt-in warm `/inject` daemon (started
  fresh, killed after).
- Test 1 = 200 questions (shared-knowledge + category-stratified) over THOR's
  store. Test 2 = two cuts over the judged Test 1 medians using the rebuilt
  shared-ids subset (strict dual-written n=53; broad shared n=152; 44 dead
  doc questions excluded - see Test 2). Test 3 = 45 questions (15 per
  project) written by an agent reading each repo (ground truth, not THOR's
  store), both systems scoped to the project, top-5 full chunks. Drift = 73
  fresh-session task prompts (74 raw scenarios, one duplicate seq deduped)
  built from the store's gotchas and decisions, three channels judged
  together three-way blind per scenario, courier scoped per-scenario to its
  actual home project; scenarios without a home project run PROJECTLESS on
  both systems (a neutral working directory). No writes were made to either
  store between hit generation and judging (verified with `thor fsck`
  before and after, all checks green). THOR pinned at commit `b03c920`
  (deployed binary verified byte-identical to the build, semantic sidecar
  fully synced); mimir unchanged at unreleased upstream main commit
  `f98c7fd`, built from source, index + embeddings refreshed before the
  run. Numbers are the measured aggregates.
