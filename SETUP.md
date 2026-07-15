# THOR setup - end to end (for an AI or a human)

THOR is a local, lossless memory plus auto-recall for AI coding agents. This walks
through the full setup. Every command is idempotent and safe to re-run.

## 1. Get the binary

### Option A: download a release (no toolchain needed)

Grab the asset for your platform from [Releases](../../releases), verify it
against the `.sha256` next to it, unpack, and put `thor` (or `thor.exe`) on your
PATH - or straight where the hooks expect it (on Windows the per-user home is
`%LOCALAPPDATA%\thor\thor.exe`).

| asset | recall | for |
|---|---|---|
| `thor-windows-x86_64.zip` | semantic + bm25 | Windows client (the agent machine) |
| `thor-linux-x86_64.tar.gz` | semantic + bm25 | Linux client |
| `thor-linux-x86_64-bm25.tar.gz` | bm25 only | servers / NAS / containers - no ONNX |

Two things the binary does NOT include:

- **No embedding model.** Semantic recall needs a local ONNX model you supply
  (step 3). Without one THOR runs pure bm25 and degrades cleanly - it does not
  break.
- **Windows: the Microsoft Visual C++ Redistributable** must be installed
  (`MSVCP140.dll`, `VCRUNTIME140.dll`). If the binary refuses to start, that is
  why.

### Option B: build from source

```sh
cd thor
cargo test                                   # run the suite (should be all green)
cargo build --release --features semantic    # semantic recall; omit the feature for bm25-only
```

The binary is `thor/target/release/thor`. Same placement as above.

Note the feature flag: a plain `cargo build --release` produces a **bm25-only**
binary (~10 MB vs ~35 MB). That is the right build for a server or NAS, but
deploy it on your client machine and you silently lose dense recall. Check the
size if you are unsure.

### Replacing a binary that is already running

The MCP server and any daemon hold the file open, so overwriting fails. Rename
the old one out of the way first, then copy the new one in:

```sh
mv "$LOCALAPPDATA/thor/thor.exe" "$LOCALAPPDATA/thor/thor.exe.old"   # Windows
cp <new-binary> "$LOCALAPPDATA/thor/thor.exe"
```

The courier spawns fresh per prompt, so it picks up the new binary immediately.
A long-lived MCP server or daemon keeps the old one **until it restarts** - for
the MCP server that means restarting your agent.

## 2. Install the hooks (one command)

```sh
thor install --with-courier --with-guard --with-daemon
```

That is the full setup, and it is the one to run on the machine your agent works
on. `--with-daemon` keeps a warm process holding the folded log and vector matrix
resident: measured, that is ~60% of per-prompt latency (349 -> 120 ms) for about
650 MB of RAM. Drop it if the RAM matters more than the wait; the courier then
falls back to the cold path and still answers.

This edits your agent's `settings.json` **idempotently** (it backs up first and only
adds THOR entries, never touching existing hooks), wiring:

- **UserPromptSubmit -> `thor courier`** - auto-recall on every prompt (injects a
  `<thor-recall>` block, scoped to the current project + global).
- **SessionStart -> `thor warm`** - pre-warm the semantic embedder so the first
  prompt is fast (no-op on a bm25-only build).
- **SessionStart -> `thor session-start`** - refresh a known project's index in the
  background, or emit a `<thor-setup>` cue so the agent offers to set up a new one.
- **PreToolUse -> `thor guard`** - moment-of-action advisory (from `--with-guard`).
- **Stop -> `thor stop-guard`** - response advisory (always added).

Optional daily GitHub backup: add `--backup-repo <path-to-a-git-clone>`.

## 3. Semantic recall (do this on a client machine)

Lexical bm25 is always on; the dense score-fusion layer goes on top and is what
you want on the machine your agent runs on. Skip it only for a server/NAS (the
bm25 build carries no ONNX), or if you cannot spare ~650 MB of RAM for the warm
model daemon. It degrades to bm25 whenever anything is missing, so turning it on
cannot make recall worse.

The release binaries for Windows and Linux already have the feature compiled in.
What it needs from you is a model:

