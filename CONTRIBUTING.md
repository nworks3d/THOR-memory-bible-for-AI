# Contributing to THOR

THOR is a memory an agent is supposed to trust. That makes correctness and
honest measurement the whole product - a fast wrong answer, a silently dropped
write, or a benchmark that flatters us is worse than no feature at all.

Everything below follows from that. It is short on purpose.

## The checklist before you commit

Run through this. If a line does not apply, say so in the PR rather than
skipping it silently.

- [ ] **Both feature sets build.** `cargo build --release` and
      `cargo build --release --features semantic`. The default (bm25, no ONNX)
      build is what servers and the NAS run - a change that only compiles with
      `semantic` is broken.
- [ ] **Tests pass**: `cargo test --release --features semantic --lib`. No test
      may need a model, a daemon, or your personal store: the suite has to pass
      on a clean machine.
- [ ] **New behaviour has a test, and the test can FAIL.** Temporarily revert
      your fix and watch the test go red. A test that passes both with and
      without your change proves nothing, and is worse than none - it is a green
      light with no bulb behind it.
- [ ] **No regression on what weighs heavy.** In order: drift-catch (this tool's
      reason to exist), correctness/losslessness, never losing a capture. A win
      elsewhere does not buy a loss here.
- [ ] **Claims come with numbers.** "Faster", "better recall", "fewer misses" -
      measure it, on the real store, and put the method in the PR. See
      "Measuring" below, because measuring THOR is easy to get wrong.
- [ ] **No secrets, no personal paths, no store contents** in code, tests,
      fixtures or commit messages. This repo is public.
- [ ] **Line endings unchanged.** Do not let your editor reformat a file you did
      not otherwise touch; a diff should be your change, not a whitespace sweep.
- [ ] **Plain typography**: a plain hyphen `-`, straight quotes, three dots for
      an ellipsis. No em dashes, en dashes or curly quotes anywhere - code,
      comments, docs or commit messages.
- [ ] **Commit message says what changed and why**, plainly. No marketing tone,
      no assistant/AI attribution trailers.

`rustfmt` and `clippy` are **not** gates. The tree is not clean under either,
and a repo-wide `cargo fmt` would churn every file for no behaviour change.
Match the style of the code around your diff instead.

## Measuring (read this before you post a number)

This project has repeatedly produced measurements that looked great and meant
nothing. The failure is always the same: **the thing you think you are measuring
never ran.** Two real examples from this repo's own history:

- `try_semantic_recall` silently falls back to bm25 when the embed daemon is not
  up. An A/B harness that forgets to start the daemon compares bm25 to bm25 and
  reports a proud "0% difference".
- A resident-cache benchmark where the cache was stale on every query measured
  the cold path plus overhead, and called it the warm path.

So, when you measure:

- **Prove the code under test actually ran.** Assert it - a daemon health check,
  a hit/rebuild counter, a canary. Refuse to report a number otherwise.
- **Prefer a mechanism that cannot be fooled.** If a change should not alter
  output, compare the output byte-for-byte instead of scoring it.
- **Beware metrics that only go up.** Coverage-style measures rise with more
  text served, so "more text" always wins on them. Report the cost too.
- **Measure on the real store**, not a synthetic fixture. Small authored corpora
  have told us the opposite of the truth more than once.
- **One run, no re-rolls.** Do not re-run a jury until you like the result.

The eval harnesses live in `thor/examples/`. `drift_eval` runs against a
committed synthetic corpus and needs nothing from you - it is the one claim in
this repo a stranger can reproduce, and CI runs it on every PR.

## Pull requests

- **One concern per PR.** A bugfix and a refactor in one diff cannot be reviewed
  or reverted independently.
- **Say what you did not do.** Skipped cases, known gaps, things you could not
  test in your environment - write them down. Silence reads as coverage.
- **Uncertain is fine, vague is not.** "n=33, single run, treat as a hint" is a
  useful sentence. "Improves recall" is not.
- CI must be green: build + tests on Linux and Windows, both feature sets, plus
  the drift corpus.

## Reporting a bug

Include what THOR served versus what you expected, plus `thor doctor` output
(store size, whether the model/sidecar/daemon are present). Recall behaviour
depends heavily on those three, and "recall is bad" without them is unactionable.

Never paste store contents, secrets, or private paths into an issue.

## Releases

Maintainer procedure: [RELEASING.md](RELEASING.md).
