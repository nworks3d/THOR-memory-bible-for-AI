# Releasing THOR

The procedure. Follow it in order; every step exists because skipping it has
produced, or would produce, a broken claim.

## Versioning

`MAJOR.MINOR.PATCH`, source of truth = `thor/Cargo.toml`.

- **Below 1.0 = pre-launch.** THOR is here.
- **1.0 = launch.** Do not tag it until all of these are true:
  - someone other than the maintainer has installed and run it from a release
    asset (one machine is not a user base),
  - CI is green on a clean runner (not just "builds on my box"),
  - the benchmark claims in README/BENCHMARKS reflect the current code and the
    current rival version.

## What a release is built from

CI, from the tag, on clean runners. Never a binary from a maintainer's machine:
a local build proves it works with your toolchain, your cache and your ONNX
download - which is exactly what a release must not depend on.

Three assets, because THOR ships in shapes that are not interchangeable:

| asset | build | for |
|---|---|---|
| `thor-windows-x86_64.zip` | `--features semantic` | Windows client (the agent machine) |
| `thor-linux-x86_64.tar.gz` | `--features semantic` | Linux client |
| `thor-linux-x86_64-bm25.tar.gz` | default | servers / NAS / containers, no ONNX |

Each ships with a `.sha256`.

## The steps

1. **Land everything on main and let CI go green.** A tag on a red main is a
   broken release with a version number.

2. **Re-measure anything the release claims.** If recall, latency or drift moved,
   the published numbers are now wrong. Re-run the speed set and the evals, then
   update `docs/BENCHMARKS.md`, `README.md` and regenerate the chart:

   ```
   python thor/tools/gen_benchmark_chart.py
   ```

   The chart is generated from the measured numbers - never hand-edited. Its
   width guard only aborts the script run: on overflow it writes no file, and
   nothing in CI invokes the script, so a green CI run is not evidence that the
   chart renders. Read the script's own last line - it prints
   `wrote <path> (height N)` on success - and check that the run exited zero.
   Do not judge it by opening `assets/benchmark.svg`: an aborted run leaves the
   PREVIOUS chart in place, which looks perfectly fine. Then open it anyway, to
   see that the new numbers are the ones on the page.

   Numbers age in both directions. THOR is compute-bound and slows as the store
   grows; the rival ships too. Re-measure both sides, at their live size, on the
   same 20-prompt set, and publish the result even when it is worse than last
   time. The honest-numbers line is the point of this repo - see the
   code-structure entry in docs/BENCHMARKS.md for what it costs to keep.

3. **Bump the version** in `thor/Cargo.toml` (and `Cargo.lock` follows via any
   build). Commit it on its own: `release: v0.9.0`.

4. **Tag and push.**

   ```
   git tag v0.9.0
   git push origin v0.9.0
   ```

   The tag triggers `.github/workflows/release.yml`, which re-runs the tests,
   builds all three assets and opens a **draft** release with generated notes.

5. **Check the draft before publishing.** Download the asset for your platform
   and actually run it:

   ```
   thor doctor
   ```

   It must report the store, and correctly report whether model/sidecar/daemon
   are present. A release binary that cannot start is the one bug users cannot
   work around.

6. **Publish the draft.**

7. **Deploy locally, if you run THOR yourself.** The live binary is not updated
   by a release: see "Deploying to your own machine" below.

## Deploying to your own machine

The running MCP server and courier hold `thor.exe` open, so overwriting it
fails. Use the rename trick:

```
# Windows, from the repo
cargo build --release --features semantic --bin thor
mv "$LOCALAPPDATA/thor/thor.exe" "$LOCALAPPDATA/thor/thor.exe.pre-<what>-<date>.bak"
cp thor/target/release/thor.exe "$LOCALAPPDATA/thor/thor.exe"
```

Then verify:

```
thor doctor          # store OK, daemon state as expected
thor recall "<something you know is in there>"
```

Two traps, both hit in practice:

- **`cargo build --release` without `--features semantic` overwrites
  `target/release/thor.exe` with a bm25-only binary** (10 MB vs 35 MB). Deploy
  that and you silently lose dense recall. Always build the deploy binary with
  `--features semantic --bin thor`, and check the size.
- **Running processes keep the old binary** until they restart. The courier
  spawns fresh per prompt so it picks up the new one immediately; a long-lived
  daemon or MCP server does not.

## Windows runtime requirements

The semantic build links ONNX statically but still needs, from the OS:

- **Microsoft Visual C++ Redistributable** (`MSVCP140.dll`, `VCRUNTIME140.dll`)
- `DirectML.dll`, `d3d12.dll`, `dxgi.dll` - present on Windows 10 1903+

If the binary refuses to start on a clean machine, this is why. It is stated in
the release notes.

## What is deliberately NOT in a release

- **The embedding model** (~235 MB). Users supply their own local ONNX
  sentence-embedding model; nothing auto-downloads. Without it THOR runs pure
  bm25 and degrades cleanly. Say this in the notes, every time - it is the most
  likely source of "semantic mode does nothing" reports.
- **The private eval corpus.** The published category numbers are measured
  against a private store and are not reproducible from this repo. `drift_eval`
  is, and CI runs it.
