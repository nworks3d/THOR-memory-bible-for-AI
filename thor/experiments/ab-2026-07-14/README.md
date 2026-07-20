# A/B candidate freezer - 2026-07-14

A snapshot of experiments that were built in a sandbox and never shipped, kept
here so the work survives and so the reasoning is auditable. **Nothing in this
folder is on the build path.** It is not compiled, not tested, and not wired
into anything - it is a freezer, not a feature.

It is kept for the same reason BENCHMARKS.md keeps its honest-weaknesses
section: what got rejected, and why, is part of the record.

## What is in this folder

- `sandbox-tracked.patch` - a `git diff` of the five tracked files the sandbox
  modified (`guard.rs`, `recall.rs`, `courier.rs`, `lib.rs`, `Cargo.toml`).
- `new/thor_src_experiments.rs` - a would-be `thor/src/experiments.rs` holding
  two candidate algorithms (a cluster cohesion gate, and an entropy-routed
  fuzzy name match for duplicate detection). Standard library only, no new
  dependency, with unit tests on synthetic edge cases.
- `new/thor_examples_resident_bench.rs` - a benchmark harness for the resident
  recall cache.
- `new/run-ab.sh` - the sandbox A/B runner. It points the data directory at an
  isolated copy so an experiment can never touch a real store.

## Where each candidate ended up

| candidate | outcome |
|---|---|
| recency ranking | **Shipped.** It is a non-flagged default in `recall.rs` on main. |
| resident recall cache | **Shipped.** Wired into the daemon; measured at roughly 60% off the per-prompt latency with byte-identical results. |
| stall-guard | **Rejected.** It looked good on hand-authored cases. Measured against a large set of real session endings it fired rarely, but the review of every one of those fires found roughly as many wrong as right - and the wrong ones would have overridden a legitimate hand-off to the user. Rejected as a blocking guard. |
| cohesion gate | **Never measured.** Proven only on one synthetic cluster. |
| entropy-routed dedup | **Never measured.** Proven only on synthetic name pairs. |
| secret scan | **Never measured beyond authored cases.** |

The lesson in the two shipped rows and the rejected one is the same: a candidate
that passes hand-written cases has proven nothing. Every one of these looked
convincing at that stage.

## If you want to revive one

The two unmeasured candidates are cleanly separable from main: copy
`new/thor_src_experiments.rs` to `thor/src/experiments.rs` and add
`mod experiments;` to `thor/src/lib.rs`. No new dependency.

For anything from `sandbox-tracked.patch`, apply single hunks rather than the
whole patch. In particular do **not** blanket-apply its `recall.rs` hunk: that
is an older, flagged version of code that has since shipped to main in a
different shape, so applying it would undo work rather than add it.

Before reviving anything, build the measurement first. That is what these
candidates lacked, and it is why three of them are still sitting here.

One editing note: two identifiers inside a test fixture in the patch were
replaced with obvious placeholders before this folder was published. They were
ids from a live memory store, and this repository deliberately carries none.
The test they belong to only cares about the *shape* of such an id, so the
placeholders serve it just as well.
