# THOR setup - end to end (for an AI or a human)

THOR is a local, lossless memory plus auto-recall for AI coding agents. This walks
through the full setup. Every command is idempotent and safe to re-run.

## 1. Get the binary

### Option A: download a release (no toolchain needed)

Grab the asset for your platform from [Releases](../../../releases), verify it
against the `.sha256` next to it, unpack, and put `thor` (or `thor.exe`) where it
will stay. The hooks do not expect a fixed location: `thor install` (step 2) writes
the binary's own path into the hook commands as it is at that moment, so give the
file its final home **before** you run `thor install`. Your PATH is the easy choice;
on Windows a tidy home next to the store is `%LOCALAPPDATA%\thor\thor.exe`, which the
examples below use.

If you do move the binary later, re-running `thor install` is not enough on its own.
It only skips a hook when the command line matches character for character, and there
is no uninstall command, so a re-run after a move leaves the old entries in place -
pointing at a file that is gone - and adds a second set next to them. Open your
agent's `settings.json`, delete the THOR lines that still carry the old path, then
run `thor install` again.

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
resident: measured, that is ~60% of per-prompt latency (349 -> 120 ms). It costs a
few hundred MB of RAM - the repo has never measured this process specifically, so
watch your own task manager rather than trusting a number here. Drop it if the RAM
matters more than the wait; the courier then falls back to the cold path and still
answers.

Careful with that RAM decision: THOR can run **two** resident processes and they are
unrelated. `--with-daemon` starts the injection daemon described above. The other one
is the embedding model process from step 3, which the courier starts by itself as soon
as a model and a vector sidecar exist - dropping `--with-daemon` does not avoid it. To
avoid that one, do not install a model.

This edits your agent's `settings.json` **idempotently** (it backs up first and only
adds THOR entries, never touching existing hooks), wiring:

- **UserPromptSubmit -> `thor courier`** - auto-recall on every prompt (injects a
  `<thor-recall>` block, scoped to the current project + global).
- **SessionStart -> `thor warm`** - pre-warm the semantic embedder so the first
  prompt is fast (no-op on a bm25-only build).
- **SessionStart -> `thor session-start`** - refresh a known project's index in the
  background, or emit a `<thor-setup>` cue so the agent offers to set up a new one.
- **PreCompact -> `thor pre-compact`** - one nudge per session, just before a
  compaction, to store durable decisions (from `--with-courier`).
- **PreToolUse -> `thor guard`** - moment-of-action advisory (from `--with-guard`).
- **SessionStart -> `thor ensure-daemon`** - start the injection daemon if it is not
  already up (from `--with-daemon`).
- **Stop -> `thor stop-guard`** - response guard (always added). Unlike the
  PreToolUse guard this one is not merely advisory: **when a rule fires** it holds
  the turn open so the agent reconsiders before finishing. When nothing fires, and
  whenever `THOR-SILENT.flag` is present, it stays out of the way.

Optional daily GitHub backup: add `--backup-repo <path-to-a-git-clone>`.

## 3. Semantic recall (do this on a client machine)

Lexical bm25 is always on; the dense score-fusion layer goes on top and is what
you want on the machine your agent runs on. Skip it only for a server/NAS (the
bm25 build carries no ONNX), or if you cannot spare ~650 MB of RAM for the warm
model daemon. It degrades to bm25 whenever anything is missing, so a broken setup
costs you the layer and nothing else. What it buys, and where it does not, is
measured in the README's "Semantic recall" section - briefly: it clearly helps on
hand-written memory facts and is a wash on indexed code.

The release binaries for Windows and Linux already have the feature compiled in.
What it needs from you is a model:

```sh
thor vectors build      # embed every stored fact once (needs the model in model/ inside THOR's home)
thor vectors status      # confirm model id + vector count
```