```sh
thor vectors build      # embed every stored fact once (needs the model under %LOCALAPPDATA%\thor\model\)
thor vectors status      # confirm model id + vector count
```

Any local ONNX sentence-embedding model + tokenizer works; a multilingual MiniLM is a
good default. The per-prompt courier never pays the model load: a warm `thor
embed-daemon` (started by `thor warm`) holds it, and recall degrades cleanly to bm25
whenever anything is missing.

## 4. Project scoping - the important part (this is the last upgrade)

THOR holds every project in ONE store but keeps them **isolated**, and holds
cross-cutting knowledge that surfaces **everywhere**.

- **The project = the session's working directory** - a `.thor` marker if present,
  else the git repo-root name. So: start each session in the project's own folder.
- **Set up a project:** `thor init` in its folder (writes `.thor` + indexes the
  tracked files - tracked-only, so gitignored secrets are never read). A project you
  have not set up triggers a `<thor-setup>` cue at SessionStart, so the agent offers
  to run this instead of indexing silently.
- **Recall is scoped by default** to the current project + the global tier, across the
  courier, the CLI, and the MCP server. To reach into another project on demand:
  `all_projects: true` (MCP) / `thor recall --all-projects "..."`, or a specific
  `project: "<key>"` / `--project <key>`.
- **Cross-cutting docs available in every project:** `thor ingest --global <dir>` -
  those files go to the reserved `@global` tier and surface everywhere.
- **Index a loose (non-git) folder:** `thor ingest <dir>` also works on a plain folder
  with no `.git` - it walks the text files directly (dotfiles, heavy dirs and any nested
  git repo skipped), the same reach as mimir's non-git doc collections. A nested git repo
  is left to its own `thor ingest` (its `.gitignore` still protects it); the only gap is a
  plaintext secret in a loose non-dot file directly in the folder, so point it at docs.
- **Pin a stable key:** when the folder name differs from how you open the project
  (e.g. a backup/source copy whose basename is not the project's key), use
  `thor ingest --project <key> <path>` (or drop a `.thor` marker in the folder). CAD /
  mesh / EDA asset dumps (STEP, STL, Gerber, ...) are always skipped.
- **Fix a mis-scoped fact:** `thor reproject <id> --project <key>` (or `--global`).
  Sync-safe: the reassignment is an appended event, so a replica agrees after sync.
- **Attribute legacy imported memories** from their mimir footer:
  `thor backfill-projects` (dry-run) then `--apply`.
- **Safety net for facts that landed global** (e.g. remembered in a remote / cwd-less
  session with no project signal): `thor review-scope` lists no-signal global memories
  added since the last review. The SessionStart hook nudges the agent (at most once a
  day) with a `<thor-scope-review>` cue to run it, propose reprojects for your
  confirmation, then `thor review-scope --mark`. Nothing moves without your OK.
- **Scoping never worsens recall:** in the right project you find the same facts as an
  unscoped search; you only lose the other projects' clutter.

Ordering note: an old binary cannot read a store containing the `fact_reprojected`
event, so upgrade every machine that shares a store (PC, sync replica, restore host)
to this build **before** the first `reproject`/`backfill`/`init`.

## 5. Deploy as a remote MCP server (only for web/mobile access)

`thor/deploy/` has a `Dockerfile` + `docker-compose.yml` template. Run `thor mcp
--http 0.0.0.0:<port>` in the container, bind it to localhost/an internal network, and
front it with an authenticating reverse proxy (the transport has no auth of its own).

## 6. Verify

```sh
thor doctor                        # store, model, sidecars, daemon, flags - start here
thor fsck                          # chain integrity + FTS/heads projection + footer health
thor recall "how does X work"      # scoped to the current project + global
thor recall --all-projects "X"     # search everything
```

`thor doctor` is the first thing to run after installing a binary and the first
thing to paste into a bug report: recall behaviour depends almost entirely on
whether the model, the vectors sidecar and the daemon are actually present, and
it reports all three. If semantic recall "does nothing", doctor tells you which
of them is missing.

Full command reference is in [README.md](README.md); measured comparison + honest
weaknesses in [BENCHMARKS.md](BENCHMARKS.md).
