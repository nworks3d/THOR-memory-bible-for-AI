# THOR vs mimir - the honest picture

An honest, measured comparison of THOR against
[mimir](https://github.com/MakerViking/mimir) on the same machine. Every number
below is measured; nothing is invented, and the weaknesses are listed in full.
Every test was re-measured fresh on 2026-07-11 - the second full measurement
round of that day - after two same-day THOR improvement rounds: the v4
serving/matching round (identifier-aware trigger matching, footer-stripped
serving, full-body memories on the deliberate path, neighbor stitching,
courier full-body typed facts in an 8000-char budget, silence scenarios, a
confusion table and a one-way noise ratchet; the identifier matching and the
confusion-table discipline are idea adoptions from mimir's own rounds,
credited in the README) and a second round at commit `b98c75b` (a retro-tag
sweep over the whole typed population, absolute-evidence trigger scoring, doc
stitching depth 2, wider courier chunk windows, symbol-boundary source
chunking, and 7 distilled decision canonicals). The opponent is mimir's
strongest build to date, unchanged between the two rounds: unreleased upstream
main commit `f98c7fd` (post-v0.13.0: an in-place MatrixCache cache fix, a warm
`/inject` daemon, a fast cold-path mode, code-content indexing as `CodeChunk`,
an identifier RRF leg, recency/type priors), built from source, with code
content indexed and docs re-indexed and re-embedded over the same two main
projects. Every test this round is scored by a **3-judge median** (three
distinct judge lenses), blind, with a fresh salted seeded relabel - no number
below is carried over from an earlier run. Worth stating plainly: **BOTH
systems scored lower on Test 1 than the same-day earlier round** (THOR 71.4%
-> 68.5%, mimir 60.8% -> 59.8%) - jury strictness varies between runs even
with a 3-judge median, and the relative picture, not the absolute level, is
what carries. No writes were made to either store between hit generation and
judging (verified with `thor fsck` before and after).

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
| code-structure | 50.0% | **57.6%** |
| code-behavior | **67.5%** | 54.2% |
| doc-reference | **75.0%** | 60.0% |
| config how-to | **79.4%** | 73.5% |
| gotcha | **76.1%** | 69.6% |
| decision | **70.4%** | 57.4% |
| **overall (n=200)** | **68.5%** | 59.8% |

mimir keeps code-structure (57.6% vs 50.0%) - its tree-sitter code-chunk
indexing (`CodeChunk`) folded into ordinary `recall` beats THOR on
symbol-shaped questions for the second run running; THOR's own new
dependency-free symbol-boundary source chunker landed this round but did not
measurably lift the judged score. THOR leads the other five categories and
overall, by 8.7 points (68.5% vs 59.8%). Both systems scored lower than the
same-day earlier round on this identical corpus (THOR 71.4% -> 68.5%, mimir
60.8% -> 59.8%): the blind maps were freshly re-salted and jury strictness
moves absolute numbers between runs even with a 3-judge median. No category
clears an 80% bar this run (config how-to came closest at 79.4%; it was
85.3% in the earlier same-day run).

## Test 2 - Same knowledge (118 facts both systems have)

The fair, apples-to-apples comparison: only questions whose source fact is a
dual-written memory or a doc chunk **both** stores hold.

**Still not re-measured this round.** No fresh Test 2 data was produced in
either 2026-07-11 round: the shared-ids subset that defines this test did not
map onto this round's item ids, so a fresh cut could not be built without
guessing. Rather than invent a number or silently carry one forward as if it
were fresh, this section keeps the last measured result for reference:

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
| Project 2 | 100.0% | 100.0% (tie) |
| Project 3 | 86.7% | **96.7%** |
| **overall (n=45)** | 92.2% | **98.9%** |

**mimir leads overall, 98.9% vs 92.2%, and widens the gap from the same-day
earlier round** (96.7% vs 94.4%). Its code-content indexing (`CodeChunk`, the
same upgrade behind its Test 1 code-structure win) has fully erased what used
to be THOR's biggest structural edge: Project 3, a 16.6-point THOR lead two
runs ago, is now a mimir win (96.7% vs 86.7%). mimir holds its perfect score
on Project 1's hand-curated docs (100.0% vs 90.0%): hand-curated
architecture/bring-up docs remain a strong retrieval substrate on design
questions. Project 2, THOR's win in the earlier round, is now a 100.0% tie.
THOR no longer wins any project outright on this test.

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
| preventer surfaced (>=partial) | 69.9% | **71.2%** | 50.7% |
| clear catch (fully surfaced) | **50.7%** | **50.7%** | 39.7% |

Both THOR channels clearly beat mimir's best case on both metrics; between
THOR's own two channels the deliberate recall edges the courier slightly on
surfaced (71.2% vs 69.9%) and they tie on clear catch (50.7%). The most
meaningful movement this round is **catch conversion**: of the scenarios where
the courier surfaces the preventer at all, **72.5% are now full, clear catches
(50.7 of 69.9), up from 64.7% in the same-day earlier round (47.9 of 74.0)** -
the wider courier chunk windows serve enough of the fact to count as a clear
catch instead of a partial glimpse. Absolute surfaced percentages moved down a
few points for every channel against the earlier round (the same
jury-strictness variance as Test 1); the relative picture held.

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
reproducible measurement: `cargo run --example drift_eval`. Those in-repo
numbers are a separate, committed synthetic corpus, were not part of this
round's re-measurement, and are unchanged.

## Speed and cost

Full per-prompt cost (process start + recall), median of 20 runs, same machine.
The prompt set is now a **canonical fixed set of 20 prompts** (the first 20
unique drift scenarios by sequence order), documented and reused verbatim
between rounds so speed numbers are cross-run comparable from here on:

| | THOR courier | mimir cold (as-deployed default) | mimir warm (opt-in daemon) |
|---|---:|---:|---:|
| latency (median) | 253 ms | 589.5 ms | **62 ms** |
| tokens injected / prompt (avg) | 679 | 236 | **48** |

THOR is **~2.3x faster** than mimir's as-deployed default path (the
`hook_recall.ps1` PowerShell hook - PowerShell's own process-start cost is
included, since it is a genuine part of that as-deployed latency) - but
mimir's **opt-in warm daemon** (a background process serving `/inject`, single
best-effort memory only, floor-gated, **not** the same `recall -n 8` + dedup
algorithm as the cold path) is itself **~4x faster than THOR** and injects the
fewest tokens by far - with the plain caveat that it is faster and quieter
because it serves less: **5 of the 20 prompts got an empty injection** (below
its single-memory relevance floor; the same five prompt positions as the
earlier round, i.e. deterministic floor behavior, counted as 0 in the
average). Faster and cheaper, but lower coverage on the same prompts. On
tokens, THOR injects **~2.9x more** than mimir's cold path on this set - the
reverse of the headline this document once carried, and slightly worse than
the earlier same-day round (579 -> 679 avg; the plausible causes are THOR's
own round-2 changes - symbol-boundary chunking moved chunk boundaries and the
courier's chunk windows were deliberately widened for drift catches within
the same budget ceiling - reported as measured).