The model folder is `model/` inside THOR's per-user home - the same home the store
lives in: `%LOCALAPPDATA%\thor\model\` on Windows, `$XDG_DATA_HOME/thor/model/` or
`$HOME/.local/share/thor/model/` on Linux and macOS.

Any local ONNX sentence-embedding model + tokenizer works, as long as it produces
384 numbers per text (that width is fixed in THOR and a different one is refused);
a multilingual MiniLM is a good default. The per-prompt courier never pays the model
load: a warm `thor embed-daemon` holds it. `thor warm` starts that process, and so
does the courier on its own: once both the model and the vector sidecar exist, the
first prompt that finds the process down starts it in the background (that is the
~650 MB above) and answers from bm25 meanwhile. Recall degrades cleanly to bm25
whenever anything is missing.

## 4. Project scoping - the important part (this is the last upgrade)

THOR holds every project in ONE store but keeps them **isolated**, and holds
cross-cutting knowledge that surfaces **everywhere**.

- **The project = the session's working directory** - a `.thor` marker if present,
  else the git repo-root name. So: start each session in the project's own folder.
- **Set up a project:** `thor init` in its folder (writes `.thor` + indexes the
  tracked files - tracked-only, so gitignored secrets are never read - and builds
  the symbol sidecar that the `where_used` and `impact` tools read). A git project you
  have not set up triggers a `<thor-setup>` cue at SessionStart, so the agent offers
  to run this instead of indexing silently. That cue comes from the `thor session-start`
  hook, which only `--with-courier` installs (step 2).
- **Recall is scoped by default** to the current project + the global tier, across the
  courier, the CLI, and the stdio MCP server (`thor mcp`). A remote HTTP MCP server is
  started with no current project at all, and a search with no current project keeps
  the global tier and hides every project - so a remote call that passes nothing sees
  global facts only. From a remote client, pass `project: "<key>"` for one project or
  `all_projects: true` for everything. Same on the local side when you want to reach
  into another project: `all_projects: true` (MCP) / `thor recall --all-projects "..."`,
  or a specific `project: "<key>"` / `--project <key>`.
- **Cross-cutting docs available in every project:** `thor ingest --global <dir>` -
  those files go to the reserved `@global` tier and surface everywhere. **Keep them
  all in ONE folder.** Each folder is reconciled against the WHOLE tier, not against
  itself: anything already in `@global` that the folder being indexed does not
  contain is retracted. So pointing `--global` at a second folder withdraws the first
  folder's documents, and passing two folders in one command does the same thing (they
  are processed one after another, so the second undoes the first). One more reason not
  to rely on noticing it: the retraction step is skipped when the folder walk hit a
  read error, so the damage is not even consistent between runs.
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
thor fsck                          # chain, heads, FTS projection + index structure, footers
thor fsck --rebuild-fts            # only if fsck reports FTS INTEGRITY ERROR
thor recall "how does X work"      # scoped to the current project + global
thor recall --all-projects "X"     # search everything
```

`thor doctor` is the first thing to run after installing a binary and the first
thing to paste into a bug report: recall behaviour depends almost entirely on
whether the model, the vectors sidecar and the injection daemon are actually
present, and it reports all three, naming the folder it looked in for the model.

`thor fsck` prints six `OK` lines on a healthy store and **exits 1** if any
integrity check fails, so it is safe to put in a backup script or a scheduled
job and act on the result. If it reports `FTS INTEGRITY ERROR`, your search
index has been damaged (a bad disk, a torn write, a copy that was interrupted).
Your memory itself is fine: the index is rebuilt from the log, so run
`thor fsck --rebuild-fts` and then `thor fsck` again to confirm. A footer
complaint is different - it is content health, it never fails the run, and it
does not change the exit code.

One thing it does not report: the embedding-model process. Doctor's
`injection daemon:` line is about the daemon from `--with-daemon`, not the
embedder. So if semantic recall "does nothing" while doctor says the model and the
sidecar are present, the embedder is the piece to look at - check for a file named
`thor-embedd.json` next to your store, and run `thor warm` if it is missing.

Full command reference is in [README.md](../README.md); measured comparison + honest
weaknesses in [BENCHMARKS.md](BENCHMARKS.md). Everything this guide left out
because it is optional - the reranker, sync, the guard rulebooks, the kill switch,
the hygiene commands - is in [OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md), with
the reason to turn each one on and the reason not to.
