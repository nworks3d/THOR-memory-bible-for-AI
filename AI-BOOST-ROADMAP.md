# AI-Boost Roadmap — making THOR measurably better for the agent

> **Implementation status (2026-07-09).** After a second audit round the priorities
> were reshuffled (capture → feedback → moment-of-action → re-orientation →
> freshness) and the first tranche is **shipped and lab-verified** on this branch:
> Stop-hook **capture nudge** (keyword-gated, once/session, loop-safe); **mark**
> tool + echo prior in the courier; **file-touch guard** (first Edit of a file →
> memories that name it, never chunks); **pin + post-compaction `<thor-brief>`**
> (standing rules guaranteed back after a compaction — the measured "gotcha ranked
> 7th" NAS scenario is now covered by construction); **per-session injection
> ledger** (no repeated blocks, rotation, cleared on compact); **silence gate**
> (the measured "ga verder met de sync refactor" noise injection is now silent);
> **live-file freshness** (`[refreshed]`/`[stale?]`); the full **MCP stewardship
> toolset** (revise/retract/resolve/history/mark/pin/unpin/reproject/brief,
> `kind:"memory"` recall, duplicate-refusing typed remember, CAS-checked writes);
> and the **dense-leg head-filter bugfix** (compile-verified here; run
> `cargo test --features semantic` locally to execute its regression test).
> Phase 1's ranking work (source-class prior, term-coverage rerank) and the
> reproducible drift eval remain open — see below.

## Open points (next round) + expected outcomes

In priority order:

1. **Validate + deploy locally** (first action, owner's machine): run
   `cargo test --features semantic` (the cloud environment could not download
   the onnxruntime binaries), install the new binary + hooks, **replace the old
   live MCP server** (still a create-only, unscoped build — verified live), and
   pin the standing rules (at minimum the WAL/NAS gotcha and the
   source-of-truth rule).
2. **Reproducible drift eval** (`examples/drift_eval.rs` + a committed scenario
   corpus): makes the 39.7% full-catch falsifiable and is the measurement gate
   for everything below.
3. **Ranking track (phase 1)**: source-class prior (memories/docs over code,
   query-routed) + term-coverage/proximity rerank. Target: dual-written cut
   82.1% → ~88-90%, Project 1 docs 67% → 80%+.
4. **One footer module + write-time fact_type** (review finding): three parsers
   and one inline writer currently share only a convention; stamp the type
   structurally at write time, keep the body parser as an import shim.
5. **Later**: fused-recall parity over MCP (semantic server build), a reserved
   typed courier slot, capture triggers as a rulebook class instead of a const,
   hook-ledger state in SQLite (race-free), freshness in `get`/MCP-recall/brief.

**Expected outcomes to hold this work against:**

- *By construction, already demonstrated in the lab*: pinned rules ~100%
  present after a compaction (previously: a measured miss); zero repeated
  injection blocks within the 5-prompt window; the measured noise-injection
  scenario now silent; file-naming gotchas surfaced at the moment of action;
  near-duplicates refused at write time.
- *To re-verify via the drift eval + a fresh benchmark run*:
  preventer-surfaced 54.8% → ≥65%; full-catch 39.7% → ≥55% on
  compaction/file-touch scenarios; injected tokens/prompt in focused sessions
  ~239 → <100.
- *Qualitative, over weeks of use*: the agent maintains the store
  (revise/retract/mark/resolve visible in the log), no banner blindness, and
  the capture nudge catches missed facts at ≤1 nudge/session without
  irritating (watch its false-positive rate).
- *Honest caveat*: the pin/guard channels bypass ranking rather than improve
  it — the pure ranking win (dual-written cut toward mimir's 89.6%) only
  arrives with point 3.

An improvement trajectory derived from a full code walk (all of `thor/src`),
the published benchmarks, and **empirical verification**: the binary was built
and exercised against a seeded store (this repo ingested + hand-written
gotcha/decision memories), and the deployed MCP server was queried live.
Every problem below was *observed*, not assumed.

## Verified problems (reproduced, not theorized)

1. **The drift miss is real and reproducible.** With the WAL gotcha
   ("never open thor.db over a network share") stored, the task prompt
   *"move the thor database to the NAS so both machines can use it"* injects
   three code/config chunks — and **not the gotcha** (it ranks 7th on a close
   paraphrase and outside the top-8 on the task phrasing). The agent would
   make exactly the mistake the stored fact prevents. This reproduces the
   benchmark's 39.7% full-catch in one shot.
2. **Ranking is type-blind.** A hand-written decision/gotcha competes against
   1500+ source chunks on raw bm25(+cosine). Code chunks are long, numerous
   and identifier-dense, so they crowd memories and curated docs out of the
   top-3 — the shared root of *both* measured losses (dual-written cut 82.1%
   vs mimir 89.6%; Project 1 docs 67% vs 93%). Fact types (`[memory/gotcha …]`)
   already exist in bodies as footers but are invisible to ranking.
3. **The courier is stateless.** The identical `<thor-recall>` block is
   re-injected byte-for-byte every prompt (verified). ~239 tokens/prompt of
   mostly repeats; hits ranked 4-6 — where the drift preventer often sits —
   can never surface. `session_id` arrives in the hook JSON and is unused.
4. **Paraphrase/cross-lingual recall misses live.** On the deployed store, a
   Dutch query for a memory whose English body states the exact answer did
   not surface it in the top-5 (bm25 cannot bridge the wording; the semantic
   layer did not catch it).
5. **The MCP surface is create-only and unscoped.** The live server exposes
   only `recall(query, limit)` / `get` / `remember(body)`: no project scoping
   (cross-project bleed observed live), no revise/retract/resolve/history, no
   duplicate refusal, provenance hardcoded. When auto-injection misses, the
   agent's manual recovery path is weak.
6. **A real dense-leg bug.** In `recall_fused_scoped` the dense candidate
   filter checks scope but not head-membership (`recall.rs:562-568`, compare
   `lexical_head_pool` at `recall.rs:427-436`), so superseded revisions of
   frequently-edited facts eat the 64 dense slots and are then discarded —
   fused recall silently decays toward bm25-only as a store ages.

## The trajectory

Proposals were generated from five lenses, adversarially verified against the
actual code (16/16 survived with sharpened mechanisms), and ranked by a
3-judge panel on impact-per-effort. Phases are ordered so each is measurable
before the next lands.

### Phase 0 — Make drift measurable in-repo (small)
**Reproducible drift eval** (`examples/drift_eval.rs` + a committed
`eval/drift_scenarios.jsonl`): seed a temp store per scenario, run the real
courier path (top-3) and the guard channel, score preventer-surfaced /
full-catch per channel. Today's 39.7% comes from a private single-judge
corpus; nothing below can be tuned or trusted without this gate.

### Phase 1 — Fix the ranking core (small→medium, biggest measured wins)
1. **Head-filter the dense leg + per-class dense quotas** (small; top-ranked
   9.0/10). Three-line bug fix mirroring the lexical leg's guards, plus a
   reserved memory quota (e.g. 16/64) in `DENSE_TOPM` so memories always have
   dense representation. Moves the same-knowledge set and stops aging decay.
2. **Typed constraint facts + a reserved injection slot** (small; DRIFT).
   Parse the existing `[memory/<type> …]` footers into a `FactType`
   (gotcha/decision/preference), let `remember`/`create` write them, and give
   the courier a slot policy: recall a pool of ~12, reserve one of the three
   slots for the best typed constraint. Expected: preventer-surfaced 54.8% →
   ~65%, Project 1 67% → ~80%+.
3. **Source-class prior, query-routed** (medium). Classify hits by entity id
   (memory / doc chunk / code chunk — a pure id parse) and the query by
   surface form (identifiers → code; decision vocabulary → knowledge), then
   apply a small class delta in fusion. Attacks the dual-written cut (82.1% →
   ~88-90%) without regressing code categories.
4. **Term-coverage + proximity rerank of the fused top pool** (medium). A
   cheap cross-encoder substitute: boost candidates matching *all* query
   terms tightly over long chunks matching a few terms often — precisely the
   failure signature behind mimir's win on clean notes.

### Phase 2 — Context economy: right tokens, right moment (medium)
5. **Per-session injection ledger** (DRIFT; judges 8.5). Key a small fail-open
   sidecar file on the hook's `session_id`: suppress revs injected in the
   last N prompts, rotate hits 4-10 in, clear the ledger when SessionStart
   reports `source=compact`. Repeat-token cost drops from ~239/prompt to
   <100 while suppressed slots let deeper hits surface.
6. **Compaction-aware re-orientation** (DRIFT; 8.4). Today THOR spends its
   budget when context is full and nothing at the moment it is empty — the
   timing is inverted. On `SessionStart(source=compact)`: inject a one-time
   generous digest (recent + typed heads, diverged entities, ~2k tokens) and
   re-pin constraints that had already been surfaced pre-compaction (from the
   transcript). Full-catch on compaction scenarios → ~55-65%.
7. **Absolute confidence floor** (7.0). Min-max normalization makes the best
   of an all-junk pool score 1.0; the courier currently injects *something*
   for every non-trivial prompt. Add an absolute cosine/strict-AND floor on
   the fused path so "THOR has nothing" injects nothing.

### Phase 3 — The agent-facing surface and the moment of action
8. **First-class MCP surface** (medium; unanimous judges' pick — see below).
9. **Memory-backed PreToolUse guard** (large; DRIFT). Recall constraints
   against the *tool call* (file path being edited, command being run), not
   just the prompt — drift is decided at the moment of action. Includes
   un-bricking the guard on non-Windows (`default_rulebook_path` is a silent
   no-op without `LOCALAPPDATA`, `guard.rs:26-34`).
10. **Situation-trigger dual embeddings** (medium; DRIFT). Embed gotchas by
    their *when* (the triggering situation) in a second sidecar slot,
    max-merged at query time — the drift prompt describes the task, never the
    constraint text.
11. **Knowledge quality**: structure-aware chunking with symbol/heading
    breadcrumbs and line ranges (7.3); code-anchored staleness marking (6.4);
    optionally a derived symbol sidecar for outline/where-used (6.4, large) —
    THOR's answer to mimir's code-symbol graph without giving up unified
    recall.

## The personal pick besides drift: a first-class MCP surface

All three judges independently chose it, and this session demonstrated why
live: auto-injection *will* miss (54.8% preventer-surfaced is the measured
ceiling today), and when it does, deliberate tool use is the agent's recovery
move — currently the weakest surface in the product:

- `recall` cannot scope to a project (bleed observed live) and runs bm25-only
  over MCP even when the semantic sidecar exists (no fused parity).
- `remember` is create-only with `parent_rev: None`: an agent that learns a
  fact changed can only mint a DIVERGED second head or leave the drift in
  place. No `revise`, `retract`, `resolve`, `history`.
- No near-duplicate refusal (the "recall before remembering" instruction is
  unenforced), provenance is hardcoded (`session_id: "mcp"`), and hits give a
  220-char snippet without advertising `get <id>` for progressive disclosure.

Concretely: add `revise`/`retract`/`resolve`/`history` tools using a
`BEGIN IMMEDIATE` checked-append (mirror `append_resolve`,
`event_store.rs:501-518`) so a concurrent write returns a typed conflict
instead of a silent branch; refuse near-duplicates server-side at
`remember`-time; thread real session/project provenance from the MCP context;
give MCP `recall` the same scoping and fused path the courier has; and stamp
every hit with type + a `get`-pointer. Expected effect: the dual-written cut
approaches parity (dedup + lifecycle hygiene directly clean the corpus that
cut measures), the +5pp fusion advantage extends to tool-driven queries, and
the memory becomes something the agent can *maintain*, not just append to —
which is what keeps every other phase's ranking signals clean over time.

## Suggested measurement gates per phase

| phase | gate (no-regression + target) |
|---|---|
| 0 | drift eval runs in CI; today's numbers reproduced ±5pp |
| 1 | dual-written cut ≥ 88%; Project 1 ≥ 80%; code categories unchanged |
| 2 | repeat-tokens/prompt < 100; post-compaction full-catch ≥ 55% |
| 3 | zero DIVERGED-by-accident heads over a week of MCP writes; recall@5 parity MCP vs courier |
