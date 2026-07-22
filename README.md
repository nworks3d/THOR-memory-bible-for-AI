![The AI Memory Bible](assets/banner.png)

# THOR - one lossless, local memory for AI coding agents

THOR is free and GPLv3. If it earns its keep, you can support the work:
**[donate via PayPal](https://www.paypal.com/paypalme/ognoizieworks)** or
**[become a YouTube member](https://www.youtube.com/@NoizieWorks/join)** -
no pressure, no paywall, everything stays open either way.

> **Are you an AI agent setting THOR up for your user?** Start with
> **[AGENTS.md](AGENTS.md)** - one page: how to walk your user through setup,
> and the six rules of working with the memory correctly.

[![Watch: THOR explained on YouTube](https://img.youtube.com/vi/tCQOUdkhErQ/maxresdefault.jpg)](https://youtu.be/tCQOUdkhErQ)

*Prefer watching? The video above walks through what THOR is and why it exists.*

## Why THOR exists

AI coding agents forget. Every new session starts blank, and even inside one
long session the context gets compressed ("compaction") and the details fall
out. You know the feeling if you have worked with an agent for more than a
day:

- You explain your project again. And again.
- You tell it "never do X on the production server" - and three sessions
  later it does X, because that rule lived in a conversation that no longer
  exists.
- It re-makes a mistake you already caught and corrected weeks ago.

THOR fixes that with one local memory that never loses anything:

- **It remembers your decisions, gotchas and working rules** - and hands them
  back to the agent *automatically, at the right moment*. Every prompt gets a
  memory check. The first time a session touches a file or command that one of
  your rules governs, that rule is pushed into the agent's context right then,
  without anyone asking for it.
- **It knows your code too.** It indexes your repositories and docs into the
  same memory, so "how does X work here" is answered from your own project,
  not from a guess.
- **It never loses a write.** Every fact is stored in an append-only,
  hash-chained log. An edit never destroys the old version; even a conflicting
  edit keeps both versions instead of silently overwriting one. There is a
  built-in integrity check (`thor fsck`).
- **Everything stays on your machine.** One Rust binary, one local database
  file. No cloud, no account, no subscription. And every optional layer fails
  soft: if a model or helper process is missing, THOR falls back to the simple
  path instead of breaking.

## Does it actually work? Measured, not promised

THOR is benchmarked head-to-head against
[mimir](https://github.com/MakerViking/mimir), blind-judged and pre-registered,
with retracted claims kept on the record:

![THOR vs mimir - coverage, quality, multi-project, drift and speed](assets/benchmark.svg?v=20260722)

The honest headline (2026-07-22 round, THOR v0.9.6 vs mimir v0.15.0): overall
retrieval quality is a **tie** (89.2% vs 84.6%, not significant). Where THOR
wins, significantly, is the thing it was built for: **drift** - after a fresh
session or compaction, THOR's automatic channel surfaces the governing rule at
79.7% against 64.4% for mimir's best explicit channel (11 wins to 0,
p = 0.001), and THOR's per-prompt hook answers every prompt (146 ms, never
empty) while mimir's own hook misses 50 of 59 drift scenarios. Full method,
per-category tables, significance tests, honest weaknesses and everything ever
retracted: **[docs/BENCHMARKS.md](docs/BENCHMARKS.md)**.

## Get started

**Step 1 - get the binary.** Download from [Releases](../../releases):
`windows-x86_64` or `linux-x86_64` (semantic client build; Windows needs the
Microsoft Visual C++ Redistributable), `linux-x86_64-bm25` for a server/NAS.
Each asset has a `.sha256`. Or build from source: `cd thor && cargo build
--release`.

**Step 2 - install the hooks** (backs up your settings first, only adds THOR
entries, idempotent):

```sh
thor install --with-courier --with-guard --with-daemon
```

**Step 3 - set up your project** (from the project's folder):

```sh
thor init
```

That is the whole loop: from then on the courier injects relevant memory on
every prompt, and your agent stores new facts over MCP as you work.

New to all this? Read these in order - they hold your hand the whole way:

1. **[docs/FEATURES.md](docs/FEATURES.md)** - what each part does, in plain
   words, and whether it is worth your time.
2. **[docs/SETUP.md](docs/SETUP.md)** - the full step-by-step walkthrough,
   assuming you have never done any of this before.
3. **[docs/OPTIONAL-FEATURES.md](docs/OPTIONAL-FEATURES.md)** - every optional
   piece: what it costs, how to turn it on, how to undo it.

## Documentation

| page | what it answers |
|---|---|
| [AGENTS.md](AGENTS.md) | you are an AI agent: how to set THOR up for your user and use it right |
| [docs/FEATURES.md](docs/FEATURES.md) | what does each part do, and should I care? (plain words, no commands) |
| [docs/SETUP.md](docs/SETUP.md) | step-by-step install and project setup, for someone who has never done it |
| [docs/OPTIONAL-FEATURES.md](docs/OPTIONAL-FEATURES.md) | every optional surface: default, real costs, on, verify, off |
| [docs/REFERENCE.md](docs/REFERENCE.md) | the depth: architecture, project scoping, semantic layer, sync, deploy, full command table |
| [docs/BENCHMARKS.md](docs/BENCHMARKS.md) | the measured head-to-head: method, numbers, significance, honest weaknesses |
| [docs/SIMILAR-PROJECTS.md](docs/SIMILAR-PROJECTS.md) | how THOR compares to other agent-memory projects |
| [docs/AI-BOOST-ROADMAP.md](docs/AI-BOOST-ROADMAP.md) | candidate improvements and what the measurements said |
| [thor/deploy/SYNC-DEPLOY.md](thor/deploy/SYNC-DEPLOY.md) | second machine / NAS: log-shipping sync and the remote MCP container |
| [CONTRIBUTING.md](CONTRIBUTING.md) | the correctness bar for changes, and how measuring THOR goes wrong |
| [docs/RELEASING.md](docs/RELEASING.md) | maintainer release procedure |

## Acknowledgments

- **MakerViking** - for the inspiration and the great fight. This project would
  not exist without the spark, and it would not be half as good without a worthy
  rival to measure against. Skål!
- **mimir** ([MakerViking/mimir](https://github.com/MakerViking/mimir)) - the
  wise opponent in every benchmark in this repo. In the sagas, Mimir guards the
  well of knowledge; here it set the bar THOR had to clear. The scoreboard
  shows real wins and real losses on purpose: a rival this good deserves honest
  numbers.
- **Idea credit, both directions.** Two THOR mechanisms are idea adoptions from
  mimir's own improvement rounds, reimplemented here in THOR's idiom: the
  identifier/path-aware matching in recall (mimir's identifier RRF leg) and the
  eval discipline that scores the injection DECISION as a confusion table with
  a one-way "injected-wrong must never rise" ratchet. Mimir in turn credits
  THOR for code-content indexing and per-prompt auto-recall - exactly the kind
  of exchange open source is for. Thanks, MakerViking.

## Support this project

THOR is built by [N-Works 3D](https://www.youtube.com/@NoizieWorks). If it has
earned its keep - saved you a re-explanation, caught a drift before it cost
you, or just kept a session from starting cold - there are two easy ways to
help keep it moving:

- **PayPal**: https://www.paypal.com/paypalme/ognoizieworks
- **YouTube members**: https://www.youtube.com/@NoizieWorks/join

No pressure and no paywall - the whole thing stays open either way. Skål, and
thanks for reading this far.

## Contributing

Bug reports and PRs welcome. THOR is a memory an agent is supposed to trust, so
the bar is correctness and honest measurement rather than volume of features -
the checklist before you commit, and the ways measuring THOR goes wrong, are in
**[CONTRIBUTING.md](CONTRIBUTING.md)**. Maintainer release procedure:
[docs/RELEASING.md](docs/RELEASING.md).

## License

GPLv3.
