# AGENTS.md - read this if you are the AI agent

Your user wants THOR running, and you are the one setting it up. This page is
the whole contract: first how to get your user set up, then how to work with
the memory once it runs. Everything else in this repo is depth; this page is
enough to do it right.

One rule above all: **assume your user has never done any of this before.**
Do every step you have the tools for yourself, narrate what you are doing in
plain language, and when a step needs the user (a download, a restart), give
them exactly one thing to do and verify it landed before moving on. Never
answer with a wall of options.

## Part 1: set your user up

1. **Get the binary.** Download the latest release asset for their platform
   (`thor-windows-x86_64.zip` or `thor-linux-x86_64.tar.gz`; the `-bm25`
   variant is for servers/NAS). Building instead? Then
   `cargo build --release --features semantic --bin thor` - and check the
   size: ~35 MB means semantic, ~10 MB means the feature flag was forgotten
   and dense recall is silently missing.
2. **First run: `thor doctor`.** It reports honestly what is present (store,
   model, sidecars, daemon). A missing model is not an error - THOR runs
   pure bm25 and degrades cleanly.
3. **Optional: the semantic model.** The user supplies a local ONNX
   sentence-embedding model (~235 MB, nothing auto-downloads). SETUP.md
   walks through it; skip this at first setup if in doubt - it can be added
   any time.
4. **Wire the hooks:** `thor install` (moment-of-action guards) or
   `thor install --with-courier` (plus automatic per-prompt recall - the
   channel that wins drift). It backs up settings and is idempotent.
5. **Register the MCP connector:** `claude mcp add thor -- <path-to>/thor.exe mcp`
   (then restart the harness once so it picks the connector up).
6. **Onboard the project:** run `thor init` in the project directory. It
   writes a `.thor` marker and ingests the tracked files.
7. **Prove it end to end:** `thor doctor` again, then recall something you
   know is in the store. Only now tell the user setup is done.

## Part 2: the working contract

THOR pushes most of what you need at the right moment: pinned rules at
session start, recall on every prompt, advisories when you touch a governed
file or command. What it cannot push is your discipline. Six rules:

1. **Recall before non-trivial work.** The store is the source of truth,
   not your assumptions and not a stale local file.
2. **Store decisions and gotchas the moment they land**, without being
   asked. Typed (`gotcha` | `decision` | `preference`), with `triggers` =
   the exact task words a future prompt would contain, `anchors` = the
   exact files/commands the fact governs (comma-separated - a space-joined
   list parses as ONE anchor that never fires), and honest `provenance`
   (`verified` only when a test ran, a file was read, or the user confirmed).
3. **Never store a near-duplicate: `revise` or `retract` the existing
   fact.** THOR refuses obvious duplicates, but the judgement is yours.
4. **Settle `[DIVERGED]` facts** with `resolve` as soon as you know which
   head is right - a contested fact serves both heads until you do.
5. **`mark` what helped, and what only distracted** (`noise: true`). This
   trains your own future recall; an unfed ranking prior learns nothing.
6. **`pin` the standing rules your user states** ("never X on prod") -
   pins are re-injected at every session start and survive compaction.

Pick the surface by the question's shape: `recall` for facts and prose,
`where_used` when a question names a symbol, `impact` before changing one,
`outline` for a file's shape, `get` to expand any id the others return.

And keep your user in the loop: say in plain language what the memory did
for them - what was recalled, what you stored and why. A memory that works
silently is a memory nobody trusts.

## Depth, when you need it

- [SETUP.md](SETUP.md) - the human-paced setup walkthrough
- [FEATURES.md](FEATURES.md) - what every part does, in plain words
- [OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md) - flags and trade-offs
- [BENCHMARKS.md](BENCHMARKS.md) - the measured claims, honest caveats included