The old "~1.5x faster / ~2.1x fewer tokens" headline does not hold on this
prompt set and stays retired. THOR's 253 ms median is also **marginally over
its own 250 ms latency guardrail** (see Honest weaknesses). Absolute numbers
move with machine load between runs (mimir's cold path re-measured 563.5 ->
589.5 ms on an unchanged store and identical injected chars) - treat ratios,
not any single absolute number, as the takeaway.

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

## Why THOR comes out ahead (and the mechanism)

- **It has the answer at all (coverage).** THOR chunks your repositories into the
  same index auto-recall searches, so a code question is answered automatically at
  the prompt. mimir's new code-content indexing narrows this gap (it now wins the
  code-structure category and the multi-project test), but THOR still leads
  overall coverage as deployed (68.5% vs 59.8%).
- **It ranks better on equal footing (score-fusion + class prior + density
  snippets).** THOR fuses lexical bm25 with a dense multilingual embedding,
  routes knowledge-phrased questions toward hand-written facts, and cuts its
  snippets where the query terms cluster. *(Test 2, not re-measured this
  round; previous round: +8 points on the broad shared set, though on the
  strictest dual-written cut mimir stayed ahead by ~3 points - see Test 2
  above.)*
- **It compensates for session drift on every channel.** Both THOR channels
  surface the drift-preventing fact more often than mimir at its best (courier
  69.9% / deliberate 71.2% vs 50.7%) and fully catch it more often (50.7% vs
  39.7%), and 72.5% of courier surfacings are now full catches (up from 64.7%,
  the wider chunk windows) - and the pins + file-touch/command guards (with
  exact author-declared anchors) cover the windows prompt-association cannot
  reach, by construction.
- **It is faster than mimir's default path.** ~2.3x lower latency than mimir's
  as-deployed cold hook (253 ms vs 589.5 ms) on the canonical prompt set -
  though mimir's opt-in warm daemon is faster still (62 ms, at lower coverage:
  5 of 20 prompts get nothing), and THOR injects more tokens than mimir's cold
  path, not fewer; see Speed and cost above for the honest full picture.
- **It never loses a write.** Every fact is an event in a hash-chained append-only
  log; a conflicting edit branches (both heads kept and surfaced) instead of
  overwriting, and `fsck` recomputes the chain so tampering is detectable.
