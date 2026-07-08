# THOR vs mimir - recall head-to-head

A blind, judged comparison of THOR's recall against [mimir](https://github.com/MakerViking/mimir)
on the same machine over the same knowledge, measuring **answer quality**: does a
system's top-5 actually contain a correct answer to the question?

## Method

- **52 real questions across 6 categories** (code-structure, code-behavior,
  doc-reference, config-how, gotcha, decision). The corpus references private
  project internals, so only the aggregate scores are published here - not the
  questions.
- Each question ships with a written *"what a correct answer must contain"* ground
  truth.
- Both systems return their **top-5** for each question. An independent judge
  scores each result set **0-2** (2 = a hit clearly contains the answer,
  1 = on-topic / partial, 0 = miss), **blind** to which system produced which set
  (the two sets are relabelled A/B per question and their ids are stripped, so the
  judge scores on content alone).
- THOR runs with the **semantic score-fusion** recall layer enabled; mimir runs
  its normal hybrid recall. Same machine, same underlying knowledge.

## Result (average answer-presence per category, scale 0-2)

| category        |  THOR  | mimir  | winner |
|-----------------|:------:|:------:|:------:|
| code-structure  | **1.78** | 0.67 | THOR |
| code-behavior   | 1.25 | 1.25 | tie |
| doc-reference   | **1.78** | 1.67 | tie |
| config-how      | 1.56 | 1.44 | tie |
| gotcha          | 2.00 | 2.00 | tie |
| decision        | 1.88 | 1.88 | tie |
| **overall**     | **1.71** | 1.48 | **THOR** |

**THOR 89 vs mimir 77 points (of 104) - 85.6% vs 74.0%.** THOR wins or ties every
category and loses none.

## What changed: the semantic layer closed the paraphrase gap

An earlier head-to-head, with THOR on pure lexical (bm25) recall, had mimir AHEAD
on the two "meaning" categories - code-behavior (mimir 1.38 vs THOR 0.88) and
doc-reference (mimir 1.44 vs THOR 1.33). Those are exactly what the dense layer
targets. With score-fusion on:

- **code-behavior:** THOR 0.88 -> **1.25** (caught mimir).
- **doc-reference:** THOR 1.33 -> **1.78** (moved ahead).

Measured internally on the same battery, score-fusion lifts THOR's own recall@5
from **67% to 73%** with no category regression.

## Honest weaknesses

- **code-behavior is a tie at 1.25** - pure paraphrase ("what does X do when Y")
  is still the hardest category for *both* systems; roughly 40% is left on the
  table. THOR caught up here; it does not beat mimir.
- THOR's overall lead is partly carried by **code-structure** (it chunks source
  directly, where mimir routes to a symbol graph). Strip that one category and
  it is **84.9% vs 82.6%** - a slim THOR edge, no longer a loss on the rest.
- This measures recall **quality only** - not latency, and not the "noise" half
  (how often a system injects irrelevant context). Those are separate tests.
- Single judge per question, and the corpus is private, so these exact numbers
  are not independently reproducible from this repo. The harness is
  `thor/examples/recall_eval.rs` plus a blind-judge pass.