- **It degrades cleanly.** Semantic off, model missing, sidecar deleted, daemon
  down - each path falls back to bm25 and can never make recall worse.

## Honest weaknesses

- **mimir leads the multi-project test outright (98.9% vs 92.2%).** Its
  code-content indexing erased what used to be THOR's biggest structural edge
  (Project 3 went from a 16.6-point THOR lead two runs ago to a mimir win,
  96.7% vs 86.7%), it keeps a perfect score on the curated-docs project
  (100.0% vs 90.0%), and THOR's only prior project win is now a tie. THOR
  wins no project outright this round.
- **mimir keeps code-structure on Test 1 (57.6% vs 50.0%).** Its tree-sitter
  code-chunk indexing (`CodeChunk`), folded into ordinary `recall`, beats THOR
  on symbol-shaped questions for the second run running - and THOR's own new
  dependency-free symbol-boundary source chunker, which landed this round,
  did not measurably lift the judged score.
- **The v4 "80% goal" round closed at 0 of 8 gates on this stricter run.**
  In the same-day earlier run one gate held (config how-to, 85.3%); on this
  re-run it slipped to 79.4%, and every other Test 1 category and every drift
  metric on all three channels stayed below 80% as well. Zero of the eight
  targets stand on the latest measurement.
- **On the canonical speed set, mimir's opt-in warm daemon beats THOR's
  latency (62 ms vs 253 ms), and THOR injects more tokens than mimir's cold
  path (679 vs 236, worse than the earlier round's 579)** - the reverse of
  the headline this document once carried. The warm daemon's caveat (5 of 20
  prompts served nothing) is real but does not erase the latency gap.
- **THOR's courier median (253 ms) is marginally over its own 250 ms
  guardrail** - the symbol-boundary chunking sits on the freshness path and
  the budget needs to be won back.
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
  median across three distinct judge lenses, blind with a fresh salted seeded
  relabel; Test 2 was not re-measured. Jury strictness moves absolute numbers
  between runs even with a 3-judge median - both systems scored lower on Test
  1 than the same-day earlier round on an identical corpus, and the earlier
  round's drift correction and speed reversal are further concrete examples.
  THOR pinned at commit `b98c75b`, mimir unchanged at unreleased main commit
  `f98c7fd` built from source, no store writes between hit generation and
  judging.

## Method

- Harness: `thor/examples/hits_dump.rs` - the real production paths (fused
  `recall_fused_scoped` for deliberate recall, `courier::injection_for_hook_json`
  for as-deployed injection, this round also invoked per-scenario with
  `--cwd` for the drift courier's corrected project scoping) - for THOR;
  `mimir recall --json` for mimir.
- Judging: every item blind - systems relabelled A/B(/C) with a seeded
  deterministic mapping per question id (`sha256(salt + id) % 2` for the
  2-way tests, `% 6` for the 3-way drift permutation; the salt is fresh per
  round, so each round's blind maps genuinely reshuffle) - scored 0-2 for
  answer-presence by a **3-judge median** (three distinct judge lenses) for
  every test this round. Hit text is **not** id-stripped: system-revealing
  markers (`m:`/`d:`/`c:` prefixes, bare THOR ULIDs, chunk ids, and THOR's
  `[project: X]` courier tag) remain in the raw text judges see; blinding is
  by relabeling, not by redaction.
- Latency: `thor courier` (production hook) vs mimir's production
  `hook_recall.ps1` (cold, as-deployed default) and mimir's opt-in warm
  `/inject` daemon, wall-clock, median of 20, the canonical fixed 20-prompt
  set for every channel; each channel's timed loop runs alone, with a
  discarded warm-up call and fresh per-run session ids (a first attempt that
  reused a session id was suppressed by the courier's own session ledger and
  was discarded and fully re-run).
- Test 1 = 200 questions (118 shared-knowledge + 82 category-stratified) over
  THOR's store. Test 2 = the 118 whose source fact both stores hold, with the
  53-question strict dual-written cut (not re-measured this round). Test 3 =
  45 questions (15 per project) written by an agent reading each repo (ground
  truth, not THOR's store), both systems scoped to the project, top-5 full
  chunks. Drift = 73 fresh-session task prompts (74 raw scenarios, one
  duplicate seq deduped) built from the store's gotchas and decisions, three
  channels measured per scenario, courier scoped per-scenario to its actual
  home project (see the correction note above); scenarios without a home
  project run PROJECTLESS on both systems (a neutral working directory).
  No writes were made to either store between hit generation and judging
  (verified with `thor fsck`). THOR pinned at commit `b98c75b` (store
  re-ingested under the new symbol-boundary chunking, semantic sidecar fully
  synced before measuring); mimir unchanged at unreleased upstream main
  commit `f98c7fd`, built from source. Numbers are the measured aggregates.
