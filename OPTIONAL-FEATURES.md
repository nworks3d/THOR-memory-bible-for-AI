# Optional features - what to turn on, and what to leave alone

THOR does one thing on its own: it stores facts in a local, append-only log and
gives them back when you search. That works with no configuration at all.

Everything on this page is **optional**. Each item makes THOR better at something
and costs you something else - memory, disk, a background process, an open port,
or just one more thing to understand. This page exists so you can decide, instead
of guessing.

Two promises hold throughout, and both are in the code rather than in this
sentence:

- **Nothing here can break recall.** Every optional layer degrades to plain
  keyword search when a piece is missing. A missing model, a deleted sidecar, a
  daemon that is not running - none of them produce an error, they produce
  ordinary results.
- **Nothing here deletes a fact.** The event log is append-only and
  hash-chained. Turning a feature off removes a derived file or stops a process;
  it never removes history. Where an exception exists, the block below says so
  explicitly.

## Start here: what should I actually turn on?

Find your situation. If two rows fit, do both.

| Your situation | Turn on | Skip |
|---|---|---|
| I code with an AI agent on my own machine | The hooks (`thor install --with-courier --with-guard --with-daemon`), semantic recall, and project scoping | Everything about syncing and servers |
| I want to try THOR before wiring anything in | Nothing. Use `thor remember` and `thor recall` by hand | All of it, until you like it |
| I run it on a server, a NAS or in a container | The bm25-only build | Semantic recall, the embedder, the warm daemon, the hooks |
| I have a laptop and a desktop | Sync: one machine is the authority, the other holds a replica | The capture inbox, unless a phone or web client writes to the replica |
| I want to reach my memory from a phone or the web | A remote server plus the capture inbox | Nothing else changes on your workstation |
| I have very little RAM | The hooks, but leave out `--with-daemon` and do not install an embedding model | Both resident processes. They are separate: the embedding model process is measured at about 650 MB, the injection daemon has never been measured |
| I am contributing to THOR itself | The evaluation harnesses | Nothing, but be aware two of them write to a live store |

If you only read one line: on the machine where your agent runs, install the
hooks and set up semantic recall. That is the shape THOR was built for. The rest
of this page is for the cases where the defaults do not fit you.

## How to read each entry

Every feature below is written the same way, so you can skim:

- **Default** - what happens if you do nothing.
- **Turn it on if** / **Leave it off if** - the actual situations, not a
  feature pitch.
- **What it costs** - the honest price: processes, memory, disk, ports,
  downloads, waiting time.
- **How to turn it on**, **How to check it worked**, **How to turn it off
  again**.

Where the repo has measured something, the number is quoted and you can find it
in the source. Where it has not, this page says so instead of implying a
benefit. If a claim here ever disagrees with what THOR actually does, the code
is right and this page is wrong - please open an issue.

## What is on this page

- [Which build to run](#which-build-to-run)  
  covers: cargo build --release --features semantic; thor-linux-x86_64-bm25.tar.gz; sha256sum -c &lt;asset&gt;.sha256; cargo build --release (build from source); cargo test; thor on PATH, or the per-user THOR home; --db &lt;path&gt;; XDG_DATA_HOME
- [Semantic recall](#semantic-recall)  
  covers: The embedding model files; thor vectors build | sync | status; thor warm (and the `thor embed-daemon` process it starts)
- [Cross-encoder rerank](#cross-encoder-rerank)  
  covers: thor recall --rerank (and MCP recall `rerank: true`)
- [The warm injection daemon](#the-warm-injection-daemon)  
  covers: thor daemon; thor ensure-daemon; thor install --with-daemon
- [Hooks: automatic recall, guards and nudges](#hooks-automatic-recall-guards-and-nudges)  
  covers: thor install; thor install --with-courier; thor session-start; thor pre-compact; thor install --with-daemon; thor install --with-guard; thor install --settings &lt;path&gt;; thor install --backup-repo &lt;path&gt;; Registering THOR as an MCP server (thor mcp); Running THOR with no hooks at all
- [Guard rulebooks](#guard-rulebooks)  
  covers: guard-rulebook.json; guard-response-rulebook.json; guard-capture-triggers.json; --rulebook &lt;path&gt; (on thor guard and thor stop-guard)
- [Projects and scoping](#projects-and-scoping)  
  covers: thor init (and the .thor marker file); thor ingest; thor ingest --global; thor ingest --project &lt;key&gt;; thor recall --all-projects and thor recall --project &lt;key&gt; (MCP: all_projects / project); the project argument on the MCP remember tool; thor reproject &lt;id&gt; --project &lt;key&gt; | --global; thor review-scope; thor backfill-projects
- [Keeping the memory healthy](#keeping-the-memory-healthy)  
  covers: thor doctor; thor fsck; thor fsck --rebuild-fts; thor symbols; thor consolidate; thor consolidate --apply-dedup; thor consolidate --min-age-events &lt;N&gt;; thor steward; thor pin &lt;id&gt; / thor unpin &lt;id&gt;; thor mark &lt;id&gt; (and --noise); expires: YYYY-MM-DD (on the MCP remember tool); provenance: verified | inferred (and THOR_EXP_PROVENANCE)
- [Backup, restore and import](#backup-restore-and-import)  
  covers: thor export; thor restore --from &lt;file&gt;; thor backup --repo &lt;path&gt; [--force]; thor import &lt;path&gt;
- [Syncing two machines](#syncing-two-machines)  
  covers: THOR_TOKEN; thor recv --http &lt;bind&gt;; thor ship --to &lt;url&gt;; thor status --to &lt;url&gt;; THOR_CAPTURE_INBOX; thor drain-inbox --inbox &lt;file&gt; | --from &lt;url&gt;
- [Running THOR as a remote server](#running-thor-as-a-remote-server)  
  covers: thor mcp --http &lt;bind&gt;; The Docker deployment (deploy/Dockerfile and deploy/docker-compose.yml); THOR_RECV_BIND (container as the replica); THOR_REPLICA_URL (container as the authority); deploy/deploy-watcher.sh
- [Switching things off](#switching-things-off)  
  covers: THOR-SILENT.flag; THOR-PRIMARY.flag
- [Tools for contributors only](#tools-for-contributors-only)  
  covers: cargo run --release --example drift_eval; thor/eval/drift_scenarios.jsonl; cargo run --release --example drift_eval -- --live `<corpus>`; cargo run --release --example hits_dump -- --queries `<in.json>` --out `<out.json>`; cargo run --release --features semantic --example recall_eval; cargo run --release --features semantic --example cache_correctness; cargo run --release --features semantic --example cache_speed; cargo run --release --features semantic --example warm_ab; python thor/tools/gen_benchmark_chart.py; python thor/tools/export_mimir.py; pwsh thor/tools/run_sidebyside.ps1

## Which build to run

THOR ships in more than one shape, and the shapes are not interchangeable. This section
helps you pick one: which file to download (or whether to compile it yourself), whether
to include the meaning-based recall layer, where to put the resulting program, and where
it should keep its data. Get this right once and the rest of the setup follows.

### cargo build --release --features semantic

`--features semantic` is a compile-time switch. It adds a second way of searching your
memory: on top of plain keyword matching, THOR can also match on meaning, so a question
phrased in words that appear nowhere in the stored fact can still find it. It does this
by turning text into lists of numbers with a small local machine-learning model, then
comparing those. The model runs entirely on your machine; nothing is sent anywhere.

- **Default:** off for a build you compile yourself. A plain `cargo build --release`
  produces a keyword-only ("bm25-only") program. The prebuilt `thor-windows-x86_64.zip`
  and `thor-linux-x86_64.tar.gz` release assets are already built with the feature on;
  `thor-linux-x86_64-bm25.tar.gz` is not.
- **Turn it on if:** this is the machine your coding agent actually works on. That is
  where recall happens on every prompt, and it is the shape the repo recommends there.
- **Leave it off if:** the machine is a server, a container or a NAS that only stores
  and serves memory; or you cannot spare the RAM described below; or you do not want to
  fetch an embedding model.
- **What it costs:**
  - Compiling with the feature downloads the ONNX runtime binaries during the build.
    The repo states no size for that download.
  - The program itself grows from about 10 MB to about 35 MB (SETUP.md and RELEASING.md
    both state 10 MB vs 35 MB).
  - You supply the model files yourself; nothing is downloaded automatically. README
    puts a typical model at about 235 MB on disk.
  - RAM: once a model and the vector sidecar both exist, the per-prompt recall path
    starts a resident background process that keeps the model in memory. This is not
    something you opt into separately - the recall path starts it on its own
    (`thor/src/courier.rs:929`). README states about 650 MB for that process.
  - On Windows the semantic build needs the Microsoft Visual C++ Redistributable
    (`MSVCP140.dll`, `VCRUNTIME140.dll`) present on the system, or it refuses to start.
- **How to turn it on:**

  ```sh
  cd thor
  cargo build --release --features semantic
  ```

  Then supply the model. It goes in a `model` folder inside THOR's per-user home -
  the same folder your store lives in, so both travel together:

  ```
  %LOCALAPPDATA%\thor\model\        # Windows
  $XDG_DATA_HOME/thor/model/        # Linux and macOS, when XDG_DATA_HOME is set
  $HOME/.local/share/thor/model/    # Linux and macOS otherwise
  ```

  **This changed:** older builds only understood `%LOCALAPPDATA%` and, when that was
  unset, fell back to a folder called `thor-model` relative to whatever directory the
  process happened to start in. On Linux and macOS that meant the answer moved with
  your shell, and `thor doctor` could report the model present while recall looked
  somewhere else and quietly stayed on keyword search. If you put a model in a
  `thor-model` folder before, move it to the location above.

  `thor vectors build` accepts `--model-dir <dir>`, but that override applies to that
  one command only - recall and the resident model process always read the folder
  above. The command now says so when you pass it.
- **How to check it worked:** run `thor doctor`. It prints one health line per surface
  (store, semantic, symbols sidecar, injection daemon); the one to read here is the
  semantic line. A keyword-only build prints exactly one:

  ```
  semantic: not built in (bm25-only binary)
  ```

  A semantic build prints two lines instead, one for the model and one for
  `vectors sidecar:`. Both must say `present` before meaning-based recall actually
  runs. The model line names the folder it looked in, so you can check it against
  where you put the files:

  ```
  semantic model: present (C:\Users\<you>\AppData\Local\thor\model)
  semantic model: absent (bm25-only recall; expected the 5 model files in <folder>)
  ```

  Doctor asks exactly the question recall asks - same folder, and the same
  all-five-files test - so the two can no longer disagree with each other.
- **How to turn it off again:** rebuild without the flag, or put the `-bm25` asset in
  place. Nothing is lost: your memory is an append-only log, and everything the semantic
  layer produces lives in separate derived files you can delete and rebuild.

### thor-linux-x86_64-bm25.tar.gz

The keyword-only prebuilt program for Linux. It is the download equivalent of leaving
`--features semantic` off: the machine-learning runtime is not merely disabled, it is
not compiled in at all.

- **Default:** none - you choose which asset to download. The plain
  `thor-linux-x86_64.tar.gz` is the semantic one.
- **Turn it on if:** the machine stores or serves memory but never runs the per-prompt
  recall on your behalf. The repo names exactly these: servers, containers, the NAS,
  a remote memory endpoint.
- **Leave it off if:** this is the machine your agent works on. The capability is
  removed at compile time, so no setting can bring it back. The failure is silent -
  recall keeps working, just on keywords only. The repo flags this as a trap that has
  been hit in practice.
- **What it costs:** nothing extra to run. It is smaller (about 10 MB against about
  35 MB) and pulls in no machine-learning runtime. The cost is the missing capability,
  not resources.
- **How to turn it on:**

  ```sh
  tar xzf thor-linux-x86_64-bm25.tar.gz
  ```

  Then place the `thor` program (see the placement entry below).
- **How to check it worked:** run `thor doctor`. Among its health lines, the semantic one
  reads `semantic: not built in (bm25-only binary)`.
- **How to turn it off again:** replace the program with the semantic asset for your
  platform. Nothing is lost; the store file is untouched. One caveat: long-running
  processes hold the file open, so rename the old one out of the way before copying the
  new one in, and restart your agent so its memory server picks up the new file.

### sha256sum -c &lt;asset&gt;.sha256

Every published release file has a small companion file next to it ending in `.sha256`.
It contains one fingerprint of the download. Re-computing that fingerprint yourself and
comparing tells you the file arrived intact and was not swapped.

- **Default:** not done for you. Nothing checks it automatically.
- **Turn it on if:** you downloaded a release asset. This is a program your agent's
  hooks will launch, so a corrupted or swapped file matters. It is one extra command, and
  it is worth running especially over a flaky link or through a company proxy that
  inspects traffic.
- **Leave it off if:** you built from source - there is no published asset and no
  `.sha256`, so the step does not apply. Also be clear about what it does not do: it
  proves the file matches what the release page says, not who produced it. There is no
  signing anywhere in the release pipeline.
- **What it costs:** one manual command and one extra small file to download. No
  configuration, no running process, nothing persistent.
- **How to turn it on:** download the asset and its `.sha256` into the same folder, then:

  ```sh
  sha256sum -c thor-linux-x86_64.tar.gz.sha256
  ```

  On Windows PowerShell, compare by hand:

  ```powershell
  Get-FileHash .\thor-windows-x86_64.zip -Algorithm SHA256
  Get-Content .\thor-windows-x86_64.zip.sha256
  ```
- **How to check it worked:** `sha256sum -c` prints `thor-linux-x86_64.tar.gz: OK` and
  exits with status 0. A mismatch prints `FAILED` and a non-zero status - do not use
  that download. On Windows, the 64-character hash from `Get-FileHash` must match the
  64-character value at the start of the `.sha256` file.
- **How to turn it off again:** just do not run it. There is nothing to undo, and you
  can delete the `.sha256` file afterwards.

### cargo build --release (build from source)

Compile the program yourself from the `thor/` folder in this repository, instead of
downloading a prebuilt one. The result lands at `thor/target/release/thor` and is placed
exactly like a downloaded one.

- **Default:** none - it is the alternative to downloading a release asset (SETUP.md
  calls it Option B).
- **Turn it on if:** no published asset fits. The release matrix has exactly three
  entries: Windows x86_64 semantic, Linux x86_64 semantic, Linux x86_64 keyword-only. If
  you are on another platform, or want a shape that is not one of those three (for
  example keyword-only on Windows), or want to run a commit that has not been tagged,
  you build it yourself.
- **Leave it off if:** you are on Windows or Linux x86_64 and one of the three assets
  fits. RELEASING.md is explicit that release assets are built by CI from the tag on
  clean machines, precisely so a release does not depend on one person's toolchain,
  cache and download.
- **What it costs:** you need a working Rust toolchain, which the download route does
  not require. The build downloads the whole dependency tree, and with
  `--features semantic` it also downloads the ONNX runtime binaries. The repo states no
  timing for a build.
- **How to turn it on:**

  ```sh
  cd thor
  cargo build --release --features semantic    # omit the feature for a keyword-only build
  ```
- **How to check it worked:** run `thor doctor` with the program you just built and read
  the semantic lines described above. If you meant to build the semantic shape, check the
  file size too: about 35 MB, not about 10 MB.
- **How to turn it off again:** drop a release asset in place of your own build. Nothing
  is lost - the store does not record how the program was produced. Rename the old file
  out of the way first if anything long-running is holding it open.

### cargo test

Runs THOR's own test suite against the source tree before you build. It only applies if
you are building from source.

- **Default:** not run for you. `cargo build` does not run tests.
- **Turn it on if:** you are building from source and any of these hold - you changed or
  forked the code, you are on a platform the project's CI does not cover, or a build
  behaved oddly.
- **Leave it off if:** you downloaded a release asset (there is no source tree to test),
  or you are building an unmodified released tag where CI already ran the same suite.
  Skipping it is a real saving: it compiles the crate a second time in the test profile
  on top of an already large dependency tree.
- **What it costs:** a second full compile plus the test run. The repo states no wall
  clock time for it.
- **How to turn it on:**

  ```sh
  cd thor
  cargo test
  ```
- **How to check it worked:** cargo prints a line like `test result: ok. N passed;
  0 failed` per test binary and exits 0. SETUP.md describes the expected outcome as
  "should be all green". The project's CI workflow records one measurement:
  243 of 243 tests pass with no model present.
- **How to turn it off again:** just do not run it. It is a one-shot check with no
  persistent state, and it works in throwaway temporary folders, so it never touches
  your real memory store.

### thor on PATH, or the per-user THOR home

Where you put the program file. PATH is the list of folders your shell searches when you
type a bare command name, so putting `thor` in one of them makes every command in this
guide run as written, from any folder.

- **Default:** wherever you unpacked or built it, which is usually not on PATH. Until
  you move it, you must type its full path every time.
- **Turn it on if:** you or your agent type THOR commands in a terminal. Also do it
  before you run `thor install`, because the hook installer writes the program's current
  absolute path into your agent's settings (`thor/src/install.rs:33`). Give the file its
  final home first, and the hooks point at the right place. On Windows the documented
  home is `%LOCALAPPDATA%\thor\thor.exe`.
- **Leave it off if:** you only ever drive THOR through the hooks and the memory server.
  Both are wired with an absolute path, so PATH buys them nothing. In the container
  deployment it is already handled: the image copies the program to `/usr/local/bin/thor`.
- **What it costs:** nothing at runtime. It is a file location and a PATH entry.
- **How to turn it on:** Windows PowerShell, one line:

  ```powershell
  New-Item -ItemType Directory -Force "$env:LOCALAPPDATA\thor" | Out-Null; Copy-Item .\thor.exe "$env:LOCALAPPDATA\thor\thor.exe" -Force
  ```

  Linux or macOS, one line:

  ```sh
  install -m 755 ./thor "$HOME/.local/bin/thor"
  ```

  Then make sure that folder is on your PATH, and open a new terminal so the change
  takes effect.
- **How to check it worked:** open a new terminal, change into any folder other than the
  one holding the file, then run `where thor` on Windows or `command -v thor` on Linux
  and macOS. It should print the path you chose. Follow it with `thor doctor`.
- **How to turn it off again:** move the file out of the PATH folder, or remove the
  folder from PATH. Nothing automated breaks - the hooks, the memory-server registration
  and the store all use absolute paths. Only your own typed commands stop working. If
  you move the file after running `thor install`, re-run `thor install` so the hooks
  point at the new location.

### --db &lt;path&gt;

Tells one invocation to open a different memory file than your normal one. It is a
global flag, so it goes before the subcommand, not after it.

- **Default:** unset. Every subcommand opens the one per-user store, so the command
  line, the hooks and the memory server all agree without any flag.
- **Turn it on if:** you deliberately want this one command to act on a different store:
  running THOR in a container (the shipped container image passes `--db /data/thor.db`),
  running a side-by-side benchmark, or inspecting a store you restored from a backup.
- **Leave it off if:** anything resembling normal use. A second store is a second silo.
  A fact you store under `--db other.db` is invisible to recall, to the memory server
  and to the installed hooks, because none of them pass the flag. Splitting your memory
  by accident is easy and unpleasant to undo.
- **What it costs:** nothing at runtime. It changes one path. The cost is entirely the
  risk of writing to the wrong store.
- **How to turn it on:**

  ```sh
  thor --db /path/to/scratch/thor.db recall "query"
  thor --db /path/to/scratch/thor.db doctor
  ```
- **How to check it worked:** the first line of `thor doctor` echoes the file it opened:

  ```
  store: OK (1483 events at /path/to/scratch/thor.db)
  ```

  Read the path, and read the event count. If the path does not exist, doctor says so
  rather than inventing a store:

  ```
  store: UNREACHABLE (no THOR store at /path/to/scratch/thor.db - this command never
  creates one; check the path (--db) or store your first memory to create it)
  ```

  That is the safety net for a typo. It only covers the reporting commands, though:
  a `--db` typo on a command that WRITES still creates a new store at that path,
  because writing is what those commands are for.
- **How to turn it off again:** omit the flag. Nothing is persisted, so the next command
  goes straight back to your normal store, which was never touched.

### XDG_DATA_HOME

An environment variable that moves THOR's whole per-user folder. That folder holds the
store file, the flag files and the derived sidecar files. THOR checks three things in
order: `LOCALAPPDATA`, then `XDG_DATA_HOME`, then `$HOME/.local/share`, and appends
`thor` to whichever one it finds.

- **Default:** normally unset, and it is consulted second - only when `LOCALAPPDATA` is
  not set. The chain is plain environment-variable order and is not gated on the
  operating system (`thor/src/ledger.rs:36-45`), so on Windows, where `LOCALAPPDATA` is
  always set, this variable never gets a turn.
- **Turn it on if:** you are on Linux or macOS and want the store somewhere other than
  `$HOME/.local/share/thor` - an encrypted volume or a bigger disk - and you want every
  subcommand, hook and background process to agree on it without passing `--db` each
  time.
- **Leave it off if:** you are on Windows (`LOCALAPPDATA` wins there), or the default
  location is fine. The honest risk: this is a property of a process's environment. Any
  hook, scheduled task or service that does not inherit it will resolve a different
  store and quietly find it empty.
- **What it costs:** nothing measurable at runtime. It is two environment-variable reads
  during path resolution. No process is started, no port opened, nothing downloaded.
  One sharp edge: never set it to an empty string. An empty value still counts as set,
  and the folder then resolves to a bare relative `thor` next to whatever folder the
  process happens to be in.
- **How to turn it on:**

  ```sh
  export XDG_DATA_HOME=/path/to/data
  ```

  The store then resolves to `/path/to/data/thor/thor.db`. Set it in the environment of
  anything that launches THOR hooks too, not just your interactive shell. Moving the
  variable does not move an existing store: copy `thor.db` and the other files from the
  old folder into the new one yourself.
- **How to check it worked:** run `thor doctor`. The first line echoes the resolved path
  and the number of stored events:

  ```
  store: OK (1234 events at /path/to/data/thor/thor.db)
  ```

  Read the event count, not just the word OK. Opening a path that does not exist yet
  creates an empty file rather than failing, so a wrong path also prints `store: OK`,
  with `0 events`. A count of 0 on a store you know is not empty means you are pointed
  at the wrong file.
- **How to turn it off again:** unset it, and the folder resolves back to
  `$HOME/.local/share/thor`. The variable itself loses nothing, but your data does not
  travel with it: move the store file back, or point `--db` at it, or THOR will open a
  different, empty store.

## Semantic recall

Out of the box, THOR finds a fact by matching the words in your question against
the words in the fact. Semantic recall adds a second way of finding things: it
turns text into a list of numbers ("a vector") that stands for its meaning, so a
question can reach a fact it shares no words with. The two are added together,
never swapped: keyword search always runs, and the semantic layer only ever adds
candidates on top. If any piece is missing, recall quietly falls back to keyword
search alone.

This section helps you decide whether to set that up, and it is three steps that
only make sense together: supply a model, build the vector file, and keep the
model loaded in memory. Doing one without the next buys you nothing.

One prerequisite for all three: your `thor` binary must have been built with the
`semantic` feature. The published Windows and Linux client downloads already are.
The `bm25` download for servers and containers is not, and neither is a plain
`cargo build --release` from source, which needs `--features semantic` instead. On
a binary without it, `thor vectors` and `thor embed-daemon` print an explanatory
line and do nothing, and `thor warm` is a silent no-op. Check with:

```sh
thor doctor
```

A binary without the feature prints exactly one line about it:

```text
semantic: not built in (bm25-only binary)
```

A binary with the feature prints two lines instead, `semantic model: ...` and
`vectors sidecar: ...`.

### The embedding model files

An embedding model is the file that converts text into those meaning-vectors.
THOR does not ship one and never downloads one: you place five files in a folder
yourself, and until you do, the semantic layer stays off.

- **Default:** absent. Nothing in the installer, the hooks or any command fetches
  a model. With no model present, recall is keyword-only and no message is
  printed about it during normal use.
- **Turn it on if:** this is the machine your coding agent actually runs on, and
  you want a question asked in your own words to reach a fact written in
  different words. This is the setup the README recommends for a client machine.
- **Leave it off if:** the machine is a server, a NAS, a container or a remote
  replica. Those do not run the per-prompt recall hook, so nothing there would
  use the model. Also leave it off if you do not want to fetch the download, or
  if you cannot spare the memory: placing the model is what unlocks the resident
  embedder process further down, and that is where the real memory cost sits.
- **What it costs:**
  - A download and disk space. The README states the model at about 235 MB. You
    source it yourself.
  - By itself, no process and no port. But note the knock-on: once both the model
    files and the vector file exist, the per-prompt recall hook starts the
    resident embedder process on its own, without you enabling anything. See the
    `thor warm` entry below for what that process costs.
- **How to turn it on:** put these five files, with exactly these names, in
  THOR's model folder:

  ```text
  model_optimized.onnx
  tokenizer.json
  config.json
  special_tokens_map.json
  tokenizer_config.json
  ```

  Any local ONNX sentence-embedding model with its tokenizer works, on one
  condition: it must produce 384 numbers per text. That width is fixed in THOR's
  code, and a vector of any other size is refused with an error rather than
  stored, so a 768-dimension model will not work. THOR also mean-pools the
  model's output. A multilingual MiniLM is a reasonable default, and it is what
  the code names as the model it was tuned on. If your download calls the ONNX
  file something else, rename it to `model_optimized.onnx`.

  The folder is a `model` folder inside THOR's per-user home, the same home the
  store lives in:

  ```text
  %LOCALAPPDATA%\thor\model\        # Windows
  $XDG_DATA_HOME/thor/model/        # Linux and macOS, when XDG_DATA_HOME is set
  $HOME/.local/share/thor/model/    # Linux and macOS otherwise
  ```

  **This changed:** older builds resolved this from `LOCALAPPDATA` alone and, when
  that was unset, fell back to a folder named `thor-model` relative to the directory
  you happened to run `thor` from. On Linux and macOS the model was therefore only
  found from one specific working directory. It now follows the same home as the
  store on every platform. Windows is unaffected. If you have a `thor-model` folder
  from before, move its contents to the location above.

  A one-off command can still be pointed elsewhere with `--model-dir`, but the
  per-prompt hook and the resident embedder always read the default folder, so a
  sidecar built from a model that only exists somewhere else is one recall can
  never load. `thor vectors build` prints a note reminding you of that whenever you
  pass the flag.

- **How to check it worked:** the model alone produces no output. The real check
  is the next step, `thor vectors build`: if a file is missing it fails loudly and
  names every file it wanted.

  `thor doctor` prints a line about this too. It looks in the same folder recall
  reads and checks that all five files are there, so an empty folder reads as
  `absent`, and the line names the folder it looked in. It still does not know
  about a folder you pointed at with `--model-dir`:

  ```text
  semantic model: present (<folder>)
  ```

- **How to turn it off again:** delete the model folder. Recall silently returns
  to keyword-only, and the resident embedder stops being started. Nothing is
  lost: no fact is stored in the model or derived from it that is not
  recomputable, and the append-only event log is untouched.

### thor vectors build | sync | status

This computes one meaning-vector per stored event and writes them to a separate
file, `thor-vectors.db`, next to your store. `build` does all of them from
scratch, `sync` does only the ones added since last time, `status` reports what is
in the file.

- **Default:** the file does not exist until something builds it, and on a
  keyword-only binary the command just prints a note and exits. Be aware of one
  automatic path though: on a semantic binary, `thor ingest` runs the `sync` step
  itself after any indexing run that created, revised or retracted something. So
  if you place a model and then index a repository, the vector file can appear
  without you ever typing `thor vectors build`.
- **Turn it on if:** you placed the model files and want the semantic layer to
  actually do something. Without this file the model is never consulted.
- **Leave it off if:** you did not place a model, or you are on a server, NAS or
  container.
- **What it costs:**
  - Time, once, up front: it runs every stored event through the model in batches
    of 256, printing progress as it goes. How long that takes depends on your
    store size and your CPU, and the repo states no measurement for it.
  - Disk. The repo states no size for the file. From the code you can work it out:
    384 numbers of 4 bytes each per event, so roughly 1.5 KB per stored event plus
    the usual database overhead. That is arithmetic from the source, not a
    measurement anyone ran.
  - The knock-on cost again: with the model present, the existence of this file is
    the second and last condition for the per-prompt hook to start the resident
    embedder process on its own.
  - What it buys is not quantified. The repo contains no measurement comparing
    recall with the semantic layer against recall without it. What the code does
    guarantee is the failure contract: an absent or empty vector file means no
    semantic candidates, never an error.
- **How to turn it on:**

  ```sh
  thor vectors build
  thor vectors status
  ```

  If your model files are somewhere else, point at them for these two commands:

  ```sh
  thor vectors build --model-dir <your-model-folder>
  ```

  Afterwards, `thor vectors sync` is the maintenance form: it embeds only events
  added since the last run, which is much faster than a full rebuild. Use
  `thor vectors build --force` if you want a full rebuild anyway.

  If you ever swap the model files for a different model, you have to rebuild
  yourself: run `thor vectors build --force`. THOR will not notice the swap. The
  model name it records in the file is a fixed string compiled into THOR, not a
  fingerprint of the files you placed, so putting different files in the folder
  leaves that name unchanged and `sync` keeps appending to vectors made by the
  old model. The recorded name only changes when you upgrade to a THOR build that
  uses a different one. That case THOR does handle: the next `thor vectors sync`
  sees the mismatch, rebuilds from scratch and prints a note saying so. Until you
  run it, recall drops back to keyword search, because a sidecar whose name does
  not match is treated as stale.

- **How to check it worked:** `thor vectors status` prints five lines:

  ```text
  vectors sidecar : <path to thor-vectors.db>
    model_id      : paraphrase-multilingual-MiniLM-L12-v2-onnx-Q@mean-v1
    expected      : paraphrase-multilingual-MiniLM-L12-v2-onnx-Q@mean-v1
    stored vectors: 1234
    tip seq       : 1234
  ```

  Two things must hold: `model_id` and `expected` must be the same string, and
  `stored vectors` must not be zero. Anything else means the semantic layer is not
  contributing.

  Do not use `thor doctor` for this. Its line only checks whether the file exists,
  and the file gets created before the model check runs, so a build that failed for
  a missing model still leaves an empty `thor-vectors.db` that doctor happily calls
  present:

  ```text
  vectors sidecar: present
  ```

- **How to turn it off again:** delete `thor-vectors.db` next to your store.
  Recall silently returns to keyword-only. This is lossless by design: the file is
  derived, never authoritative, and your facts live in the append-only log which
  this never touches.

  One catch: deleting it is not sticky. As long as the model files are still in
  place, the next `thor ingest` that changes anything rebuilds it via the automatic
  `sync`. To make it stay gone, remove the model files too.

### thor warm (and the `thor embed-daemon` process it starts)

Turning your question into a meaning-vector needs the model in memory, and loading
it takes long enough that it must not happen while you are waiting on a prompt. So
the model lives in one long-running background process that loads it once and
answers small requests over a local network port. `thor warm` is the command that
starts that process if it is not already running. `thor embed-daemon` is the
process itself.

These are one feature, not two: `thor warm` checks whether the process answers and,
if not, starts it in the background and returns. Running `thor embed-daemon`
directly starts the same server in your terminal and does not return until it exits,
so it is not what you want in a hook or a script.

- **Default:** nothing is running on a fresh install, and no command you have run so
  far starts it. But on the recommended client setup you do not opt in by typing
  anything, on two paths. First, `thor install --with-courier` wires `thor warm`
  into your agent's SessionStart hook automatically. Second, the per-prompt recall
  hook starts the process by itself, detached, the first time it wants a
  meaning-vector and nothing answers, provided the model files and the vector file
  are both present. That prompt falls back to keyword search; the next one is warm.
- **Turn it on if:** you have the model and the vector file, and you want the
  semantic layer used from the first prompt of a session instead of the second.
- **Leave it off if:** memory is tight. Nothing breaks without it: keyword search is
  the floor and the code is explicit that nothing on this path can block or slow a
  prompt.
- **What it costs:**
  - Memory: the repo states about 650 MB for the resident model, both in the README
    and in the source. If you also run the injection daemon described elsewhere in
    this guide, note these are two separate processes. The 650 MB above is this one,
  the embedder; the injection daemon's own footprint has never been measured.
  - A second long-lived process. It is started detached, with its input and output
    disconnected, so it outlives the session that started it. It stops by itself
    after 20 minutes with no request, and removes its port file on the way out.
  - One TCP port on `127.0.0.1` only. The port number is chosen by the operating
    system, not fixed, and is published in a small file next to your store so the
    hook can find it. Nothing listens on an external interface.
  - A small worst case on a single prompt: if the port file points at a process that
    died, the hook waits for its connect budget of 400 ms and, if that connects to a
    recycled port, up to another 500 ms for a reply, then gives up, deletes the stale
    port file and answers with keyword search. That same prompt starts a fresh
    process in the background, so the next one is warm again.
  - What it buys is stated in the source but not benchmarked: loading the model cold
    is put at about 1.25 seconds and a warm request at about 10 ms. Those two figures
    appear only as comments in the code. No benchmark in the repo backs them.
- **How to turn it on:**

  ```sh
  thor warm
  ```

  That is safe to run at any time and safe to run twice: if the process is already
  up, it does nothing. It is already wired into SessionStart if you installed with
  `--with-courier`. On a keyword-only binary it does nothing at all, so the same
  hook is harmless everywhere.

  Only run the process in the foreground yourself if you are debugging it, and
  expect it to sit there:

  ```sh
  thor embed-daemon
  ```

- **How to check it worked:** a running process publishes a file named
  `thor-embedd.json` next to your store, holding its port, its process id and the
  model identity:

  ```sh
  thor warm
  ```

  Then look for `thor-embedd.json` in the same folder as `thor.db`. Its content
  looks like:

  ```json
  {"port":54321,"pid":12345,"model_id":"paraphrase-multilingual-MiniLM-L12-v2-onnx-Q@mean-v1"}
  ```

  `thor doctor` does not report this process. Its `injection daemon: WARM` or
  `COLD` line is about a different, separate process, so do not read it as an
  answer about the embedder.

- **How to turn it off again:** stop the process, or simply wait: it exits on its
  own after 20 minutes without a request and cleans up its port file. Nothing is
  lost, because it holds no data of its own.

  Be aware that stopping it is not sticky. As long as the model files and
  `thor-vectors.db` are both there, the next prompt through the recall hook starts
  it again. To keep it stopped for good you have to remove what triggers it: take
  the `thor warm` entry out of your agent's SessionStart hooks, and delete either
  the model folder or the vector file. Deleting either one only costs you the
  semantic layer, which drops recall back to keyword search.

## Cross-encoder rerank

This section covers one optional feature with two surfaces: the `--rerank` flag on
the `thor recall` command, and the `rerank: true` argument on the MCP `recall` tool
(MCP is the connection your AI agent uses to talk to THOR). Both do the same thing.
The decision this section helps you make is a narrow one: whether it is worth
downloading a second model so that, on a question you have already asked and whose
answer came back in the wrong order, you can ask again and get a different ordering.
For most people the honest answer is no.

### thor recall --rerank (and MCP recall `rerank: true`)

A reranker is a second, slower model that reads your question and one stored fact
*together* and scores how well they match. Normal THOR recall compares the question
and each fact separately and then compares the two scores, which is fast but can
misjudge a fact that says the same thing in different words. Rerank rescores the top
12 hits against your question with a full model pass and reorders them. It never
changes what is stored.

It does change more than the order, though, and this is easy to miss: with the flag
on, recall first fetches a deeper candidate pool - at least 12 candidates instead of
the 8 results a plain `thor recall` shows you - reorders that pool, and only then
cuts back to 8. So a fact that normally sits just below the cut can be pulled up into
your results. The set of results can change, not only their sequence. Only the first
12 hits are rescored; anything past that pool keeps the position it already had.

- **Default:** off, and it stays off unless you type it. There is no setting, config
  file or environment variable that can switch it on: on the CLI it is a flag you
  pass per command, and over MCP it is an argument on a single call. It is also off
  at the build level - the reranker code only exists in a binary built with the
  `semantic` feature. The published Windows and Linux *client* downloads have that;
  the `bm25` download for servers and containers does not, and neither does a plain
  `cargo build --release` from source.
- **Turn it on if:** you asked a question in your own words, in a way that shares
  almost no vocabulary with how the fact was written, and the result you wanted came
  back below results you did not want. Rerank is a deliberate second try on that one
  question. The README puts it plainly: it is "a second try when the normal order
  looks wrong, not something to switch on and forget."
- **Leave it off if:** you are looking something up exactly - a document name, a
  number, an identifier. The repo measured this and it *loses* there: exact-lookup
  questions can get worse. Also leave it off if you do not want to source and place a
  second model by hand: nothing in THOR downloads it for you. And it is never
  available on the automatic per-prompt path (the small recall block THOR injects
  into your agent's prompts), by design, because it is too slow for that.
  The measured result, on a 53-question set: top-1 improved by 3 percentage points,
  with 16 questions getting better and 7 getting worse; top-3 unchanged; top-5
  slightly worse. That mixed result is exactly why it is not a default.
- **What it costs:**
  - Time, and this is the main cost. One model pass per document: the README states
    roughly 1 second median for a 12-hit pool on a CPU.
  - Loading the model. The code states this costs seconds, because the ONNX runtime
    has to start a session on a model of a few hundred megabytes. Over MCP that is
    paid once and the loaded model is then kept in memory for as long as the server
    runs. On the CLI, every single `thor recall --rerank` pays it again.
  - Disk and download: the five model files you supply. The repo does not state an
    exact size for the reranker model beyond describing it as "a few-hundred-MB
    model", so no number is given here.
  - RAM: the repo states no measurement for the reranker's memory use. What the code
    does say is that the ONNX file is streamed from disk rather than read whole into
    memory.
  - A slightly wider search. The flag makes recall pull at least 12 candidates
    instead of 8 before rescoring, so the search step itself does a bit more work.
  - Accuracy has a ceiling you should know about: the reranker only reads the first
    1000 characters of each fact (its footer stripped first). A long document chunk
    is therefore judged on its opening, not its whole body.
  - No extra background process, no network port, no network call at recall time, no
    automatic download, and no extra dependency - it reuses the same library the
    semantic build already pulls in, so it does not make the binary bigger than the
    semantic build already is.
- **How to turn it on:**

  First, make sure your `thor` binary has the semantic feature. If you built it
  yourself, that means:

  ```sh
  cargo build --release --features semantic
  ```

  If you are not sure which kind of binary you have, ask THOR:

  ```sh
  thor doctor
  ```

  A binary without the feature prints the single line
  `semantic: not built in (bm25-only binary)`. A binary with it prints two other
  lines instead, starting with `semantic model:`. Only the second kind can rerank.

  Then place five reranker files in THOR's reranker folder. The ONNX file must be
  named exactly `model.onnx` (rename it if your download calls it something else),
  alongside `tokenizer.json`, `config.json`, `special_tokens_map.json` and
  `tokenizer_config.json`. A multilingual base reranker is a reasonable choice. The
  folder is:

  ```text
  %LOCALAPPDATA%\thor\reranker\        # Windows
  $XDG_DATA_HOME/thor/reranker/        # Linux and macOS, when XDG_DATA_HOME is set
  $HOME/.local/share/thor/reranker/    # Linux and macOS otherwise
  ```

  **This changed:** like the embedding model, this used to fall back to a folder
  named `thor-reranker` relative to the directory you ran `thor` from whenever
  `LOCALAPPDATA` was unset. It now sits in THOR's per-user home on every platform.
  If you have a `thor-reranker` folder from before, move its contents.

  Then use it, per call:

  ```sh
  thor recall "<your question>" --rerank
  ```

  Over MCP, call the `recall` tool with `rerank: true` in its arguments.

  If you install the model while your agent's MCP server is already running, restart
  the agent. The first failed load is remembered for the life of that process: a
  model that was missing at startup does not get picked up mid-run.

- **How to check it worked:** run the same question twice, once without the flag and
  once with it, and compare the two lists (both the order and which facts appear):

  ```sh
  thor recall "<your question>"
  thor recall "<your question>" --rerank
  ```

  If the model is missing, failed to load, or there was nothing to reorder, THOR says
  so out loud and returns the normal order. It is never an error. On the CLI the line
  is:

  ```text
  (rerank skipped: reranker model unavailable or nothing to reorder)
  ```

  and on a build without the semantic feature:

  ```text
  (rerank unavailable: non-semantic build)
  ```

  Over MCP the same two cases print slightly different text, both ending in
  `- fused order)`.

  Two honest caveats about reading that output. First, the "skipped" line does not
  only mean a missing model: the exact same line appears with a perfectly good model
  installed when the pool had fewer than two hits, since there is nothing to reorder.
  Second, the absence of the line does not prove the order actually changed - if the
  reranker hands back an order that would drop or duplicate a hit, THOR voids it and
  returns the original order without a note. The only reliable check is comparing the
  two outputs yourself.

- **How to turn it off again:** stop passing the flag, or stop sending
  `rerank: true`. It applies to one call only, so there is no state to unwind and
  nothing to uninstall. If you want it gone for good, delete the reranker folder.
  That is lossless: the reranker only reorders what recall retrieved, it never writes
  anything to the store, and with the model absent every recall simply returns the
  normal order.

## The warm injection daemon

If you installed the courier (the hook that searches your memory before every prompt and pastes what it finds into the prompt), every prompt pays for a fresh start: a new process, the store read from disk, the whole event log folded, the vector matrix scanned. The warm injection daemon is one long-lived background process that keeps that work in memory so the courier can just ask it. This section helps you decide whether that trade is worth it on your machine. If you do not run the courier, you can skip the whole section: there is nothing here to speed up.

### thor daemon

Starts one long-lived local web server that holds THOR's read state in memory and answers the courier over it. "Long-lived" means it keeps running after the command returns; it does not stop when your agent session ends.

- **Default:** off. Nothing starts it. Without it the courier does the same work in its own short-lived process on every prompt, and gives the same answer.
- **Turn it on if:** you use the courier and the wait before each prompt bothers you. The repo measures the courier at 120 ms per prompt with the daemon running against 349 ms with it stopped (median of 20 prompts, on a store of 16.1k events, measured 2026-07-15). That is roughly 60% off the per-prompt wait, and the injected text is byte-identical either way.
- **Leave it off if:** memory on this machine is tight, or you do not want a permanent local network listener on the box. Also leave it off on a bm25-only build (a build without the `semantic` feature): the resident cache that produces the speed-up is compiled only into the semantic build, so a bm25-only daemon saves you nothing but process startup, and the repo's only figure for that component is about 15 ms. The same holds on a semantic build that has no vector sidecar yet: the daemon builds its cache on the first prompt only when the file `thor-vectors.db` sits next to your store, and without it every prompt takes the same path the courier takes on its own.
- **What it costs:**
  - One extra process, running until you stop it or reboot.
  - One open TCP port on the loopback interface, `127.0.0.1:8765` by default. Loopback means only programs on this same machine can reach it. This matters more than it first looks: `thor daemon` is not an inject-only server. It is exactly the same server as `thor mcp --http`, so the full MCP tool surface, including the tools that write to your memory, is mounted at `/mcp` on that same port, and the port carries no authentication of its own. Anything that can open a socket on your machine can read and write your store through it. Never bind it beyond loopback. If you do, the daemon prints a warning at startup and keeps running.
  - Memory. README and SETUP both put the resident cost at a few hundred MB, and both say plainly that the repo has never measured this process. Be careful with any 650 MB figure you meet: the only place the repo says 650 MB was actually measured is about a different process, the embedding-model daemon. Read the real figure off your own task manager once it is up.
  - A worst case when the daemon is alive but stuck. The courier waits at most 450 ms for an answer, then spends up to another 150 ms checking whether the daemon is really dead, then does the cold path anyway. How often you pay that depends on what the check finds. If the daemon does not answer the check either, the discovery file is deleted and every later prompt goes straight to the cold path, so you pay it once. If it does answer, which is what a daemon that is alive but still building its resident state does, the discovery file is kept on purpose, and a prompt sent while it is still busy can pay the same wait again. A daemon that is simply gone costs almost nothing here: the connection is refused immediately.
  - It holds the `thor` binary open. To install a new THOR build you have to stop and restart the daemon.
  - No download, no extra dependency, no larger binary.
- **How to turn it on:**

  ```sh
  thor daemon
  ```

  That runs in the foreground and stays there. Nothing bad happens if you type it twice: when a healthy THOR daemon on the same store already holds the port, the second one prints that it is already running and exits without starting a thing. Use `thor ensure-daemon` (below) if you want it started in the background instead. To pick another port:

  ```sh
  thor daemon --bind 127.0.0.1:8766
  ```

- **How to check it worked:**

  ```sh
  thor doctor
  ```

  Look for the line `injection daemon: WARM (pid <n>, bind <addr>, db <path>)`. When it is not running the same line starts with `injection daemon: COLD` and then tells you to run `thor daemon` or install with `--with-daemon` to warm it.

- **How to turn it off again:** stop the process. `thor doctor` prints its pid, so `taskkill /PID <pid> /F` on Windows or `kill <pid>` on Linux and macOS. Nothing is lost. The daemon holds only a cache of state that is rebuilt from the store, and the courier falls back to its own cold path on any failure. If you also wired it into your agent's settings (see `thor install --with-daemon`), remove that hook entry too, otherwise your next session starts it again.

### thor ensure-daemon

Starts the daemon only if it is not already running, in the background, and returns immediately. This is the form meant for automation, not for you to type by hand.

- **Default:** off. Nothing runs it unless you install it as a hook or type it yourself.
- **Turn it on if:** you want the daemon started for you without a terminal window sitting open, or you are writing your own startup script. Normally you do not call this directly; you install it as a session hook with `thor install --with-daemon`.
- **Leave it off if:** you decided against the daemon at all, for the reasons under `thor daemon` above.
- **What it costs:** nothing on its own when the daemon is already up. It asks the daemon's `/health` endpoint first, and returns straight away if it answers. If it does not answer, it spawns `thor daemon` fully detached (no window, no console output) and returns. Two of these firing at the same second do not produce two daemons: a 15-second marker file lets only one start win. All the real costs are the daemon's, listed above.
- **How to turn it on:**

  ```sh
  thor ensure-daemon
  ```

- **How to check it worked:**

  ```sh
  thor doctor
  ```

  Same line as above: `injection daemon: WARM (pid ..., bind ..., db ...)`. Give it a moment on a large store. The daemon answers `/health` while it is still loading its state, so `doctor` can say WARM a little before the first prompt is actually fast.

- **How to turn it off again:** it starts nothing on its own, so there is nothing to undo beyond stopping the daemon it started (see above).

### thor install --with-daemon

Adds one entry to your agent's `settings.json`: run `thor ensure-daemon` whenever a session starts. That is the whole difference the flag makes. It writes a hook; it does not start anything at install time. One thing to expect if this is your first `thor install` on that settings file: every install run, flags or no flags, also adds the Stop response guard entry when it is not there yet, so you will see two lines added instead of one.

- **Default:** off. `thor install` with no flags does not write this entry, and `thor install --with-courier` does not either.
- **Turn it on if:** you have decided you want the daemon and you want it to be there without thinking about it. SETUP calls `thor install --with-courier --with-guard --with-daemon` the full setup, and the one to run on the machine your agent actually works on.
- **Leave it off if:** you do not run the courier. This flag exists only to speed up the per-prompt courier hook, so on a machine without the courier it starts a process that nothing asks anything of. Leave it off too for the memory and open-port reasons under `thor daemon`.
- **What it costs:** the install itself costs nothing measurable: it backs up your `settings.json` first, only adds THOR entries, and never touches hooks you already had. Every real cost is the daemon's, and you start paying it at your next session.
- **How to turn it on:**

  ```sh
  thor install --with-daemon
  ```

  Or as part of the full set:

  ```sh
  thor install --with-courier --with-guard --with-daemon
  ```

- **How to check it worked:** the install prints what it added, including `SessionStart (warm injection daemon ensure-start)`, and ends with `Restart Claude Code for the hooks to take effect.` It means that literally: the session you ran the install in does not get the hook. Start a new agent session, then run:

  ```sh
  thor doctor
  ```

  The `injection daemon:` line should read WARM.

- **How to turn it off again:** open your agent's `settings.json`, delete the `SessionStart` entry whose command ends in `ensure-daemon`, and stop the running daemon. Lossless: the courier goes back to its cold path, injects the same text, and only takes longer.

## Hooks: automatic recall, guards and nudges

A hook is a small command your coding agent runs by itself at a fixed moment: when you
submit a prompt, when a session starts, before it runs a tool, when it finishes a turn.
THOR ships several such commands, and `thor install` is the one command that writes them
into your agent's settings file for you. This section helps you decide which of them you
actually want on, and what each one costs you in wait time, memory and noise.

Two things hold for every block below. First, `thor install` never removes anything: it
copies your settings file to `settings.json.thor-bak` first, only adds THOR's own
entries, and re-running it adds nothing a second time. Second, hooks only take effect
after you restart your agent - nothing you install shows up in the session you installed
it from.

### thor install

Writes THOR's hook entries into your agent's settings file. With no flags at all it
installs exactly one hook: the Stop response guard, which runs `thor stop-guard` every
time the agent finishes a turn. That hook does two things. It can hold the turn when the
agent's closing message matches a response rule (for example, asking you to do something
it could have done itself), and once per session it can hold the turn when the message
looks like it contains a decision or a gotcha that was never stored.

- **Default:** nothing. THOR installs no hooks until you run this command yourself. If
  you run it, the Stop response guard is added whether or not you pass any other flag -
  every flag below is added on top of it, never instead of it.
- **Turn it on if:** you want the agent to be caught just before it yields, either
  because it handed work back to you that it could do itself, or because it ended a turn
  on a durable decision it never wrote down.
- **Leave it off if:** you do not want anything editing your agent's settings file, or
  you never want a hook that can block a stop. This one is not advisory: it holds the
  turn. A false trigger word costs you one forced extra turn. The source names that risk
  itself and keeps the built-in trigger list deliberately narrow because of it.
- **What it costs:** one short-lived `thor stop-guard` process per finished turn. No port,
  no long-lived process, no download, and no extra binary size (the guard is compiled into
  the same binary either way). A blocked stop costs one extra turn. The repo states no
  measurement of the Stop hook's own run time.
- **How to turn it on:**
  ```sh
  thor install
  ```
  It writes to your user-level settings file by default (`%USERPROFILE%\.claude\settings.json`
  on Windows, `~/.claude/settings.json` elsewhere). Restart your agent afterwards.
- **How to check it worked:** the command prints the file it wrote to, followed by the
  list of what it added:
  ```
  THOR hooks installed into <path to settings.json>
    + Stop (response guard)
    backup: <path to settings.json.thor-bak>
  ```
  Run it a second time and it prints `(nothing to add - THOR hooks were already present)`.
- **How to turn it off again:** delete that one entry from the `hooks.Stop` array in the
  settings file (the pre-install copy sits next to it as `settings.json.thor-bak`), then
  restart the agent. There is no `thor uninstall` subcommand - removal is by hand.
  Nothing stored is lost; hooks only read and nudge.

### thor install --with-courier

Adds the per-prompt auto-recall. Every prompt you submit, a `thor courier` process looks
in the store and prints a `<thor-recall>` block into the prompt, so stored gotchas and
decisions reach the agent without anyone searching for them. This one flag also wires
three companion hooks in the same run: `thor warm`, `thor session-start` and
`thor pre-compact` (each described below).

- **Default:** off. Without this flag no recall is injected anywhere; you only get memory
  when you or the agent asks for it.
- **Turn it on if:** this is the machine your agent actually works on and you want stored
  facts to surface by themselves.
- **Leave it off if:** the machine only holds a replica of the store (a server, a NAS, a
  container) - a remote store does not run the courier anyway. Also leave it off if you
  want zero automatic text in your prompts.
- **What it costs:**
  - One extra `thor courier` process per prompt. Measured on a 16.1k-event store, median
    of 20 prompts: 349 ms with no warm daemon, 120 ms with one (see `--with-daemon`).
  - Prompt space: at most 3 hits per prompt, inside a hard ceiling of 8000 characters
    (roughly 2000 tokens). That is a ceiling, not a target.
  - The `thor warm` hook it installs is not free on a build with the semantic feature: it
    starts a long-lived embedder process that holds the embedding model resident. On a
    bm25-only build it does nothing.
  - The `thor session-start` hook it installs spawns a detached background re-index of the
    current project at every session start, if that project carries a `.thor` marker file.
    On a large repo or a slow disk that competes with your first prompt.
- **How to turn it on:**
  ```sh
  thor install --with-courier
  ```
  Then restart your agent.
- **How to check it worked:** the install output lists the added entries. On a first
  install the Stop response guard is in that list too, because every `thor install` run
  adds it:
  ```
    + Stop (response guard)
    + UserPromptSubmit (recall courier)
    + SessionStart (pre-warm embedder)
    + PreCompact (persist-before-compaction nudge)
    + SessionStart (project refresh + onboarding cue)
  ```
  In a new session, a `<thor-recall>` block appears with your prompt once the store has
  something relevant to say.
- **How to turn it off again:** delete THOR's entries from the settings file (the
  `.thor-bak` copy holds the pre-install state), or silence THOR at runtime by creating an
  empty file named `THOR-SILENT.flag` next to the store. Both are lossless: nothing stored
  is touched. Note that the silence flag does not stop either of the two SessionStart hooks
  this flag installs: neither `thor session-start` nor `thor warm` checks it. The ones that
  do check it are the courier, the pre-compact nudge, the Stop guard and the
  before-the-tool-call guard.

### thor session-start

One command your agent runs at the start of every session, and again right after a
context compaction. It does three useful things: it re-injects your pinned facts as a
`<thor-brief>` block, it refreshes the index of a project that has a `.thor` marker in
the background, and for a git project THOR does not know yet it prints a `<thor-setup>`
cue so the agent offers to set it up instead of indexing anything behind your back. It
can also nudge, at most once per time window, to review global facts that were stored
without a project.

- **Default:** off, and it is not separately installable - `thor install --with-courier`
  is the only installer flag that wires it.
- **Turn it on if:** you use `thor pin` (pins do nothing without this hook), you work
  across several indexed projects, or you want the after-compaction re-injection to be
  reliable. A continuation prompt like "carry on" shares no words with your standing
  rules, so per-prompt recall cannot bring them back on its own.
- **Leave it off if:** you only ever use THOR by hand and keep no pins. Then it costs you
  a detached `thor ingest` process at every session start in a marked project, plus the
  `<thor-setup>` and `<thor-scope-review>` text in your context, and buys nothing.
- **What it costs:** one short-lived process per session start, plus the detached
  background re-index described above in a marked project. No port, no download. The repo
  states no measurement of its own run time. One honest caveat: unlike the courier, this
  hook does not check `THOR-SILENT.flag`, so the kill switch does not silence it.
- **How to turn it on:**
  ```sh
  thor install --with-courier
  ```
  To have it without the courier, add the entry by hand to the `SessionStart` array of
  your settings file. Copy the command string the installer writes: it is the quoted
  absolute path of the `thor` binary followed by `session-start`, not a bare `thor`.
- **How to check it worked:** the install output lists
  `+ SessionStart (project refresh + onboarding cue)`, and your settings file then has a
  `SessionStart` entry whose command ends in `session-start`. In a fresh session in a
  project with pins, a `<thor-brief>` block appears.
- **How to turn it off again:** delete that one `SessionStart` entry and restart the
  agent. Lossless: pins, index and store all stay exactly as they are, they simply stop
  being re-injected for you.

### thor pre-compact

When your agent is about to compact its context (summarise the conversation and throw the
detail away), this hook prints one advisory line asking it to store durable decisions and
gotchas first. It fires at most once per session.

- **Default:** off; installed only by `thor install --with-courier`.
- **Turn it on if:** you run long sessions that hit compaction and you want one prompt to
  write things down before they are compacted away. This is the only moment memory can act
  before the context is gone - pins and the brief are recovery afterwards.
- **Leave it off if:** you do not use the courier at all, or you keep your agent's context
  deliberately free of injected text. The advisory is unconditional: it does not check
  whether anything was actually left unstored, so in a session where you capture
  diligently it is pure noise.
- **What it costs:** one short-lived process, once per session, and a few lines of context.
  No port, no download, no long-lived process. The repo publishes no measured number for
  what it saves.
- **How to turn it on:**
  ```sh
  thor install --with-courier
  ```
  Restart the agent. To scope it to one project instead, add `--settings <path>` (below).
- **How to check it worked:** the install output lists
  `+ PreCompact (persist-before-compaction nudge)`, and the settings file then contains a
  `hooks.PreCompact` array whose command ends in `pre-compact`.
- **How to turn it off again:** delete the `PreCompact` entry from the settings file and
  restart. `thor install` only ever adds entries, so a hand-removed entry stays removed.
  Lossless.

### thor install --with-daemon

Adds a session-start hook that starts a warm background process (a "daemon") if one is not
already answering. While it runs, the per-prompt courier asks that process instead of
opening the store and rebuilding its working state from scratch each time. The injected
text is the same either way.

- **Default:** off. Without it the courier works exactly the same, just from a cold start
  every prompt.
- **Turn it on if:** per-prompt wait time matters to you and you can spare the memory.
  Measured on a 16.1k-event store, median of 20 prompts: 349 ms without the daemon,
  120 ms with it, with byte-identical injection.
- **Leave it off if:** RAM is tight, or you do not want a long-lived local server process
  on the machine. Also note it holds the binary file open, so replacing `thor` requires
  stopping it first.
- **What it costs:**
  - One extra long-lived detached process per store.
  - A listening port on the loopback interface, by default `127.0.0.1:8765`. It is not a
    recall-only endpoint: the same server mounts THOR's full tool surface, so anything that
    can reach that port can read and write your memory. Keep it on loopback.
  - RAM: the repo states no measurement for this daemon. The one measured 650 MB
    figure in the tree belongs to the separate embedding-model process, so do not
    read it as this daemon's cost. Budget for it being significant and read the real
    number off your own task manager once it is up.
- **How to turn it on:**
  ```sh
  thor install --with-daemon
  ```
  It only pays off together with `--with-courier`; it exists to make that per-prompt hook
  faster. The usual combination is:
  ```sh
  thor install --with-courier --with-guard --with-daemon
  ```
- **How to check it worked:**
  ```sh
  thor doctor
  ```
  It prints `injection daemon: WARM (pid ..., bind ..., db ...)` when the daemon is up,
  and `injection daemon: COLD` when it is not.
- **How to turn it off again:** remove the `SessionStart` entry whose command ends in
  `ensure-daemon` and stop the running process. The courier falls back to the cold path
  with no change in what it injects. Lossless.

### thor install --with-guard

Adds a hook that runs before every tool call your agent makes (a shell command, a file
edit, and so on). It can print a short advisory into the agent's context, from three
sources: a rule in your own risk rulebook matches the tool call, or this is the first time
this session the agent touches a file you have stored a memory about, or the shell command
about to run contains a distinctive word (a host name, a subcommand, a flag) that a stored
gotcha, decision or preference names. The last two are the ones that catch what per-prompt
recall cannot see: your prompt may share no words at all with the constraint, while the
file path or the command does.

- **Default:** off. The Stop response guard is installed by every `thor install` run, but
  this before-the-tool-call guard is not.
- **Turn it on if:** you have project-specific constraints worth surfacing at the moment of
  action (a deploy route, a production container, a secret handling rule). In the repo's
  own replayable drift test the guard channel catches 16 of 16 of its scenarios.
- **Leave it off if:** you would be installing it into your user-level settings file. The
  rulebook is project-specific by design, so a global install gives wrong deploy advice in
  unrelated projects. Scope it to a project instead (see `--settings`). Also weigh that it
  runs on every single tool call.
- **What it costs:** one short-lived process per tool call. No long-lived process, no port,
  no download, no extra binary size. The first touch of a given file in a session pays a
  store open plus a recall, and so does the first sight of a given set of command words; a
  "nothing stored about this" answer is remembered for 15 minutes, so the common case of an
  unremarkable tool call stays cheap. The advisory is advisory only: it never decides a
  permission, it only adds text.
- **How to turn it on:**
  ```sh
  thor install --with-guard --settings <your-project>/.claude/settings.json
  ```
  Restart the agent. No advisory can appear in the session you ran the install from.
- **How to check it worked:** the command prints `+ PreToolUse (command guard)`, and a
  re-run prints the "nothing to add" line. In a later session, a flagged tool call is
  preceded by a `[THOR guard]` line in the agent's context.
- **How to turn it off again:** remove the `PreToolUse` entry from that settings file (the
  `.thor-bak` copy holds the pre-install state), or create `THOR-SILENT.flag` next to the
  store to silence it at runtime. Lossless.

### thor install --settings &lt;path&gt;

Writes the hook entries into the settings file you name instead of the user-level one. Use
it to confine hooks to a single project.

- **Default:** absent. With no `--settings`, install writes to `%USERPROFILE%\.claude\settings.json`
  on Windows, `~/.claude/settings.json` when `HOME` is set, and - if neither variable is
  set - to the relative path `.claude/settings.json` under whatever directory you are
  standing in. Check the path the command prints if you are unsure which of the three you
  got.
- **Turn it on if:** you are installing the before-the-tool-call guard, whose rulebook
  belongs to one project, or you want THOR's injection active in one project only.
- **Leave it off if:** you are installing the courier. Recall is already scoped to the
  project of the session's working directory, so one user-level install is simpler and
  never bleeds another project's facts in.
- **What it costs:** nothing at runtime. The flag only changes which file gets written.
- **How to turn it on:**
  ```sh
  thor install --with-guard --settings <your-project>/.claude/settings.json
  ```
- **How to check it worked:** the command prints `THOR hooks installed into <path>` with
  the path you named.
- **How to turn it off again:** delete the added entries from that file; the `.thor-bak`
  copy next to it holds the pre-install state. One thing to know: `--settings` only
  redirects where the entries are written, it does not let you choose which hooks land
  there. The Stop response guard is added to whatever file you name.

### thor install --backup-repo &lt;path&gt;

Adds a session-start hook that exports your whole memory as one JSON-per-line file into a
git clone you already have, then commits and pushes it. It skips itself unless the last
backup commit is more than 20 hours old.

- **Default:** off. No backup is ever pushed anywhere unless you pass this flag.
- **Turn it on if:** you already keep a local clone of a backup repository, git on that
  machine has push credentials for it (THOR shells out to plain `git` and relies on it for
  authentication), and you want an off-machine, versioned copy without thinking about it.
  The copy is provably restorable: `thor restore --from <file>` replays it into a fresh
  store and fails loudly if any event's hash does not reconstruct.
- **Leave it off if:**
  - You have no such clone, or its remote and branch are not `origin` and `main`. Both are
    hardcoded; anything else fails.
  - You do not want your entire memory in a remote repository. The export writes every
    event's full body verbatim in plaintext, so every stored fact and every indexed code
    chunk lands in that repository's git history. Only point it at a repository you are
    sure should hold all of that, and never at a public one.
- **What it costs:** one export plus a `git pull --rebase`, `git commit` and `git push` at
  most once per 20 hours, at session start. Disk in the clone grows with your event log.
  The repo states no measurement of how long the export takes.
- **How to turn it on:**
  ```sh
  thor install --backup-repo <path-to-a-git-clone>
  ```
  It combines freely with the other flags:
  ```sh
  thor install --with-courier --with-guard --with-daemon --backup-repo <path-to-a-git-clone>
  ```
- **How to check it worked:** the install run prints
  `+ SessionStart (daily GitHub backup, debounced 20h)`. You can also run the underlying
  command by hand and read its one-line result:
  ```sh
  thor backup --repo <path-to-a-git-clone>
  ```
  It prints either `pushed thor backup (<n> events)`, `no change since last backup ...`,
  or `backup is <n>h old (< 20h) - skipping`.
- **How to turn it off again:** remove by hand the `SessionStart` group whose command
  contains `backup --repo` and restart the agent. Nothing is lost locally, and whatever
  was already pushed stays in that repository's history - removing the hook does not
  unpublish it.

### Registering THOR as an MCP server (thor mcp)

MCP is the protocol your agent uses to call outside tools. Registering THOR this way runs
the same binary as a small server your agent talks to, and gives it THOR's full toolset to
call directly: recall, get, history, brief, remember, revise, retract, resolve, mark, pin,
unpin, reproject, plus the three code-navigation tools outline, where_used and impact.

- **Default:** not registered. THOR does nothing through MCP until you add it to your
  agent's MCP configuration.
- **Turn it on if:** you want the agent to maintain the memory and not only be fed by it:
  storing decisions mid-session, correcting or retracting a fact it finds wrong, pinning a
  standing rule. This is the only guarded write path. The MCP `remember` tool refuses near
  duplicates, composes the typed footer and applies the project scope. The command line has
  no `remember` equivalent - its write path is the raw `thor create <entity_id> <body>`,
  which does none of that.
- **Leave it off if:** you only want passive auto-recall. The courier already injects per
  prompt, and searching, reading history, indexing, pinning and marking all exist as
  command-line subcommands.
- **What it costs:** a long-lived process for as long as your agent session runs. It holds
  the binary file open, so replacing `thor` while it runs fails and the server keeps the
  old binary until the session restarts. Over stdio it opens no network port. It adds tool
  definitions to the agent's context.
- **How to turn it on:** register the built binary with your agent, exactly as the
  subcommand's own help states:
  ```sh
  claude mcp add thor -- <path-to-thor-binary> mcp
  ```
  On Windows the documented per-user location of the binary is `%LOCALAPPDATA%\thor\thor.exe`.
  Restart the agent afterwards.
- **How to check it worked:**
  ```sh
  claude mcp list
  ```
  The `thor` entry appears there, and in a fresh session the agent lists THOR's tools
  (recall, remember, brief, ...).
- **How to turn it off again:** remove the registration from your agent's MCP
  configuration and restart the session. Lossless: the MCP server is only a reader and
  writer of the same store, and removing it removes no data.

### Running THOR with no hooks at all

The deliberate opposite of everything above: install no hooks, and use THOR only when you
ask it something. Nothing is injected, nothing is held, nothing runs in the background.

- **Default:** this is the default. There is exactly one place in the whole codebase that
  writes to a settings file, and it only runs when you type `thor install`.
- **Turn it on if:** the machine only holds a replica of the store (server, NAS,
  container), your agent configuration is shared or locked down, or you want memory
  available on request but never volunteered.
- **Leave it off if:** this is your main working machine and you want the automatic layer.
  Everything automatic is exactly what you give up: no recall injected per prompt, no pins
  re-injected at session start or after a compaction, no nudge before a compaction, no
  advisories before a tool call, no capture nudge at the end of a turn.
- **What it costs:** nothing. No process, no port, no RAM, no disk, no download, no added
  latency. Nothing runs until you invoke a command yourself.
- **How to turn it on:** do not run `thor install`. If you already did, delete THOR's
  entries from the settings file. Use the store directly:
  ```sh
  thor recall "how does X work"
  thor get <entity_id>
  ```
  Registering the MCP server (previous block) is compatible with this: it is a tool the
  agent may call, not a hook that fires on its own. Storing facts properly needs it - the
  command line's only write path is the raw `thor create <entity_id> <body>`, without the
  duplicate check, the typed footer and the project scope that the MCP `remember` tool
  applies.
- **How to check it worked:** your agent's settings file contains no command with `thor`
  in it, no `<thor-recall>` or `<thor-brief>` block appears in a session, and
  `thor recall "..."` still answers from a terminal.
- **How to turn it off again:** run the install with the flags you want, for example
  `thor install --with-courier --with-guard --with-daemon`, then restart the agent. It
  backs up the settings file first and only adds THOR entries. Nothing was lost while the
  hooks were off: the log, the projects, sync and backups are all independent of them.

## Guard rulebooks

A "hook" is a small command your agent runs automatically at a fixed moment: just before it uses a tool, or just before it finishes its turn. THOR ships two such hook commands (`thor guard` and `thor stop-guard`), and each one can read a plain JSON file that you write, called a rulebook: a list of text patterns, and the sentence THOR should say when a pattern matches. This section helps you decide whether to write any of those files at all, and where to put them.

All three files below are optional. None of them is created by the installer, and a missing or broken one is always silent, never an error. Where they live is fixed and per-user, never inside a repo:

- Windows: `%LOCALAPPDATA%\thor\`
- Linux/macOS: `$XDG_DATA_HOME/thor/`, and if that variable is not set, `$HOME/.local/share/thor/`

That is the same folder as the store file `thor.db`. THOR deliberately never looks for a rulebook in the folder you happen to be working in, because a repository you cloned could then plant one and feed text straight into your agent's context.

Templates for the first two files are tracked in this repo as `thor/guard-rulebook.example.json` and `thor/guard-response-rulebook.example.json`. They are examples to copy and rewrite, not defaults: their contents describe one specific setup, not yours.

### guard-rulebook.json

A list of rules matched against every tool call your agent is about to make. When a rule matches, THOR adds one sentence of advice to the agent's context at that exact moment: for example, "do not `docker cp` into the production container, that is a hot patch and not a deploy". It only advises. It never blocks the tool call, and it never approves one either, so your normal permission prompts behave exactly as if the hook were not there.

- **Default:** absent, and inert twice over. The file is not shipped and nothing creates it, and the hook that would read it (`PreToolUse`) is not installed by a plain `thor install` at all. The installer adds it only with `thor install --with-guard` - or you wire a `PreToolUse` entry that runs `thor guard` into `settings.json` yourself. What the file depends on is that hook, not that flag.
- **Turn it on if:** you have commands that are dangerous or against policy in a way an agent cannot work out from reading the code - a deploy route it must not shortcut, a hot-patch path, a bypass flag like `git commit --no-verify`, a secret it must never print. These are exactly the cases ordinary recall misses, because your prompt shares no words with the rule.
- **Leave it off if:** you cannot keep it up to date. The rules are project-specific by design, which is why the hook is opt-in: installed globally, a rulebook written for one project gives wrong advice in every other one. Also leave it off if you cannot phrase your rules narrowly - see the cost below.
- **What it costs:** no extra process, no network port, no download, no larger binary; the file is data, not code. Per tool call it costs one file read, one JSON parse, and a set of substring scans. The repo states no measurement of that added latency. What is not free is the hook you have to install to use the file: `thor guard` also carries the memory advisories, so the first tool call in a session that touches a given file additionally pays a store open plus a recall, and a "no memory names this file" answer is cached for only 15 minutes (`NEG_CACHE_SECS` in `thor/src/guard.rs`) before the next touch pays again. The real cost is context: every reminder that fires is inserted into your agent's context, so a rule that is too broad pays for itself on every single tool call, forever.
- **How to turn it on:** copy the template into the per-user folder, then edit it. Every string in it is an example and must be replaced. From the repo root:

  Windows (PowerShell):

  ```powershell
  thor install --with-guard
  copy thor\guard-rulebook.example.json "$env:LOCALAPPDATA\thor\guard-rulebook.json"
  ```

  Linux/macOS:

  ```sh
  thor install --with-guard
  cp thor/guard-rulebook.example.json "${XDG_DATA_HOME:-$HOME/.local/share}/thor/guard-rulebook.json"
  ```

  Restart your agent after the install command. It says so itself on the last line it prints, "Restart Claude Code for the hooks to take effect.", and it means it: the session you ran it from has no `PreToolUse` hook, so no advisory can appear there no matter how good your rules are.

  Each rule is one JSON object. The fields:

  ```json
  {
    "id": "hot-patch-is-not-a-deploy",
    "tools": ["Bash", "PowerShell"],
    "all_of": ["docker cp"],
    "any_of": [],
    "none_of": ["-dev:", "_dev:"],
    "reminder": "docker cp into a live container is not a deploy."
  }
  ```

  - `reminder` is mandatory. A rule object without a `reminder` string is dropped silently, so a typo there costs you the whole rule with no warning.
  - `tools` limits the rule to certain tool names; leave it out or empty and the rule applies to any tool. A single `"tool": "Bash"` string works too.
  - `all_of` must all be present, `any_of` needs at least one present (an empty or missing `any_of` means "no extra condition"), and `none_of` cancels the rule if any one of them is present. Use `none_of` for the safe twin of a dangerous command.
  - Matching is plain substring matching, case-insensitive, with no word boundaries and no regular expressions. It runs over every text value in the tool call flattened together, so a pattern matches whether it appears in a shell command, a file path, or the body of an edit.
  - `id` is read but never printed. It is there for you, to keep your own file readable.

- **How to check it worked:** feed the guard a fake hook payload on standard input. This example matches the template's `docker cp` rule:

  ```sh
  echo '{"tool_name":"Bash","tool_input":{"command":"docker cp fix.js app-prod:/srv/app/fix.js"}}' | thor guard
  ```

  A matching rule prints one line of JSON containing `"hookEventName":"PreToolUse"` and an `additionalContext` string that starts with `[THOR guard] ` followed by your reminder. No match prints nothing at all and still exits 0. That silence is normal: the guard is built to fail open, so a bad rulebook can never wedge a tool call.
- **How to turn it off again:** delete or rename the file. A missing rulebook is silent, and the rest of the guard (the memories THOR surfaces about a file or command you are about to touch) keeps working. To stop the hook running entirely, remove the `PreToolUse` entry from your agent's `settings.json`, or drop an empty `THOR-SILENT.flag` file next to `thor.db`, which silences every injecting hook at once: the recall courier, both guards and the capture nudge. It is not quite everything THOR can install - the `SessionStart` commands (`thor session-start`, `thor warm`, `thor ensure-daemon`, `thor backup`) never read that flag and keep doing their work. Nothing stored is lost either way; the rulebook is input, never data THOR keeps.

### guard-response-rulebook.json

The same rule format, but matched against your agent's final message instead of a tool call. When a rule matches, THOR holds the turn open and hands the reminder back to the agent so it reconsiders before yielding. The case it is built for: the agent asking you to do something it could have done itself.

- **Default:** absent. The `Stop` hook that reads it is installed by default (a plain `thor install` adds it, no flag needed), so the hook already runs on every turn; with no file present it simply has no response rules and only the capture nudge below can fire.
- **Turn it on if:** your agent keeps handing work back to you that it is actually equipped to do - asking which branch to push to when the repo's rules already say, or claiming it has no access without checking - and one interrupted turn is a fair price for making it try.
- **Leave it off if:** you are not willing to pay a wasted turn on a false positive. A matching rule stops the turn. Matching is plain substrings with no word boundaries, so a short or generic phrase like "can you check" will fire on innocent prose that merely contains it. Every reply is scanned on every stop.
- **What it costs:** no extra process, no port, no download, no larger binary. The hook that reads it runs anyway, so enabling this adds a file read, a JSON parse and some substring scans per finished turn. The repo states no measurement for that. The cost that matters is the wasted turn on a bad rule.
- **How to turn it on:** copy the template and rewrite the phrases to match how your agent actually talks to you. The shipped phrases are a mix of English and Dutch from one specific setup; keep only what applies to you.

  Windows (PowerShell):

  ```powershell
  copy thor\guard-response-rulebook.example.json "$env:LOCALAPPDATA\thor\guard-response-rulebook.json"
  ```

  Linux/macOS:

  ```sh
  cp thor/guard-response-rulebook.example.json "${XDG_DATA_HOME:-$HOME/.local/share}/thor/guard-response-rulebook.json"
  ```

  Two differences from the command rulebook: leave `tools` out of every rule (the matcher is handed the literal tool name `response`, so any rule that names real tools can never match), and remember there is no command text here - the only thing searched is the assistant's last message.
- **How to check it worked:** send it a fake Stop payload whose message contains one of your phrases:

  ```sh
  echo '{"session_id":"probe-1","last_assistant_message":"which branch do you want me to push to?"}' | thor stop-guard
  ```

  A firing rule prints `{"decision":"block","reason":"[THOR] ..."}` with your reminder after the prefix. Nothing printed means no rule matched. Note what this output is and is not: the repo shows the reason being written as the hook's JSON result, which is what makes the agent reconsider. Whether your agent also displays that text to you is up to your agent, and the repo makes no claim about it.
- **How to turn it off again:** delete the file. The response rules go empty and the capture nudge below keeps working. To stop the hook entirely, remove the `Stop` entry from `settings.json`, or create `THOR-SILENT.flag` next to `thor.db`. Nothing is lost.

### guard-capture-triggers.json

A JSON array of plain strings, nothing else. If your agent's final message contains one of them, THOR blocks the stop once per session with a note telling the agent to store the fact it just stated. This is the safety net that stops a decision or a gotcha from evaporating because nobody wrote it down.

- **Default:** absent, and the nudge is already on. With no file, THOR uses a built-in list of 15 trigger phrases in English and Dutch ("decision:", "we decided", "from now on", "gotcha:", "migrated to", "new project", and the Dutch equivalents). Since the `Stop` hook is installed by default, you get the nudge with zero setup.
- **Turn it on if:** the built-in list keeps firing on the way you normally write, or you work in a language it does not cover and durable facts are slipping through unstored.
- **Leave it off if:** you have no concrete complaint. Writing this file **replaces** the built-in list rather than adding to it, so a short custom list silently throws away all 15 shipped triggers. A trigger that is too broad costs one forced extra turn per session; one that is too narrow disables the nudge without telling you.
- **What it costs:** no process, no port, no download, no larger binary. The code path is compiled in whether or not the file exists. Runtime cost is one file read plus substring scans per finished turn, and the repo states no measurement for it.
- **How to turn it on:** create the file next to `thor.db` with a JSON array of strings.

  Windows (PowerShell):

  ```powershell
  [System.IO.File]::WriteAllText("$env:LOCALAPPDATA\thor\guard-capture-triggers.json", '["decision:","gotcha:","my own trigger"]')
  ```

  Linux/macOS:

  ```sh
  printf '["decision:","gotcha:","my own trigger"]' > "${XDG_DATA_HOME:-$HOME/.local/share}/thor/guard-capture-triggers.json"
  ```

  Three things to know while writing it. Case does not matter: every entry is trimmed and lowercased before use, and so is the message. But internal spacing does matter in one direction: the message has runs of spaces collapsed to a single space while your trigger is only trimmed at the ends, so a trigger containing two spaces in a row can never match. Write single spaces.

  Third, and this one bites hardest on Windows: save the file without a byte order mark. That is why the PowerShell line above calls `[System.IO.File]::WriteAllText` instead of the more obvious `Set-Content -Encoding utf8`. In Windows PowerShell 5.1 that switch puts three extra bytes at the front of the file, the JSON parser rejects the whole thing, and THOR quietly falls back to the built-in list. You get no error and no warning - your triggers simply never fire. If you write or edit the file in an editor, save it as "UTF-8" and not "UTF-8 with BOM".

  One more detail about where the file goes. This file is read from the folder holding the store file, while the two rulebooks above are read from THOR's per-user data folder. With a default install those are the same folder. If you have pointed THOR at a store somewhere else, put this file next to that store.
- **How to check it worked:** send a fake Stop payload containing one of your triggers, and use a session id you have not probed with before, because the nudge is claimed once per session and will stay silent on a repeat:

  ```sh
  echo '{"session_id":"probe-2","last_assistant_message":"my own trigger"}' | thor stop-guard
  ```

  It should print `{"decision":"block","reason":"[THOR capture] This reply looks like it contains a durable decision, gotcha, or milestone ..."}`. Nothing printed means no trigger matched - or that this session id already used its one nudge.
- **How to turn it off again:** delete the file and the built-in list comes back. An empty array or invalid JSON also falls back to the built-in list, so you cannot use this file to switch the nudge off. To silence the nudge itself you need `THOR-SILENT.flag` next to `thor.db`, or you must remove the `Stop` hook from `settings.json`.

### --rulebook &lt;path&gt; (on thor guard and thor stop-guard)

An option on the two hook commands that says "read this file instead of the usual one". `thor guard --rulebook <path>` changes which file holds the command rules; `thor stop-guard --rulebook <path>` changes which file holds the response rules.

- **Default:** not passed. `thor install` writes each hook command as the quoted absolute path of the `thor` executable followed by the subcommand and nothing else, so each command falls back to its own fixed path in the per-user folder. For `thor guard` there is a second layer of default: without `--with-guard` that hook is not installed at all, so there is no command to add the option to.
- **Turn it on if:** your risk rules genuinely differ per project (a different deploy route, different container names) and you want a project's own `.claude/settings.json` to point at its own rulebook - which is the scoping the code recommends for the command guard. Also useful to try out an edited rulebook before you promote it to the default path.
- **Leave it off if:** you have one machine and one rulebook. The default path already works, and one file is one thing to maintain. And never point it at a file inside a repo you cloned from someone else: the fixed default path exists precisely because a project directory could plant a rulebook, and every `reminder` string in it goes straight into your agent's context.
- **What it costs:** nothing the code establishes beyond changing which path is opened. It starts no process, opens no port, downloads nothing.
- **How to turn it on:** there is no command for this; you edit the hook entry in the relevant `settings.json` by hand. Open the file, find THOR's entry, and append the option to the command that is already there. Keep the quoted absolute path to the executable exactly as the installer wrote it - if you replace it with a bare `thor`, the hook starts depending on `thor` being on the `PATH` of whatever process runs it.

  ```json
  {
    "hooks": {
      "PreToolUse": [
        {
          "matcher": "*",
          "hooks": [
            {
              "type": "command",
              "command": "\"<absolute-path-to-thor-executable>\" guard --rulebook \"<absolute-path-to-your-rulebook.json>\""
            }
          ]
        }
      ]
    }
  }
  ```

  The same edit applies to the `Stop` entry with `stop-guard`. Use absolute paths for the rulebook too; a relative path would be resolved against whatever directory the agent happens to be in.
- **How to check it worked:** run the same probe as above, with the option added, and confirm you get a reminder from the file you named:

  ```sh
  echo '{"tool_name":"Bash","tool_input":{"command":"git commit --no-verify"}}' | thor guard --rulebook "<absolute-path-to-your-rulebook.json>"
  ```

  A matching rule prints the `[THOR guard] ` advisory line. To be sure the option is really what did it, run the same probe without it and check that the default rulebook answers differently.
- **How to turn it off again:** remove the option from the hook command in `settings.json`. The guard goes back to the default path in the per-user folder. Nothing is lost - the option only chooses which file is read.

## Projects and scoping

THOR keeps every project in one store, but it only shows you one project at a time: a search inside project A does not surface project B's code or notes. This section is about deciding what THOR calls your project, what it indexes, and what to do when a note ends up filed under the wrong one.

A word you will meet all the way through: the **project key**. It is just a name, a short piece of text such as `my-app`. Every stored item carries it, and searching only returns items whose key matches the folder you are working in, plus the items in the shared **global tier** (the "belongs everywhere" pile). If you never do anything from this section, THOR takes the key from the name of your git repository's top folder.

### thor init (and the .thor marker file)

`thor init` writes a small text file named `.thor` at the top of your project folder. That file holds one line: the project key. It then indexes the project's files straight away, in the same command.

- **Default:** no `.thor` file exists. THOR derives the key from the name of the git repository's top folder. A folder that is neither a git repository nor marked has no project at all for search purposes, so a search there only sees the global tier.
- **Turn it on if:** it is a repository you actually work in with an agent, and especially when the folder name on disk is not the name you want the project to have: a backup copy, a second checkout, a git worktree. The marker keeps the key stable when the folder is renamed, copied or checked out elsewhere, so everything indexed earlier keeps matching.
- **Leave it off if:** it is a scratch folder or a throwaway clone. With no marker the project stays "unknown", so the optional SessionStart hook never starts a background re-index for it. Also note that `thor init` is not marker-only: it runs a full index of the folder in the same command. Do not run it in a folder you do not want indexed. If you only want the key and not the indexing, write the key into a file named `.thor` yourself.
- **What it costs:** no network port, no download, no extra long-running process, no bigger binary. The cost is the indexing that `thor init` performs in the same command: it runs in the foreground and blocks until it finishes, and it grows the store (every text file becomes chunks of at most 1800 characters). On a semantic build the same run also tops up the dense vector sidecar (`thor-vectors.db`, a second database file next to the store) by embedding the events it just wrote. One thing it does **not** do: `thor init` does not refresh the symbol sidecar (`thor-symbols.db`, the file the `where_used` and `impact` tools read). Only `thor ingest` and `thor symbols` rebuild that, so run `thor symbols` once after your first `thor init` if you want those tools to work straight away. The repo states no timing measurement for any of this.
- **How to turn it on:**

```
cd <project-folder>
thor init
```

  To choose the key yourself instead of taking the folder name:

```
thor init --key <project-key>
```

- **How to check it worked:** the command prints `wrote <path>/.thor (project key '<key>')`. After that, `thor recall "<something>"` tags every hit with `[proj:<key>]` or `[global]`.
- **How to turn it off again:** delete the `.thor` file. The key falls back to the git repository's top folder name. This is **not** lossless if the marker key was different from that folder name: everything already stored under the old key keeps the old key and drops out of scope. Notes can be moved with `thor reproject` (below), but indexed code chunks are not meant to be - `thor reproject` skips chunk-shaped ids unless you add `--force`, because indexing owns them, and a forced move is overwritten by the next index run anyway. The clean fix for chunks is to index the folder again under the new key.
- **Before your first run, if you share one store across machines:** SETUP.md states that an old binary cannot read a store containing the `fact_reprojected` event, so upgrade every machine that shares the store (PC, sync replica, restore host) to this build before the first `init`, `reproject` or `backfill-projects`.

### thor ingest

Reads the text files in a folder and stores each piece as a searchable chunk, so the agent can find your actual source and docs, not only notes someone typed in.

- **Default:** off. Nothing is indexed until you run `thor ingest` yourself, or run `thor init` (which indexes as part of the same command), or install the optional SessionStart hook, which re-indexes an already-marked project in the background.
- **Turn it on if:** you want the agent to find code and docs by meaning or keyword across the whole project. It is also what the `where_used` and `impact` tools read: without indexed code chunks those tools have nothing to work with.
- **Leave it off if:** you do not want the folder's contents copied into the store. Every indexed text file becomes chunks in an append-only log, so they stay there, and they compete with your notes for the limited number of results a search returns. Be especially careful on a folder that is **not** a git repository: there is no `.gitignore` to lean on, so a plain-text secret sitting in a loose (non-dot) file directly in the folder would be read. Point it at docs, not at a home directory.
- **What it costs:** no network, no port, no download, no long-running process, no change in binary size. The real costs are: store growth (chunks of at most 1800 characters per file; a file over 200,000 characters is always truncated and partly indexed, never skipped), the foreground run time, a rebuild of the derived symbol sidecar (`thor-symbols.db`) on every foreground run, and, on a semantic build, a top-up of the dense vector sidecar (`thor-vectors.db`) that embeds every event the run just wrote. The repo states no timing measurement for this.
- **A trap worth knowing:** in a git repository the file list comes from `git ls-files`, so only tracked files are read and ignored files are never touched. But if the `git` command fails or is not on your PATH, that list comes back **empty** and the run indexes nothing while still looking like it succeeded. If a run reports zero files in a git repository, check that `git` works there first.
- **How to turn it on:**

```
thor ingest .
```

  To run it in the background and get your prompt back at once:

```
thor ingest <path> --detach
```

- **How to check it worked:** the foreground run prints a summary line, `ingest: <n> created, <n> revised, <n> unchanged, <n> retracted (<n> files; skipped <n> binary, <n> truncated)`. Then search for a phrase you know is in one of the files: `thor recall "<a phrase from a file>"`. The `--detach` run prints nothing at all (its output is discarded), so use the foreground form when you want to verify.
- **How to turn it off again:** stop running it, and remove the `.thor` marker so the SessionStart hook does not refresh it. There is no un-index or purge command: chunks are only withdrawn when you index the folder again and the file has vanished from it.

### thor ingest --global

Indexes a folder into the reserved `@global` tier instead of a project, so those files surface in every project, whichever folder you are working in.

- **Default:** off. Without the flag, an indexed folder gets a project key derived from the folder itself.
- **Turn it on if:** you keep a small folder of cross-cutting documents that you want present in every session: house rules, conventions, a dev-loop description.
- **Leave it off if:** the content is source code or anything specific to one project. A global chunk competes for a search result slot in **every** project, so a large global tier is a permanent drag on every project's results. Keep it small.
- **What it costs:** no new process, no port, no download, no extra binary size. It runs exactly the same code path as a normal index, only with the key forced. The costs are the same as `thor ingest`: store growth and foreground run time. The extra, real cost is the crowding described above; the repo states no measurement of how much a large global tier degrades results.
- **A trap worth knowing:** the global tier is reconciled as if it were a single project. When you run `thor ingest --global` on folder B, everything already stored in the global tier from folder A counts as "vanished" and is withdrawn. Keep all your global documents in one folder, and always point the command at that one folder.
- **How to turn it on:**

```
thor ingest --global <docs-dir>
```

- **How to check it worked:** the run prints `ingest [global]: ...`, and the stored ids start with `@global:`. In `thor recall` output those hits are tagged `[global]`.
- **How to turn it off again:** indexing the same folder **without** the flag does not remove the global copies - the two live under different keys. To remove them, **empty** the folder (keep the folder itself) and run `thor ingest --global <same-dir>` once more: the chunks whose files are gone are then withdrawn. Deleting the folder outright does not work - the run prints `thor ingest: skip (not a directory)` and withdraws nothing. Nothing is erased from the log, and indexing the folder again restores them.

### thor ingest --project &lt;key&gt;

Forces one chosen project key for every chunk of that one run, instead of deriving the key from the folder.

- **Default:** off. The key comes from the `.thor` marker if there is one, otherwise the git repository's top folder name, otherwise the folder's own name.
- **Turn it on if:** you are indexing a copy or mirror of a project whose folder name does not match the project key you use in your normal session (a backup copy, a source export, a network share). It is also the way to carve one subdirectory of a large repository into a project of its own: when the path you give is a strict subdirectory of a git repository and you pass a key, only that subtree is indexed.
- **Leave it off if:** the derived key is already right. Adding `--project` to a folder that already has the correct key just splits the same material across two keys. If you want the key to stick permanently, use a `.thor` marker instead - this flag applies to one command only.
- **What it costs:** nothing at runtime beyond the normal index run: no process, no port, no download, no extra binary size. The lasting cost is duplication if you use it inconsistently, since chunks written under a pinned key keep that key forever.
- **How to turn it on:**

```
thor ingest --project <key> <path>
```

- **How to check it worked:** the run prints `ingest [<key>]: ...`, and `thor recall --project <key> "<phrase>"` returns those hits. This check does not work with `--detach`, which prints nothing.
- **How to turn it off again:** just leave the flag off next time. Chunks already written under the pinned key keep it - a later run without the flag creates a second set under the derived key rather than moving the first set.

### thor recall --all-projects and thor recall --project &lt;key&gt; (MCP: all_projects / project)

Two per-search escape hatches. `--all-projects` switches scoping off for that one search; `--project <key>` searches a named project (plus the global tier) instead of the one your working directory implies.

- **Default:** off. Every search from the command line, from the MCP server started in a project folder, and from the courier is scoped to the current project plus the global tier.
- **Turn it on if:** you are deliberately looking across repositories ("where did I solve this before?"), or auditing what another project knows, without changing directory or moving anything.
- **Leave it off if:** you would be using it as your normal search. The isolation is the point: an unscoped search pulls every other project's material into the ranking and pushes the current project's own results down.
- **What it costs:** nothing measurable at the infrastructure level: no process, no port, no download, no extra binary size, and no second query - scoping is a filter over the same result list. One thing does happen as a side effect: a search through the MCP server records an access count for each hit it returns, which feeds ranking. So an unscoped MCP search touches the store; the command-line search does not.
- **How to turn it on:**

```
thor recall --all-projects "<query>"
thor recall --project <key> "<query>"
```

  Through the MCP server (the interface your agent uses), pass `all_projects: true` or `project: "<key>"` on the recall tool. `all_projects` wins if both are given.

- **How to check it worked:** each hit on the command line is prefixed `[proj:<key>]` or `[global]`, so results from other projects are visible as such.
- **How to turn it off again:** leave the argument off. It is a per-search argument; no setting is stored.

### the project argument on the MCP remember tool

When your agent stores a new note through THOR's MCP tools, it can say which project that one note belongs to, instead of inheriting the project of the folder the server started in.

- **Default:** omitted. The note inherits whatever project the MCP server itself carries. For a server started as a normal local process (stdio), that is the project of its working directory: the `.thor` marker if present, else the git repository's top folder name. For a server reachable over HTTP, there is **no** inherited project, so a note written there lands in the global tier unless the caller passes a project.
- **Turn it on if:** the note clearly does not belong to the current folder's project, or it is a standing rule that must be visible everywhere (pass the literal value `global`).
- **Leave it off if:** it is routine capture. The inherited project is normally right, and hand-forcing `global` is the main way notes end up too broad - which is exactly what the scope review below then has to clean up.
- **What it costs:** nothing. It is an optional field on a tool that is already there: no extra process, no port, no download, no bigger binary.
- **How to turn it on:** call the MCP remember tool with `project: "<key>"`, or `project: "global"`. The command line has no equivalent flag on write.
- **How to check it worked:** the stored id shows the scope. `<key>:mem-...` is scoped to a project; an id starting with `mcp-` is global. Search from another project to confirm a global note shows up there too. Note that if the caller also supplies its own `entity_id`, that id is used exactly as given and the project argument has no effect on it.
- **How to turn it off again:** omit the argument. A note already written under the wrong scope is fixed with `thor reproject`, not by writing it again.

### thor reproject &lt;id&gt; --project &lt;key&gt; | --global

Moves one already-stored note to a different project, or to the global tier. It does not edit or delete anything: it appends a "this now belongs to X" record, which is why a synchronised copy of the store agrees after it syncs.

- **Default:** off, and it never happens by itself. A note's project is decided at birth by its id, and only a `reproject` changes it (the most recent one wins).
- **Turn it on if:** you notice a note under the wrong project - typically one of the ones `thor review-scope` lists, or a note stored in a session that had no project signal.
- **Leave it off if:** the scoping is fine. Do not use it to move indexed code chunks: those belong to `thor ingest`, which is why the command skips chunk-shaped ids unless you pass `--force`, and a forced move can simply be overwritten by the next index run. Do not bulk-move notes you have not read either; the scope decision is a judgement call.
- **What it costs:** very little: one small record appended to the store per accepted id, permanently. No process, no port, no download, no bigger binary. As with `init`, every machine sharing the store must already run a build that understands this record - see the ordering note under `thor init`.
- **A trap worth knowing with `--batch`:** it reads ids from standard input, one per line. A file with Windows line endings once produced 153 stray records against ids that never existed, because of the trailing carriage return. The code now trims it, but read the ids you feed it.
- **How to turn it on:**

```
thor reproject <entity-id> --project <key>
thor reproject <entity-id> --global
cat ids.txt | thor reproject --batch --project <key>
```

- **How to check it worked:** it prints `reprojected <n> entit(y|ies) to <target>`; an id it does not recognise prints `skip unknown entity`. Then search in the target project and confirm the note appears. Do not expect it to disappear from the old project when you move it to global: the global tier surfaces everywhere by design, including where it came from.
- **How to turn it off again:** move it back the same way, which appends another record. Nothing is ever removed. `thor history <id>` shows **that** a move happened, but not where to - the target is inside the record body, not in the history listing.

### thor review-scope

Lists notes that landed in the global tier with no project signal since you last reviewed, so you can decide which ones actually belong to a project. With `--mark` it records that you have reviewed everything up to now.

- **Default:** off as a command: nothing runs it for you. If the optional SessionStart hook is installed, it can nudge the agent (at most once a day) to run it and propose moves for your confirmation. Nothing moves without your approval.
- **Turn it on if:** you sometimes store notes from outside a project folder - a remote session, or a session with no working directory - and want to catch them before they pile up in the global tier and crowd every project's results.
- **Leave it off if:** you always work inside a project folder. Skipping it costs nothing structural; it only means notes that are too broadly scoped stay that way.
- **What it costs:** nothing measurable: no process, no port, no download, no network, no binary-size difference (the code is compiled in either way). One correction to the obvious assumption: the listing is not purely read-only. Opening the store applies its normal startup work (journal mode, table creation if missing, search-index sync), so it can write to the store files even when it lists nothing.
- **What it will and will not list:** only notes that were born in the global tier, that were never moved, that arrived after the last review mark, **and** that carry no `| project: ...` footer. THOR's own typed notes always write that footer, so a note your agent stored with a type or tags is treated as already attributed and never shows up here. In practice the list is narrow.
- **How to turn it on:**

```
thor review-scope          # list the candidates
thor review-scope --mark   # record that you reviewed up to now
```

- **How to check it worked:** with nothing to review it prints `no global memories to review (all attributed, or none new since the last review).`. With candidates it prints a count line, then one indented line per note (`  <id> (seq <n>): <first line>`), then the suggested next commands. `--mark` prints `scope review marked done up to seq <n>`.
- **How to turn it off again:** simply do not run it, and ignore the nudge. Nothing is lost: the listing changes no notes, and `--mark` only moves a small bookmark file (`thor-review.json`, kept next to the store). Be aware that `--mark` also restarts the once-a-day nudge timer.

### thor backfill-projects

A one-time cleanup for a store that was seeded from an older memory tool. It reads the `| project: <name> |` footer that those imported notes carry and plans to move each one back to the project it names.

- **Default:** off, and preview-only. It never runs by itself, and without `--apply` it only prints what it would do.
- **Turn it on if:** you have just imported a store from a previous memory tool whose notes carry that footer, and they are all sitting in the global tier where they surface in every project. Run it once.
- **Leave it off if:** your store was never seeded from such an import. There will be nothing to attribute and it will say so. Do not run `--apply` before reading the preview: it is a bulk scope change.
- **What it costs:** no process, no port, no download, no added delay, no bigger binary. Two real costs: it appends one move record per note, permanently, and every machine that shares the store must already run a build that understands that record - the same ordering note as under `thor init`. Note also that it does not check where a note came from: any global note whose text contains a `| project: <name> |` footer field qualifies, imported or not.
- **How to turn it on:**

```
thor backfill-projects           # preview, changes nothing
thor backfill-projects --apply   # write the moves
```

- **How to check it worked:** with nothing to do it prints `backfill: nothing to attribute (no footers with a non-global project).` and stops. Otherwise the preview prints `backfill: <n> memor(y|ies) to reproject:` followed by two-space indented `  <id> -> <project>` lines and a reminder that it was a dry run; `--apply` prints `backfill: applied <n> reprojection(s).`.
- **How to turn it off again:** leave `--apply` off - preview is the default. Moves that were already applied are undone one at a time with `thor reproject <id> --global`. Nothing is deleted from the log either way.

## Keeping the memory healthy

Everything in this section is a command you type yourself. None of it runs on a
schedule, none of it runs behind your back, and skipping all of it leaves THOR
working exactly as it does today. Use this section to decide which of them are
worth putting on your own calendar, and which you will only ever need after
something went wrong.

Two words come up throughout, so here they are once, in plain language:

- **The store** is the single file THOR writes your memory to. It lives in the
  per-user THOR home: `%LOCALAPPDATA%\thor\` on Windows, `$XDG_DATA_HOME/thor/`
  elsewhere, and `~/.local/share/thor/` when `XDG_DATA_HOME` is not set.
- **A sidecar** is a second, separate file next to the store, built entirely
  from what is already in the store. It is derived data. Deleting a sidecar
  never loses a fact, because the fact is in the store, not in the sidecar.

### thor doctor

Prints one line per part of THOR, saying whether that part is present or
missing. It is the "is anything obviously wrong" command.

- **Default:** nothing ever runs it. It only happens when you type it.
- **Turn it on if:** you just installed or upgraded the binary, recall behaves
  in a way you did not expect, or you are about to file a bug report. The repo
  calls it the first thing to run after installing a binary and the first thing
  to paste into a bug report.
- **Leave it off if:** there is no real reason to avoid it, but it is not a
  free command on a very large store - see the costs below.
- **What it costs:** no extra process, no network port, no download, and no
  change to the binary (the command is compiled into every build whether you
  use it or not). What it does cost when you run it: it opens the store and
  counts every single event, so it is not instant on a large store. The repo
  states no timing measurement for that.

  **This changed:** `thor doctor` used to open the store the same way a write
  command does, which meant it CREATED an empty store when the path was wrong.
  A typo in `--db` then reported `store: OK (0 events ...)`, which reads like a
  healthy THOR that has forgotten everything - the worst possible answer to "why
  is my memory not coming back". It now refuses instead:

  ```
  store: UNREACHABLE (no THOR store at <path> - this command never creates one;
  check the path (--db) or store your first memory to create it)
  ```

  On a machine where you have not stored anything yet, that line is correct
  rather than a fault. `thor init` in a project folder, or your first
  `remember`, creates the store. The same change applies to `thor fsck` and
  `thor status`.
- **How to turn it on:**

  ```sh
  thor doctor
  ```

- **How to check it worked:** the output is the whole point. It looks like this:

  ```
  store: OK (12345 events at <store path>)
  semantic model: present (<model folder>)
  vectors sidecar: present
  symbols sidecar: absent (run `thor symbols`; where_used/impact and the symbol recall bonus stay off)
  injection daemon: COLD (hook falls back to the in-process path; run `thor daemon` or install with --with-daemon to warm it)
  flag: THOR-PRIMARY.flag present
  ```

  On a binary built without the semantic feature the model and vectors lines are
  replaced by a single `semantic: not built in (bm25-only binary)`. The flag
  lines only appear for flag files that actually exist.
- **How to turn it off again:** stop typing it. Nothing is lost.

### thor fsck

Checks that the stored log has not been altered. THOR's log is hash-chained:
every entry carries a fingerprint of the entry before it, so changing any past
entry breaks the chain from that point on. `fsck` recomputes the whole chain and
tells you whether it still adds up.

- **Default:** never runs on its own. There is no hook, no timer, no installer
  step that calls it.
- **Turn it on if:** you just restored from a backup, moved the store to another
  machine, had a crash or an unclean shutdown, had a log-shipping incident, or
  you are about to cut a release. Also worth running now and then on a store you
  care about.
- **Leave it off if:** you want it on a per-prompt or per-session hook. It reads
  and folds the entire log every time and there is no incremental mode, so on a
  large store this is a full scan for every run.
- **What it costs:** no extra process, no port, no download, no change in binary
  size. Per run: one full pass over the log, plus a second full derivation of
  the current set of facts. It appends nothing to the log, and it opens the store
  without the schema and search-index work an ordinary command does - which is the
  point: otherwise it would repair the search index during its own open and then
  report that index as healthy. Measured once, on one machine: 0.5 s over a store
  of 19,157 events on an NVMe SSD. Treat that as an order of magnitude, not a
  promise - it is a full scan, so expect it to grow with the log.
- **How to turn it on:**

  ```sh
  thor fsck
  ```

- **How to check it worked:** a healthy store prints exactly six OK lines and a
  summary, and the command exits 0:

  ```
  Chain integrity: OK
  Fork detection: OK
  Differential auditor: OK
  FTS projection: OK
  FTS integrity: OK
  Footer integrity: OK
  fsck: all checks passed
  ```

  One honest note on that output: the five integrity checks stop at the first
  failure, so if one fails you get its error line and nothing after it. A short
  output means "stopped early", not "fewer things to check".

  **This changed (three things):**

  The `FTS projection` line used to be weaker than it looked, because opening the
  store repaired a row-count mismatch moments before the check ran, so it could
  only ever report OK. `fsck` now opens the store without that repair, so the
  line means what it says. If it reports a mismatch, any ordinary command that
  writes (a `remember`, an ingest) rebuilds the index on its next open - you do
  not have to repair it by hand.

  `FTS integrity` is new, and it exists because the projection line still could
  not see the failure that actually hurts you. The projection check compares row
  counts: how many facts are in the log versus how many are in the search index.
  That catches a missing fact. It cannot catch a fact that is present but whose
  index entry is damaged - a torn write, a bad sector, a half-finished copy. The
  counts still match, the check still says OK, and searches quietly stop finding
  things. This new line asks SQLite's own search engine to verify its index
  structure, which is the only check that sees that damage. A repair is a single
  command, and nothing can be lost by running it, because the index is derived
  from the log rather than stored in it:

  ```sh
  thor fsck --rebuild-fts
  ```

  Third: **`fsck` now exits 1 when an integrity check fails.** It used to print
  `CHAIN INTEGRITY ERROR` in red and then exit 0, which meant no script, no
  scheduled job and no release step could ever act on it - a backup verifier
  would have reported success on a corrupt store. A footer defect still exits 0
  (see below), so the exit code means exactly one thing: something is corrupt.

  The sixth line is content health, not log integrity. Some facts carry a
  metadata footer (the bracketed tail that records their type and tags). If a
  footer got lost, `fsck` names the facts and ends with
  `fsck: integrity checks passed; N fact(s) need a footer repair (see above)`.
  A wiped footer never fails the run and never changes the exit code - nothing is
  corrupt, it just needs repairing.
- **How to turn it off again:** stop running it. Nothing is lost.

### thor symbols

Builds a sidecar named `thor-symbols.db` next to the store, recording which
names each stored code chunk defines and which names it calls. It reads the code
already inside the store, not your working directory.

- **Default:** absent on a fresh store, then built for you. Every ingest refreshes
  it, and that includes the ingest `thor init` runs, so setting a project up the
  way SETUP.md describes already gives you a sidecar. If you installed THOR's
  SessionStart hook (`thor install --with-courier`), that hook starts an ingest by
  itself for any folder carrying a `.thor` marker file, and that ingest refreshes
  the sidecar with no action from you.

  **This changed:** older builds refreshed the sidecar only from `thor ingest`, so
  a project set up with `thor init` was left without one and `where_used` and
  `impact` answered on nothing. If you set a project up that way before, run
  `thor symbols` once to catch up.

  Two cases where you still run it by hand: a store that was filled some other way
  than by indexing (a replica that received its events over log shipping never
  ingests anything), and after you delete the sidecar yourself.
- **Turn it on if:** you have ingested source code and want the `where_used` and
  `impact` tools (who calls this symbol, what does changing it touch). It also
  gives a small ranking bonus in deliberate recall to code chunks that define
  the symbol you asked about. Also run it after deleting the sidecar.
- **Leave it off if:** you only store prose memories and notes. There is nothing
  in the store for it to extract, and `where_used` and `impact` are not
  questions you would ask.
- **What it costs:** no extra process, no port, no model download, no change in
  binary size (the code is compiled in unconditionally). The cost is at rebuild
  time: one full pass over every stored code chunk, plus a second SQLite file on
  disk. The repo states no measurement of how long that takes or how large the
  file gets.
- **How to turn it on:**

  ```sh
  thor symbols
  ```

- **How to check it worked:** it prints
  `symbols rebuilt: N source chunks -> N definitions, N call edges (<path>)`.
  Read those counts, not `thor doctor`: doctor's `symbols sidecar: present` only
  means the file exists, and the file is created the moment anything opens it,
  so a rebuild that found nothing still reports `present`. A count of zero
  definitions means there is no code in the store to extract from.
- **How to turn it off again:** delete `thor-symbols.db` next to the store.
  Nothing is lost. The sidecar sits outside the hash-chained log by design, so
  `fsck`, `export`, log shipping and the auditors never look at it, and
  `thor symbols` rebuilds it from the store whenever you want it back. What you
  lose until then: `where_used`, `impact`, and that ranking bonus.

### thor consolidate

Prints a report about the state of your memory: near-identical facts, notes that
look forgotten, and groups of facts on the same topic. It only reports. Nothing
is changed unless you add the flag described in the next block.

- **Default:** never runs on its own, and it is report-only.
- **Turn it on if:** you want a periodic hygiene pass. The rubric THOR itself
  ships suggests roughly every 2000 store events, or after a heavy burst of
  writing. It also works as a CI gate on a shared store, because it exits with
  code 1 whenever anything needs digesting and 0 when clean.
- **Leave it off if:** your store is young or small - there is nothing to find
  yet. Never wire it onto a per-prompt or per-session path: it loads the whole
  log, computes a usage number for every live fact, and on a build with the
  semantic feature - and only when the vectors sidecar next to the store exists
  and was built by the same model - it compares every pair of fact vectors
  against each other.
- **What it costs:** no extra process, no port, no download, no change in binary
  size. Per run: one full pass over the log, plus that all-pairs comparison when
  the semantic build finds a usable vectors sidecar (without one the report
  says so: "cosine pass skipped: vectors sidecar unavailable - lexical bands
  only"). Without `--apply-dedup` it writes nothing to the hash-chained
  log, though it does read the local ledger sidecar (pins and usage counters),
  and opening the store can trigger the one-off search-index repair that any
  ordinary command does (`doctor`, `fsck` and `status` are the exceptions - they
  open without it). The repo states no timing measurement.
- **Honest limit:** the report is a worklist, not a cleanup. Only duplicate
  twins can ever be acted on mechanically, and only with the flag below.
  Everything else - the decay candidates, the topic clusters - is something you
  or an agent must confirm one item at a time. The repo states no measurement of
  what running it improves.
- **How to turn it on:**

  ```sh
  thor consolidate
  ```

- **How to check it worked:** a clean store prints

  ```
  THOR consolidate - metabolism report
  clean: nothing to digest
  ```

  and exits 0. Otherwise you get a section per finding, for example
  `N duplicate group(s) ...` with a `keep <id>  retract <id> ...` line each, and
  `N decay candidate(s) (untyped, never marked, never read, long inactive) - confirm each via retract:`,
  and the process exits 1.
- **How to turn it off again:** stop running it. Nothing is lost.

### thor consolidate --apply-dedup

The one part of the report THOR will act on for you. For each duplicate group it
keeps one copy and retracts the others.

- **Default:** off. Without the flag, `thor consolidate` cannot write a memory
  change at all.
- **Turn it on if:** you have an older store carrying byte-identical twins from
  before the duplicate-refusing gates existed, you have read the report, and you
  agree with every single `keep X retract Y` line it printed.
- **Leave it off if:** you have not read the report, or you have not made a
  backup. Duplicates are detected by a normalized 120-character prefix of the
  body. Two genuinely different facts that happen to open with the same 120
  characters - the same template header, the same `DECISION 2026-xx-xx:` opener,
  the same boilerplate lead-in - are treated as twins. Never wire this into an
  unattended job.
- **Which copy survives:** highest priority first - a pinned fact, then a copy
  carrying a source-store reference (the marker left by the one-time seeding
  import), then a typed fact, then one with positive usage strength, then the
  oldest. A pinned twin is never a retract target.
- **What it costs:** no extra process, no port, no download, no change in binary
  size. It appends one retract event per retracted twin. Those events are
  permanent entries in the log, like every other event.
- **How to turn it on:** back up first, read the report, then run it.

  ```sh
  thor export --out backup-before-dedup.jsonl
  thor consolidate
  thor consolidate --apply-dedup
  ```

- **How to check it worked:** it prints
  `retracted N duplicate twin(s), M skipped; re-run for the post-apply report`,
  plus a `skip <id>: ...` line for every group or target it declined (for
  example `skip <id>: pinned since the report was built`). Re-run
  `thor consolidate` to see the store after the change.
- **How to turn it off again:** omit the flag. To undo one individual retract,
  revise that entity with the original body restored, citing the retract as the
  parent revision - recall only skips an entity whose current revision is a
  retract. Nothing is deleted: the full history stays. Be aware that between the
  retract and the undo, `thor get <id>` shows the tombstone body rather than the
  original text, which the history still holds.

### thor consolidate --min-age-events &lt;N&gt;

Sets how old a note must be before the report is allowed to call it a decay
candidate. Age is counted in events behind the newest entry in the log, not in
days, because the hash-chained log carries no clock.

- **Default:** 2000 events. The repo states no rationale for that number, and no
  write rate to translate it into calendar time - only the mechanical meaning:
  2000 events behind the tip of your own log.
- **Turn it on if:** the default gives you an empty decay list, or an unusably
  long one, for how fast your store grows. A larger number makes the pass more
  conservative.
- **Leave it off if:** you would be tempted to set it low and treat the output
  as a to-do list. With a small number the pass will propose recent notes that
  simply have not been used yet. Age is the only thing separating "never used
  yet" from "never going to be used".
- **What it costs:** nothing of its own - it is a number handed to a filter the
  report already runs. Two things it does affect beyond the printed text: a
  longer decay list also makes `thor consolidate` exit 1, so it changes a CI
  result; and it does not reach `thor steward`, where the same floor is fixed at
  2000 and cannot be changed from the command line.
- **How to turn it on:**

  ```sh
  thor consolidate --min-age-events 5000
  ```

- **How to check it worked:** every decay line prints its own distance, so you
  can see the floor take effect:
  `<entity id> (N events behind tip) | <first line of the fact>`.
- **How to turn it off again:** omit the flag to go back to 2000. Nothing
  persists - the value applies to that one invocation only.

### thor steward

Writes the consolidate report, with a fixed review rubric in front of it, to a
markdown file next to the store. The file is meant to be opened in an agent
session that has THOR's MCP tools, so every keep/retype/retract decision lands
as a normal, reversible event in the log.

- **Default:** never runs on its own. It exists only as a command you type.
- **Turn it on if:** the consolidate report has grown past what you want to read
  in a terminal, or you are about to hand store maintenance to an agent session.
- **Leave it off if:** you are going to work the report directly. It adds a file
  and nothing else. Note that every run writes a new file named after the store
  tip, and nothing ever prunes them, so repeated runs accumulate.
- **What it costs:** no extra process, no port, no download, no change in binary
  size. One full pass over the log, the same as `thor consolidate`. It appends
  nothing to the hash-chained log. It does create a `steward` folder next to the
  store and write `steward-<tip seq>.md` into it - so the line it prints,
  "no writes were made", is true about the log and not about the filesystem. The
  decay floor here is fixed at 2000 events; `--min-age-events` does not apply.
- **How to turn it on:**

  ```sh
  thor steward
  ```

- **How to check it worked:** it ends with

  ```
  steward review prepared: <path>
  (open it in an agent session with the THOR MCP tools; no writes were made)
  ```

  Open that path and you should see the rubric followed by the consolidate
  report.
- **How to turn it off again:** stop running it, and delete the generated
  `steward-*.md` files. They are plain markdown outside the store, so deleting
  them cannot lose a fact.

### thor pin &lt;id&gt; / thor unpin &lt;id&gt;

Marks a fact as a standing rule. Pinned facts are re-injected into the agent at
every session start, and right after a compaction (a compaction is when the
conversation so far gets summarized away to free up room - at that moment the
agent has lost the details, and ordinary recall has nothing to match on, because
a follow-up like "carry on" shares no words with the rule).

- **Default:** nothing is pinned. `thor install` writes no pins.
- **Turn it on if:** you have hard standing constraints an agent must never lose
  track of - a deploy rule, a naming convention, a safety rule. Pinning also
  protects a fact from the hygiene passes: a pinned fact is never a decay
  candidate and never a dedup retract target.
- **Leave it off if:** the fact is merely useful rather than governing. Every
  pinned line is injected into every session, so pins are a permanent context
  tax. The cap is 8 lines total, and pins past the cap are silently skipped, so
  an over-pinned list quietly drops its own tail without telling you.
- **What it costs:** no extra process, no port, no download, no change in binary
  size. All your pins live in one row of a local SQLite sidecar next to the store
  (`thor-ledger.db`). The injected text is up to 400 characters per pin, with
  whitespace collapsed and the metadata footer stripped, cut off with "..."
  past that; at most 8 lines in total across all pins.
- **Two things to know before you rely on it:** the injected block is
  project-scoped, so a fact pinned in another project does not appear in this
  session's block, only this project's pins and the global ones. And pins are
  local: they are not part of the hash-chained log, so they do not travel with
  log shipping and are not included in a `thor export` backup. Re-pin by hand on
  a second machine. The repo states the re-injection benefit as design
  reasoning, not as a measurement.

  One more thing, if you go looking for the pins on disk: older builds of THOR
  printed a line in `thor pin --help` saying pins lived in a file called
  `thor-pins.json`. That was wrong, and the help no longer says it - your pins
  have always been in `thor-ledger.db`. If a `thor-pins.json` really is sitting
  next to your store, it is a leftover from a THOR old enough to predate the
  ledger; run `thor pin --list` first, check that every rule you expect is in
  the list, and only then delete the file.
- **How to turn it on:** the automatic re-injection needs THOR's SessionStart
  hook, and the only installer flag that writes it is
  `thor install --with-courier` (plain `thor install` installs the Stop
  response guard and nothing else). The pin commands themselves work without
  any hook - only the automatic re-injection depends on it.

  ```sh
  thor pin <entity_id>
  thor pin --list
  ```

- **How to check it worked:** `thor pin <id>` prints
  `pinned <id> (N pin(s) total) - it now re-injects at every session start`, or
  `already pinned: <id>`. An unknown id is refused. `thor pin --list` prints
  every pin with a snippet, or `no pinned facts.` when the list is empty. Start
  a fresh session in that project and look for a `<thor-brief>` block near the
  top of the context.
- **How to turn it off again:**

  ```sh
  thor unpin <entity_id>
  ```

  It prints `unpinned <id>`, or `not pinned: <id>` if it was not on the list.
  Nothing is lost: unpinning edits a local list and never touches the fact
  itself.

### thor mark &lt;id&gt; (and --noise)

Records that a fact actually helped you, or that it kept showing up and only got
in the way. Both feed a single usage number, and THOR reads that one number in
the automatic per-prompt injection and throughout `thor consolidate`: the decay
list, the choice of which copy of a duplicate group survives, and the order of
the retro-tag work list.

- **Default:** off, and inert until used. Nothing marks anything by itself from
  the command line. Note that the same operation is also an MCP tool, and the
  MCP server instructions tell the agent to call it when an injected or recalled
  fact helped - so when you run THOR as an MCP server, marks do happen without
  you typing them.
- **How the number works:** usage strength is the sum of recency-weighted
  "helped" marks, plus half a point per local read of the fact (capped at four
  reads), minus one point per noise mark. A "helped" mark loses half its weight
  every 2000 events behind the newest log entry.
- **Turn it on if:** you want the difference between "old and genuinely dead"
  and "old but load-bearing" to be visible to the decay pass. One honest
  "helped" mark takes a fact off the decay candidate list regardless of its age,
  for as long as that mark has not decayed below the noise marks against it.
- **Leave it off if (especially --noise):** a noise mark is the only user action
  that actively pushes a fact toward decay, and it stacks - two noise marks
  against one fresh "helped" mark already put the number below zero. Marking
  noise on a fact that is merely off-topic for today's task, rather than
  genuinely useless, is how a good fact ends up on a decay list months later.
- **What it costs:** no extra process, no port, no download, no change in binary
  size. A "helped" mark appends one event to the hash-chained log, which means
  it is permanent and it travels with log shipping. A noise mark increments one
  counter in the local ledger sidecar and is not synced anywhere.
- **Do not expect a ranking boost:** the effect on the automatic injection is at
  most a single swap into the third slot, only when there were more candidates
  than slots and only within a small ranking margin. It is a tiebreaker, not a
  promotion.
- **How to turn it on:**

  ```sh
  thor mark <entity_id>            # this fact helped
  thor mark <entity_id> --noise    # this fact was noise here
  ```

- **How to check it worked:** a useful mark prints
  `marked <id> as useful (fact_echoed, seq N)` and the event then shows up in
  `thor history <id>`. A noise mark prints
  `marked <id> as noise (local ledger, not synced)`. The effect on decay is only
  visible through `thor consolidate`.
- **How to turn it off again:** stop marking. A "helped" mark cannot be removed:
  it is a permanent event in the hash-chained log and can only be outweighed by
  noise marks or aged out by the 2000-event half-life. A noise mark lives in the
  local ledger sidecar and the repo ships no command to reset it.

### expires: YYYY-MM-DD (on the MCP remember tool)

An optional field when a fact is written. After that date the fact stops being
returned by recall. It is not deleted - the log still holds it, and
`thor get`/`thor history` still show it.

- **Default:** absent. A fact written without the field never expires.
- **Turn it on if:** the fact has a genuine natural end date - a workaround for
  a known upstream bug, a temporary deploy freeze, a version pin with a planned
  removal.
- **Leave it off if:** the fact is durable in any way. The failure mode is
  silent and one-directional: the day after the date, the fact quietly stops
  appearing in every agent's context, with no warning anywhere, while still
  sitting in the log. There is no expiry sweep and no notice. Guessing the date
  too short costs you the fact.
- **One gap worth knowing:** the expiry check runs on the recall paths only. The
  guard's moment-of-action pass, which surfaces facts anchored to an exact file
  path or command, does not check the date - so an expired fact can still fire
  there.
- **What it costs:** no extra process, no port, no download, no change in binary
  size. It is a text field appended to the fact's metadata footer.
- **How to turn it on:** only through the MCP `remember` tool. There is no
  `thor remember` CLI subcommand.

  ```
  remember(body: "pin to v1.9 until the upstream fix lands",
           fact_type: "gotcha",
           expires: "2027-01-15")
  ```

  A malformed value is refused at write time with an explanation, but only on
  that MCP path. If you write the footer text yourself in a body passed to the
  CLI, nothing validates it, and a value that does not parse simply never
  expires anything.
- **How to check it worked:** `thor get <id>` shows the footer including
  `| expires: <date>` - and keeps showing it after the date passes, while
  `thor recall` no longer returns the fact. That difference is exactly the
  design: the filter is applied at read time, it never evicts anything.
- **How to turn it off again:** this is the part that trips people up. Revising
  the fact with a body that simply leaves the footer out does **not** remove the
  expiry. On every revision, if the new body brings no footer of its own, the
  previous version's entire footer is re-attached automatically - including
  `expires`. That default protects your tags and your fact type, so it stays.

  Step by step, the route that works:

  1. `thor get <id>` and look at the last line of the body: the bracketed
     footer, something like
     `[memory/gotcha | tags: deploy | expires: 2027-01-15 | project: acme]`.
     Do not copy the `Rev:` or `Kind:` lines that `get` prints around it - those
     are not part of the body.
  2. Write your new text, a blank line, then that same footer re-typed with the
     `| expires: ...` field left out. A footer you supply always wins over the
     carried one.
  3. Revise with that body.

  You no longer have to remember this by heart. If you revise without a footer
  and the fact you are revising has an expiry date, the MCP `revise` tool now
  says so in its reply:

  ```
  revised acme:mem-1234 -> rev 8f2c...
  note: this fact still expires on 2027-01-15. Your body carried no footer, so
  the previous one was kept, expiry included. To change or drop the date, send
  the body with the full footer re-typed (leave the expires field out to
  remove it).
  ```

  That note comes from the MCP tool. The `thor revise` command line does not
  print it. Nothing is lost either way: every revision stays in history.

### provenance: verified | inferred (and THOR_EXP_PROVENANCE)

An optional label on a fact recording **how it was learned**: `verified` means
something was actually checked - a test was run, a file or command output was
read, or you confirmed it - and `inferred` means it was reasoned out and never
checked. It is one more field on the `remember` tool, stored in the fact's
metadata footer.

On its own the label is just a note. Setting the environment variable
`THOR_EXP_PROVENANCE` switches on what it is for: when a fact marked `inferred`
comes back in auto-recall **and the current prompt is about its topic**, the
injected line gets a reminder attached telling the agent to check the source
before building on it.

- **Default:** no label on a fact unless the writer passes one, and the reminder
  is off. Without the variable set, the label is inert - stored, stripped for
  ranking, never shown.
- **Turn it on if:** your agents write facts you will later depend on, and you
  want the shaky ones to announce themselves rather than quietly harden into
  truth. It is most useful where cheap models do the writing. On a 20-scenario
  test, a weak model built on the stale belief 12 times out of 20 without the
  reminder and 5 times with it; a strong model got 1 wrong without it and none
  with it. Neither arm was made worse by it. Fair warning on that number: the
  scenario set is not part of this repository, so it is not something you can
  re-run here - the same caveat as the category numbers in BENCHMARKS.md.
- **Leave it off if:** you write your facts yourself, or nobody is labelling
  anything - a store with no `inferred` facts gives the reminder nothing to fire
  on, so it costs you a wasted setting rather than noise. It is also honest to
  say this is an experiment: the flag name says so.
- **What it costs:** one environment-variable read per prompt and one small text
  scan per served fact, both far below anything you could notice. No process, no
  port, no download. The reminder is appended to a line that was already being
  shown, so it never adds or removes a result and never changes what gets
  surfaced.
- **How to turn it on:**

  ```sh
  # Windows, for your user account (then restart your agent)
  setx THOR_EXP_PROVENANCE 1

  # Linux and macOS: export it in the environment that starts your agent
  export THOR_EXP_PROVENANCE=1
  ```

  Then label facts as you write them, through the MCP tool:

  ```
  remember(body: "the deploy watcher ticks every five minutes",
           fact_type: "gotcha",
           provenance: "verified")
  ```

- **How to check it worked:** ask your agent something that touches a fact you
  marked `inferred`. The `<thor-recall>` block should show that fact with
  `[provenance: inferred - not yet confirmed by a test or file read; new activity
  on this topic now - reconcile against the source before you rely on it]` at the
  end of its line. Facts with no label, or marked `verified`, look exactly as
  before.
- **How to turn it off again:** clear the variable and restart your agent. The
  labels already written stay in the footers and do nothing - they are stripped
  before ranking, so they cannot affect recall.

## Backup, restore and import

THOR keeps everything in one file: a store called `thor.db` in your per-user data
directory (on Windows that is `%LOCALAPPDATA%\thor\thor.db`). Nothing in this
section runs unless you run it - THOR never backs itself up on its own. This
section helps you decide how you want a second copy of that store: a file you
copy by hand, a git repo that gets it automatically, or nothing at all because
you already back up the whole machine.

One thing is worth reading before you pick: an export contains every fact you
ever stored, as plain readable text. Treat the exported file like the store
itself.

### thor export

Writes the entire memory out as one plain-text file: one JSON object per line,
in the order the events were recorded. This is the file `thor restore` reads
back, and the file the git backup pushes.

- **Default:** off. The command only runs when you type it - no hook and no
  other subcommand runs `thor export` for you. (`thor backup`, further down,
  writes the very same export itself as its first step, but that is a separate
  command you also have to ask for.)
- **Turn it on if:** you are about to upgrade THOR or do something risky, you
  want to move your memory to another machine, or you want to feed the file into
  your own backup routine.
- **Leave it off if:** you already back up the whole machine (the store is a
  single file, so a normal file backup covers it), or you have nowhere private
  to keep the output. The exported file holds every fact in readable plain text.
- **What it costs:** no extra process, no network port, no download, no new
  dependency, and no growth in the size of the `thor` binary - the code ships in
  every build whether you use it or not. What it does cost is one full read of
  the store per run, plus the disk space of the output file. THOR's own doc
  comment claims each daily export compresses to almost nothing in git because
  the log only grows; that is an argument, not a measurement, and the repo states
  no measurement for it.
- **How to turn it on:**

  ```
  thor export --out events.jsonl
  ```

  Without `--out` it writes to standard output instead, so you can redirect it
  yourself:

  ```
  thor export > events.jsonl
  ```

- **How to check it worked:** with `--out` it prints `exported N events to
  <path>` (on the error stream, so you still see it when you redirect output).
  Open the file: every line is one JSON object.
- **How to turn it off again:** stop running it, and delete the exported file
  when you no longer need it. Nothing is lost - export adds no events. Note that
  it is not strictly read-only on disk: THOR opens the store read-write to run
  it, which can create or update SQLite's own companion files (`thor.db-wal` and
  `thor.db-shm`) next to the store. Your memory content is unchanged.

### thor restore --from &lt;file&gt;

Rebuilds a store from a file that `thor export` produced. It replays every event
in order and recomputes each event's hash, checking that it matches the hash
recorded in the file. If a single one does not match, the restore stops with an
error - so a backup that cannot faithfully rebuild your memory tells you, instead
of quietly producing a different memory.

- **Default:** off in a normal local install - nothing in the CLI or the agent
  hooks calls it, and it refuses to run into a store that already has events. One
  exception: the container image shipped in the repo runs it automatically the
  first time it starts, if `/data/thor.db` does not exist yet and
  `/data/events.jsonl` does.
- **Turn it on if:** you lost or corrupted your store, you are setting up a new
  machine from an export, or you want to prove your backup actually restores
  (restore into a scratch path now and then, rather than finding out later).
- **Leave it off if:** you want to merge an export into a store you are already
  using. It cannot do that. It refuses any target that already contains events,
  and it is not a sync tool - use `thor ship` / `thor recv` to keep two machines
  in step.
- **What it costs:** one full replay of the log. Every event is written in its
  own database transaction, recomputing the hash chain and re-indexing the text
  as it goes, so a large store takes a while. No process stays running
  afterwards.
- **How to turn it on:** always aim `--db` at a new, empty path, never at your
  live store.

  ```
  thor --db <path-to-a-new-empty-store> restore --from events.jsonl
  ```

- **How to check it worked:** it prints `restored N events into <path> (every
  replay hash verified)`. Follow it with a consistency check:

  ```
  thor --db <path-to-the-new-store> fsck
  ```

- **How to turn it off again:** not applicable - it is a one-shot operation. To
  undo it, delete the store file you restored into. Be aware of one honest
  detail: there is no single all-or-nothing transaction around the whole replay.
  Each event is written first and its hash checked immediately after, so if a
  mismatch is found the command stops with everything up to that point already
  written. Do not try to continue from there - delete the half-built store file
  and start over from a good export.

### thor backup --repo &lt;path&gt; [--force]

Does the export for you and pushes it into a git repository: it writes the log to
`<repo>/thor/events.jsonl`, then commits and pushes. It runs the plain `git`
command as a subprocess, so `git` has to be on your PATH, and whatever
credentials git already has are what it uses. By itself it does one run and
exits; it only becomes automatic if you wire it into the agent's session-start
hook (below), and even then it does nothing if the last backup was less than 20
hours ago, unless you pass `--force`.

- **Default:** off. It never runs on its own. It only runs when you type it, or
  when you asked `thor install` to add it as a hook.
- **Turn it on if:** you already have a git clone of a private backup repository
  on this machine, git can already push to it without prompting you, and you want
  a hands-off versioned copy.
- **Leave it off if:** any of these are true. (a) The export is every fact in
  plain text, so the target repository must be private and you must trust wherever
  it is hosted - never point this at a public repo. (b) The remote and the branch
  are fixed in the code: it always runs against a remote named `origin` and a
  branch named `main`. A repo set up any other way will fail. (c) You already
  back up the store some other way - then this adds nothing.
- **What it costs:** no background process, no network port, no download, and no
  growth in the size of the `thor` binary. The real costs are the time of a full
  export plus a git pull, commit and push over the network - and, if you install
  it as a session-start hook, that whole round trip happens in the foreground at
  the start of an agent session, so you wait for it. (THOR's other slow
  session-start work is handed off to a detached background process - the
  re-indexing of a project, and starting the injection daemon. The backup has no
  such hand-off: nothing is detached, so the export and the network round trip
  are yours to wait through.) One more thing to know: it only stages
  and commits the `thor/` subdirectory, so it can share a repository with another
  tool's files - but the pull step it runs first rebases the *whole* repository,
  not just that subdirectory.
- **How to turn it on:** one run by hand:

  ```
  thor backup --repo <path-to-a-private-backup-clone>
  ```

  To back up even though the last one was recent:

  ```
  thor backup --repo <path-to-a-private-backup-clone> --force
  ```

  To make it automatic at the start of every agent session (a "hook" is a command
  your coding agent runs for you at a set moment):

  ```
  thor install --backup-repo <path-to-a-private-backup-clone>
  ```

  Know what that second command does beyond the backup: `thor install` always
  also adds THOR's `Stop` response-guard hook, whether or not you asked for it -
  there is no flag to leave it out. It edits your agent's `settings.json` and
  saves the previous version next to it as `settings.json.thor-bak`, and it is
  idempotent, so running it again adds nothing that is already there. If you
  want only the backup hook, add the `SessionStart` entry to `settings.json` by
  hand instead.

- **How to check it worked:** the single run prints one status line. A successful
  push says `pushed thor backup (N events)`. A run that was too soon says
  `backup is Nh old (< 20h) - skipping`. A run with nothing new says `no change
  since last backup (N events) - nothing to commit`. Then look in the backup
  clone for a new commit touching `thor/`. For the hook form, `thor install`
  prints `+ SessionStart (daily GitHub backup, debounced 20h)` the first time it
  adds it.
- **How to turn it off again:** stop running the command; for the hook, delete
  the `SessionStart` entry whose command ends in `backup --repo ...` from your
  agent's `settings.json` and restart the agent. There is no uninstall
  subcommand. Nothing is lost from your memory either way - backup only reads the
  store. The commits already pushed stay in the backup repository until you
  remove them there.

### thor import &lt;path&gt;

Fills a fresh THOR store from a JSONL snapshot that came out of a different
memory tool. This is a migration step, not a way to add facts. It is idempotent
per fact: running it twice on the same file changes nothing the second time, and
it refuses a new fact whose text is a near-duplicate of one already stored.

- **Default:** off, and one-shot. After an import that actually changed
  something, THOR writes a marker file called `SEEDED.flag` next to your store,
  and every later `thor import` refuses while that file exists. An import that
  changed nothing (empty or mistyped file) does not arm the marker, so it cannot
  lock you out of the real seeding.
- **Turn it on if:** exactly once, when you are moving from another memory tool
  into a fresh THOR store and do not want to retype the facts.
- **Leave it off if:** you want to add or change facts normally - use `remember`
  and `revise` for that. Do not import over a store you have already curated: a
  re-import can silently overwrite edits you made later and bring back facts you
  retracted. That is exactly why the one-time guard exists.
- **What it costs:** no process, no network port, no download, no daemon, and no
  growth in the size of the `thor` binary. The only lasting trace besides the
  imported facts is the one small marker file.
- **How to turn it on:**

  ```
  thor import <path-to-snapshot.jsonl>
  ```

- **How to check it worked:** it prints every counter on one line, so a run that
  only retracted things does not read as a no-op:

  ```
  Import into <store>: N created, M revised, K retracted (U unchanged, D duplicates refused, X malformed, V diverged skipped).
  ```

  When it changed something it also tells you that `SEEDED.flag` was armed next
  to the store.
- **How to turn it off again:** it is already off after the first run. To allow
  another seeding on purpose, delete `SEEDED.flag` next to your store - that is a
  deliberate file operation, the same convention THOR uses for its other flag
  files. Deleting the flag does not undo the previous import: every imported fact
  is already an event in the log, and the log is append-only. To stop an
  individual imported fact from being served, retract it the normal way with
  THOR's `retract` tool from your agent.

## Syncing two machines

Everything in this section is for one situation only: you use THOR on more than one
computer and you want the second one to know what the first one remembers. If you
work on a single machine, skip the whole section - there is nothing here to turn on.

THOR never shares one database file between machines. Instead one machine is the
**authority** (the real store, the one you work on) and the other runs a **replica**
(a byte-for-byte copy of the authority's event log). The authority pushes new events
to the replica over the network; the replica only ever receives. That direction is
fixed, and the features below are the pieces that make it work: a shared password
(`THOR_TOKEN`), a receiver (`thor recv`), a sender (`thor ship`), a way to check on
it (`thor status`), and - only if you also want to *write* from the replica side -
the capture inbox plus its drain command.

### THOR_TOKEN

An environment variable holding one shared secret string that both machines must
have set to the exact same value. It is the only thing protecting the sync
connection: THOR sends it as a "bearer token", which just means the sender puts the
secret in an HTTP header and the receiver compares it. There is no other login, no
user name, no certificate.

- **Default:** unset. With it unset, `thor recv` refuses to start at all rather than
  opening an endpoint that anyone could talk to, and `thor ship` and
  `thor drain-inbox --from` stop with "no token: pass --token or set THOR_TOKEN".
- **Turn it on if:** you are setting up any of the sync features below. It is the
  prerequisite for all of them.
- **Leave it off if:** you use one machine. The README puts it plainly: "Nothing to
  turn on if you work on one machine".
- **What it costs:** setting the variable by itself costs nothing measurable - no
  extra process, no port, no download, and no change in binary size (the HTTP
  libraries are compiled in either way; there is no build switch for sync). The real
  cost is human: you now own a long-lived secret that must be kept out of git and out
  of your shell history, and it must be identical on both machines.
- **How to turn it on:** generate any long random string and set it in the
  environment on both machines. On Linux or macOS:

  ```sh
  export THOR_TOKEN='<shared-token>'
  ```

  In Windows PowerShell the same thing is written differently - the one-line
  `THOR_TOKEN=... thor recv ...` form copied from the README is POSIX shell syntax
  and does not work there:

  ```powershell
  $env:THOR_TOKEN = '<shared-token>'
  ```

- **How to check it worked:** start `thor recv` on the replica (next block). If the
  token is missing it prints "THOR_TOKEN is not set - the sync transport has no other
  auth; refusing to open an unauthenticated endpoint" and exits. If it is set, it
  prints the listening line instead.
- **How to turn it off again:** unset the variable and stop passing `--token`. Sync
  then stops working: `thor recv` refuses to start, `thor ship` and
  `thor drain-inbox --from` error out. No stored data is touched.

  One honest wrinkle: `thor status --to <url>` does **not** error when the token is
  missing. It sends an empty token, the replica answers 401, and status reports the
  replica as UNREACHABLE. So "unreachable" in that output can also mean "wrong or
  missing token", not only "the machine is down".

### thor recv --http &lt;bind&gt;

Run this on the machine that should hold the copy. It opens a network listener and
waits for the authority to push events into it. Every event it receives is
hash-checked before it is appended, so the copy cannot silently drift.

- **Default:** off. Nothing starts a receiver on its own; it exists only as this
  explicit command (or, in the Docker template, when you set both `THOR_RECV_BIND`
  and `THOR_TOKEN` - the entrypoint starts the receiver only when neither is empty).
- **Turn it on if:** you have a second machine - a laptop next to a desktop, or an
  always-on box that answers recall for a phone or a remote editor - and you want it
  to answer with the first machine's facts.
- **Leave it off if:** you have one machine, or if the second machine would ever
  write to its own store directly. A replica's log has to stay an exact prefix of the
  authority's log. A direct write on the replica forks the chain, and every later push
  is then rejected until you wipe the replica and seed it again from scratch. If you
  need replica-side writes, use the capture inbox below instead.
- **What it costs:**
  - one long-running process that blocks until you stop it, with its own internal
    thread pool and its own open handle on the replica's database file;
  - one TCP port you choose, which must be reachable from the authority. Keep it on
    a LAN or a private tunnel - the token is the only protection, so do not expose it
    to the open internet;
  - publishing that port exposes four routes, not one: `GET /ship/cursor`,
    `POST /ship/append`, `GET /inbox/pull` and `POST /inbox/ack`. All four are
    behind the token, and the two `/inbox` ones return nothing useful unless a
    capture inbox is configured, but they are always mounted;
  - inside the Docker template the receiver runs in the background next to the MCP
    server, in a restart loop: if it dies it comes back after five seconds and logs
    one line saying so. **This changed** - it used to be started with a bare `&` and
    nothing watched it, so a dead receiver left the container looking healthy while
    it had silently stopped accepting pushes;
  - no model download, and no increase in binary size.
- **How to turn it on:** first do the one-time seeding step, then start the receiver.
  Seeding matters: the receiver only accepts a push when its log is already a prefix
  of the authority's, so a replica store that has any history of its own must be
  replaced with a fresh copy of the authority's log.

  ```sh
  # 1. on the authority, export the log
  thor export --out events.jsonl

  # 2. copy events.jsonl to the replica, remove the replica's old store
  #    (thor.db plus its -wal and -shm files), then replay it into a fresh one
  thor restore --from events.jsonl

  # 3. on the replica, start the receiver
  export THOR_TOKEN='<shared-token>'
  thor recv --http 0.0.0.0:<recv-port>
  ```

- **How to check it worked:** the process prints one line and stays running:

  ```
  thor sync receiver listening on http://0.0.0.0:<recv-port>/ship (bearer-gated)
  ```

  Then confirm from the other side with `thor status --to` (below).
- **How to turn it off again:** stop the process (or clear `THOR_RECV_BIND` and
  restart the container). Nothing is lost on either machine: the replica keeps every
  event it already received, and the authority just reports a growing lag.

### thor ship --to &lt;url&gt;

Run this on the authority. It asks the replica how far it has got, then pushes every
local event past that point. Without `--watch` it ships once and exits; with
`--watch` it stays running and re-ships on a timer.

- **Default:** off. Nothing ships anywhere unless you run this command. (The Docker
  image is the one exception: it auto-starts a resident shipper - `thor ship --watch
  --interval 60` - when both `THOR_REPLICA_URL` and `THOR_TOKEN` are set.)
- **Turn it on if:** you run a receiver and want it kept current without thinking
  about it.
- **Leave it off if:** you have one machine. And prefer the one-shot form over
  `--watch` if you only sync now and then - `--watch` is a permanent background
  process you have to supervise yourself (a scheduled task, a service unit, or a
  container), and it keeps the shared token in its environment the whole time.
- **What it costs:** no listening port is opened here - shipping is outbound only,
  the port belongs to `thor recv` on the other machine. No download, no change in
  binary size. With `--watch`: one permanently resident process, a network round
  trip every interval (more than one while a backlog drains, or if you lower
  `--batch`), and it reopens the store on every tick (that is deliberate, so newly
  written events are picked up). The repo states no measurement of its CPU or
  memory use.
- **How to turn it on:** one-shot, then the resident form. Both need the token, so
  either pass `--token` or export `THOR_TOKEN` first - a `--watch` line without
  either exits before it makes any network call.

  ```sh
  # ship once
  thor ship --to http://<replica-host>:<recv-port> --token '<shared-token>'

  # keep shipping every 60 seconds (Ctrl-C to stop)
  export THOR_TOKEN='<shared-token>'
  thor ship --to http://<replica-host>:<recv-port> --watch --interval 60
  ```

  The extra flags:
  - `--interval N` - seconds between pushes in `--watch` mode. Default 60.
  - `--batch N` - how many events go in one HTTP request. Default 256. A shipment
    also stops at about 4 MiB of serialized data, whichever limit is hit first, and
    a single event larger than that still ships on its own rather than stalling.
    Both forms honour it. **This changed:** until recently `--batch` was accepted
    and then ignored in `--watch` mode, which always sent 256. If you run a resident
    shipper with a `--batch` value, it now finally does what you asked, so expect
    more and smaller requests while a backlog drains. Nothing changes if you never
    passed the flag - the default is still 256, and the container entrypoint passes
    no `--batch` at all.
- **How to check it worked:** a one-shot run prints

  ```
  shipped 12 event(s) in 1 batch(es); receiver now at contiguous_seq 4211
  ```

  A `--watch` run prints a header and then one line per tick:

  ```
  thor reconcile: shipping to http://<replica-host>:<recv-port> every 60s (Ctrl-C to stop)
  synced: replica at seq 4211 (+12 this tick)
  ```

  To see for yourself that `--batch` is honoured, you need something to ship, so
  make a small backlog first. Stop any resident shipper, write three facts, then
  ship once with a batch of one:

  ```sh
  thor remember "batch check one"
  thor remember "batch check two"
  thor remember "batch check three"
  thor ship --to http://<replica-host>:<recv-port> --token '<shared-token>' --batch 1
  ```

  The summary line should report three batches rather than one:

  ```
  shipped 3 event(s) in 3 batch(es); receiver now at contiguous_seq 4214
  ```

  If it says `shipped 0 event(s) in 0 batch(es)`, the replica was already current
  and there was nothing to split up - write a fact and try again.

  If the replica is down it says so plainly instead of crashing - "replica offline
  since epoch ... - RPO degraded, last synced seq ..." - and repairs itself on a
  later tick once the replica is back.
- **How to turn it off again:** stop the process, or stop scheduling it. Nothing is
  lost. The authority's own log is untouched by shipping, and the shipper keeps no
  state of its own - it re-asks the replica where it stands on every run, so you can
  stop and start it freely.

### thor status --to &lt;url&gt;

A one-shot read that answers "is my second machine actually up to date?". Without
`--to` it prints only this store's own position. With `--to` it also asks the
replica and prints the difference.

- **Default:** never runs by itself; you type it when you want to know.
- **Turn it on if:** you use ship and recv. Run it right after setting them up, and
  again whenever you suspect the replica is behind.
- **Leave it off if:** you have no replica. Without `--to` it just prints your local
  position and "(no --to given: local status only)", which tells you nothing you need.
- **What it costs:** no process, no port, no download. It does open a network
  connection when `--to` is given, and that probe blocks for up to 30 seconds if the
  replica does not answer. It never appends an event, and it never creates a store:
  point it at a path with no store and it says so instead of quietly making one.
  It is still not read-only in the strict sense - opening any SQLite database
  touches its companion files (`thor.db-wal` and `thor.db-shm`) - but nothing in
  your memory changes.
- **How to turn it on:**

  ```sh
  thor status
  thor status --to http://<replica-host>:<recv-port> --token '<shared-token>'
  ```

  The token can also come from `THOR_TOKEN`. Note that without `--db`, this reads the
  default per-user store, which may not be the store you meant if you keep THOR's
  database somewhere else.
- **How to check it worked:** it is itself the check. A healthy pair prints two
  lines:

  ```
  local:   contiguous_seq 4211 (tip a1b2c3d4)
  replica: contiguous_seq 4211 (reachable) - in sync
  ```

  The other honest outcomes on the second line:
  - `LAG 12 event(s) not yet replicated` - the replica is behind.
  - `AHEAD by 3 (not a pure replica of this store)` - the replica has events yours
    does not. That means the chain forked; the replica has to be seeded again.
  - `UNREACHABLE - RPO degraded; recent local writes exist only here until it
    returns (...)` - no answer. Remember this same line appears when the token is
    wrong or missing, so check the token before assuming the machine is down.
- **How to turn it off again:** nothing to turn off - stop running it.

### THOR_CAPTURE_INBOX

Only relevant if your replica also answers an MCP endpoint that you write to (for
example a small always-on machine your phone talks to). Set this variable to a file
path on the replica, and the replica's HTTP MCP server stops writing `remember`,
`revise` and `retract` into its own database. It appends them as one JSON line each
into that file instead and answers the client "queued to capture inbox: entity &lt;id&gt;
(pending sync to the authority)". Reads are unaffected.

- **Default:** unset, so nothing is diverted and a write to a replica would fork its
  log.
- **Turn it on if:** you run a replica whose MCP endpoint you actually write to, and
  you want those writes to survive without breaking replication.
- **Leave it off if:** you are on the authority, or on a single machine, or your
  replica is read-only. Do not set it machine-wide on the authority "just in case":
  the local stdio MCP server ignores it, but `thor daemon` and `thor mcp --http` use
  the same HTTP server that reads this variable, so on a machine running the daemon
  it would divert your real writes into a file instead of storing them.
- **What it costs:** no new process, no port, no download, no measurable growth in
  binary size - a diverted write is one appended JSON line. The real cost is the
  delay: a captured write is **not** visible in the replica's own recall until it has
  been drained on the authority and shipped back. The repo is explicit that this is
  "a capture channel, not a live write". Also note the divert covers only those three
  tools. Three other MCP tools do append to the replica's own log and can therefore
  still fork it: `resolve`, `reproject`, and `mark` in its default (useful) form.
  `pin`, `unpin` and `mark` with `noise: true` are safe here - they write only the
  local `thor-ledger.db` sidecar next to the store, which is never part of the
  hash-chained log.
- **How to turn it on:** set it on the replica only, pointing at a path on the same
  data volume as its store, then restart the replica's MCP process (and its
  `thor recv` process - the drain routes read the same variable):

  ```sh
  export THOR_CAPTURE_INBOX=/data/inbox.jsonl
  ```

- **How to check it worked:** send a `remember` to the replica's MCP endpoint. The
  reply must say "queued to capture inbox" rather than a normal confirmation, and a
  new JSON line must appear in the file you named.
- **How to turn it off again:** drain the file first (next block), then unset the
  variable and restart. This is **not** lossless if you skip the drain: any lines
  still sitting in the inbox file, or in the `.draining` copy next to it, are never
  applied to the real log and are simply lost.

  One silent failure worth knowing: if you set the variable for the replica's MCP
  server but not for its `thor recv` process, `/inbox/pull` returns an empty list
  instead of an error. The drain will look successful while quietly fetching nothing.

### thor drain-inbox --inbox &lt;file&gt; | --from &lt;url&gt;

The other half of the capture inbox. Run this **on the authority**. It takes the
writes the replica queued and replays each one as a real event in the authority's
log, keeping the original entity id so later edits still chain onto the right fact.
The next `thor ship` then sends them back to the replica the normal, non-forking way.

- **Default:** never runs by itself.
- **Turn it on if:** you set `THOR_CAPTURE_INBOX` on a replica. Without the drain,
  those captures never become real memories. It is meant to run from the same
  scheduled job as your ship.
- **Leave it off if:** you have no replica, or your replica is read-only.
- **What it costs:** one more scheduled job on the authority. No resident process, no
  port opened, no download, no binary growth - it is a short-lived command that opens
  the store, applies the ops and exits.
- **How to turn it on:** pick exactly one of the two forms. Passing both, or neither,
  is rejected with "pass exactly one of --inbox &lt;file&gt; or --from &lt;url&gt;".

  ```sh
  # pull over the network from the replica's receiver (needs the token)
  thor drain-inbox --from http://<replica-host>:<recv-port> --token '<shared-token>'

  # or apply a file you copied over by hand
  thor drain-inbox --inbox /path/to/inbox.jsonl
  ```

  `--from` talks to the `/inbox/pull` and `/inbox/ack` routes on the replica's
  `thor recv` receiver, not on its MCP server. It only acknowledges (and lets the
  replica delete) a batch that applied cleanly, so a failure leaves the captures on
  the replica for the next attempt.
- **How to check it worked:** it prints a summary line and exits non-zero if
  anything failed:

  ```
  drain done: 3 applied, 1 skipped, 0 error(s) of 4 op(s)
  ```

  "skipped" means the fact was already present on the authority. Re-running the same
  drain is safe for newly created facts - a duplicate create is skipped rather than
  applied twice. That guarantee covers creates only: a revise or a retract that was
  already applied fails on the second attempt instead of being skipped, and shows up
  in the error count.
- **How to turn it off again:** stop running it. Nothing is deleted; undrained
  captures simply stay queued on the replica until you drain them or delete the file.

## Running THOR as a remote server

Everything in this section is for people who want THOR reachable from somewhere
other than the machine it is installed on - a phone, a browser, or a second
computer. If you only ever use THOR from a terminal on one machine, skip this
whole section: the normal stdio setup already gives you every tool, with no
network port at all. This section helps you decide whether you need a remote
endpoint, whether you need the container, and whether you need the two
replication roles that only exist inside that container.

Two words used throughout, in plain terms:

- **Bearer token**: a long secret string that both ends of a connection share.
  A request that carries the right string is accepted; anything else is
  rejected. It is a password for machines, sent in an HTTP header.
- **Authority and replica**: the *authority* is the one machine whose store is
  the real one. A *replica* is a second machine holding a verbatim copy that
  the authority pushes into. The replica never writes to its own copy, because
  that would make the two copies disagree.

### thor mcp --http &lt;bind&gt;

`thor mcp` normally talks to your coding agent over standard input and output -
no network involved. With `--http` it instead serves the exact same set of THOR
tools over HTTP, so a client that has no THOR binary and no checkout (a phone
app, a web connector) can reach it.

- **Default:** off. Plain `thor mcp` speaks stdio only and opens no port.
  Be aware of one related default: `thor daemon` (a separate feature, covered
  elsewhere in this guide) starts the *same* HTTP server, bound to
  `127.0.0.1:8765` unless you pass another bind. So "no HTTP server anywhere"
  is only true if you also never run the daemon.
- **Turn it on if:** you want recall and remember from a phone or a browser,
  and you are willing to put a real authentication gate in front of the port.
- **Leave it off if:** you work in a terminal on one machine. This is the
  largest security surface in the whole remote area. The MCP transport has no
  authentication of any kind, and neither do the other two routes it serves.
  Anyone who can reach the port can read and write your entire memory store.
- **What it costs:**
  - One long-running process that blocks until you stop it. It owns a Tokio
    async runtime.
  - One TCP listener on the bind you give it, serving three routes: `/mcp` (the
    tools), `/inject` (the "warm courier", which accepts prompt text so hooks
    can get memory injected quickly) and `/health` (a status probe). None of
    the three is authenticated.
  - A side effect that is easy to miss: starting it **writes a file called
    `THOR-DAEMON.flag` next to your store** and thereby takes over local prompt
    injection. Local hooks discover that flag and send their prompts to this
    HTTP server instead of starting their own. That is intended for the daemon;
    it happens with `thor mcp --http` too.
  - Recall from an HTTP client sees **the global tier only**, and this catches
    people out. The HTTP server is started with no current project (the local
    stdio server derives one from the folder it was started in; the HTTP one
    never does), and a search with no current project keeps global-tier facts
    and hides every project's facts. So a remote `recall` that finds nothing is
    usually not an empty memory - it is a search that was never pointed at your
    project. Pass `project: "<key>"` for one project, or `all_projects: true`
    to search them all. Correspondingly, a `remember` from a remote client can
    land in the global tier; `thor review-scope` and `thor reproject` exist to
    clean that up.
  - If the bind is already held by a healthy THOR daemon on the same store, it
    adopts that process and exits cleanly instead of failing.
- **How to turn it on:**

  ```sh
  thor mcp --http 127.0.0.1:<port>
  ```

  Bind it to loopback or an internal network and front it with an
  authenticating reverse proxy or access gate. If you bind it beyond loopback
  it prints a warning of its own about `/inject` being exposed with no auth.
- **How to check it worked:** on start it prints

  ```
  thor MCP (streamable-http) listening on http://<bind>/mcp (warm inject at http://<bind>/inject)
  ```

  and a GET on `/health` returns a small JSON object with `status`, `pid`,
  `bind` and `db`:

  ```sh
  curl http://127.0.0.1:<port>/health
  ```
- **How to turn it off again:** stop the process. Nothing is lost from the
  store. One exception: if you also enabled the capture inbox
  (`THOR_CAPTURE_INBOX`), drain any pending captures before you stop it, or
  they stay queued.

### The Docker deployment (deploy/Dockerfile and deploy/docker-compose.yml)

`thor/deploy/` holds a container template: a two-stage build that compiles THOR
from source and then runs it in a slim Debian image. The container's job is to
be an always-on THOR that serves the remote MCP endpoint, and optionally to
hold a replica of your workstation's store.

- **Default:** not built and not running. Nothing in the repo builds or starts
  it - there is no Docker step in CI and none in `thor install`. You have to
  run `docker compose` yourself. The `Dockerfile` is complete as shipped and
  needs no editing; only `docker-compose.yml` carries placeholders you must
  fill in.
- **Turn it on if:** you have an always-on machine (a NAS, a small server) and
  you want a remote MCP endpoint and/or a replica living there.
- **Leave it off if:** you work on one machine. The local binary plus stdio MCP
  does everything the container does, without the port and without the image.
- **What it costs:**
  - A full Rust release build. The repo's own deploy script warns this takes
    roughly 10-20 minutes on NAS-class hardware and that long silent windows
    are normal.
  - **The container's THOR is bm25-only.** The Dockerfile runs
    `cargo build --release --bin thor` with no `--features semantic`, and
    semantic is not a default feature. So the container does keyword recall
    only - no embeddings, no semantic search, no rerank - regardless of what
    your workstation does.
  - One container with `restart: always`, so it comes back after a host reboot
    and after a crash.
  - A data volume. The store lives on the `./data` bind mount, not in the
    image, so it survives rebuilds.
  - Port 8078 inside the container. The compose template only `expose`s it
    (visible to other containers on the shared network), it does not publish it
    to the host or LAN - because the MCP transport has no auth and needs an
    external gate.
  - In a replication role, a second background process inside the same
    container (see the two variables below).
- **How to turn it on:** edit `thor/deploy/docker-compose.yml` and fill in the
  placeholders: `THOR_TOKEN`, the external network name, and at most one of
  `THOR_REPLICA_URL` / `THOR_RECV_BIND`. Uncomment the `ports:` mapping only if
  you chose the replica role. Then, from `thor/deploy/`:

  ```sh
  docker compose build
  docker compose up -d
  docker compose logs -f
  ```

  Keep the real token on the host only. The file in the repo carries
  placeholders on purpose - do not commit a real token into it.

  On first start the entrypoint does three optional things in order: it
  restores `/data/thor.db` from a mounted `/data/events.jsonl` if and only if
  no store exists yet; it starts a replication process if one of the two role
  variables is set; then it serves MCP on `0.0.0.0:8078`.
- **How to check it worked:** the container log ends with the MCP listen line

  ```
  thor MCP (streamable-http) listening on http://0.0.0.0:8078/mcp (warm inject at http://0.0.0.0:8078/inject)
  ```

  and, if you chose a replication role, either the receiver line or the
  per-tick ship lines described below.
- **How to turn it off again:** `docker compose down`. Nothing is lost - the
  store sits on the `./data` bind mount, outside the image.

### THOR_RECV_BIND (container as the replica)

This is a container environment variable, not a command. Setting it to a bind
address turns the container into a **replica**: the entrypoint starts
`thor recv` in the background next to the MCP server, so a remote authority
(typically your workstation, running `thor ship`) can push its event log into
this store. This is the common topology.

- **Default:** off. The image bakes in no value, and the compose template ships
  it empty (`THOR_RECV_BIND: ""`). It only takes effect when `THOR_TOKEN` is
  also non-empty; `thor recv` refuses to start without a token at all.
- **Turn it on if:** your workstation holds the authoritative store and you
  want the container to keep a fresh copy, so the remote MCP endpoint answers
  recall with today's facts - without ever sharing a database file over the
  network.
- **Leave it off if:** you only want a remote MCP endpoint and do not care that
  it holds a separate store. Also leave it off if anything would ever write
  directly into the replica's store: the replica's log must stay an exact
  prefix of the authority's, and a direct write forks the chain and blocks
  every further push until you re-seed the replica from scratch. (The capture
  inbox exists for exactly that case; it is covered elsewhere in this guide.)
- **What it costs:**
  - One extra long-lived process inside the container, with its own async
    runtime and its own connection to the same `/data/thor.db`.
  - That process is restarted for you if it falls over. The entrypoint runs it in
    a loop and prints `thor: receiver exited (<code>), restarting in 5s` each
    time, so a crash loop is visible in `docker logs` instead of silent.
    **This changed:** it used to be started with a bare `&` and then the MCP
    server was `exec`ed as process 1, so nothing watched the receiver at all -
    `restart: always` only ever watched process 1. A replica could go stale for
    days while looking perfectly healthy. If your container image predates this,
    check the log rather than assuming.
  - You must publish the port so the authority can reach it, which is real
    network exposure. Publishing it exposes **four** routes, not one:
    `GET /ship/cursor`, `POST /ship/append`, `GET /inbox/pull` and
    `POST /inbox/ack`. All four are bearer-gated, which is why LAN or tailnet
    exposure is acceptable here (unlike port 8078, which has no auth). Do not
    put it on the open internet.
  - A one-time re-seed of the replica store, described below.
  - The exclusivity with `THOR_REPLICA_URL` is **documentation, not enforced**.
    The entrypoint has two independent checks; setting both non-empty starts
    both processes in the same container. Nothing stops you, so it is on you to
    set exactly one.
- **How to turn it on:** in the compose file you keep on the host (not the one
  in the repo), set:

  ```yaml
  services:
    thor-mcp:
      environment:
        THOR_TOKEN: "<shared-token>"
        THOR_RECV_BIND: "0.0.0.0:<recv-port>"
        THOR_REPLICA_URL: ""
      ports:
        - "<recv-port>:<recv-port>"
  ```

  Do this once, before the first push, so the two histories match:

  ```sh
  # 1. on the authority, export the log
  thor export --out events.jsonl
  # 2. stop the container, then on the host: remove the replica store
  #    (/data/thor.db, /data/thor.db-wal, /data/thor.db-shm) and drop the fresh
  #    events.jsonl at /data/events.jsonl
  # 3. start the container - it restores the export, then recv is ready
  ```

  Then run the shipper on the authority (see the sync section of this guide).
- **How to check it worked:** the container log contains

  ```
  thor sync receiver listening on http://<bind>/ship (bearer-gated)
  ```

  and from the authority machine:

  ```sh
  thor status --to http://<replica-host>:<recv-port> --token <shared-token>
  ```

  A healthy pair prints `replica: contiguous_seq <n> (reachable) - in sync`.
  Pass the token: without it the probe sends an empty bearer, gets a 401, and
  reports the replica as UNREACHABLE, which looks like a network problem but is
  not.
- **How to turn it off again:** clear the variable and restart the container.
  It falls back to MCP-only and stops accepting pushes. No data is lost - the
  replica keeps every event it already received. Un-publishing the port is a
  separate edit to the `ports:` mapping.

### THOR_REPLICA_URL (container as the authority)

The mirror image of the variable above. Setting it to a replica's receive
endpoint makes the container the **authority**: the entrypoint starts
`thor ship --to <that URL> --watch --interval 60` in the background, so the
container's store is pushed to a `thor recv` running somewhere else. (`--to` is
required on `thor ship`; the entrypoint fills it in from the variable.)

- **Default:** off. The compose template ships it empty
  (`THOR_REPLICA_URL: ""`), and the image sets no value. Like the other role,
  it only takes effect when `THOR_TOKEN` is also non-empty.
- **Turn it on if:** the container genuinely holds the source of truth and
  another machine should hold the copy.
- **Leave it off if:** your workstation is the authority - which is the common
  case. There you set `THOR_RECV_BIND` instead. Setting both is not blocked by
  any code, but it starts two replication processes in one container and is not
  a supported topology.
- **What it costs:**
  - One extra long-lived process inside the container. It never returns, and it
    runs in the same restart loop described above, logging
    `thor: shipper exited (<code>), restarting in 5s` if it ever falls over.
  - A network round trip to the replica every 60 seconds, and the shared token
    sitting in the container's environment.
  - No listening port of its own - shipping is outbound only. The port belongs
    to the `thor recv` on the other machine.
- **How to turn it on:** in the compose file you keep on the host:

  ```yaml
  services:
    thor-mcp:
      environment:
        THOR_TOKEN: "<shared-token>"
        THOR_REPLICA_URL: "http://<replica-host>:<recv-port>"
        THOR_RECV_BIND: ""
  ```

  Run `thor recv` on the replica machine, and re-seed that replica from the
  container's export first (same three steps as above, in the other direction).
- **How to check it worked:** the container log prints one line per tick, of
  the form `synced: replica at seq <n> (+<k> this tick)`. To ask the container
  itself, you must pass `--db` explicitly - without it, `thor` inside the
  container resolves the default per-user store path and reads a different
  (empty) database:

  ```sh
  docker compose exec thor-mcp thor --db /data/thor.db status --to http://<replica-host>:<recv-port> --token <shared-token>
  ```

  When the replica is current the line reads
  `replica: contiguous_seq <n> (reachable) - in sync`. There is no "lag 0"
  output; a lag line appears only while events are actually outstanding.
- **How to turn it off again:** clear the variable and restart the container.
  Shipping stops and it is MCP-only again. Nothing is deleted on either side -
  the replica simply stops advancing.

### deploy/deploy-watcher.sh

A small shell script template for the machine running the container. Registered
as a scheduled task that runs as root every few minutes, it watches for a
trigger file; when it appears, it unpacks the newest source tarball in that
directory and rebuilds and restarts the container. The point is that you can
redeploy from your workstation by copying a file and touching another file,
without holding root over SSH.

- **Default:** not installed, and inert even if you do run it. As shipped, line
  24 reads `T=/path/to/your/thor-project`, and the very next line exits
  immediately when no trigger file is found there. So an unedited copy does
  nothing, every tick, forever.
- **Turn it on if:** you run the Docker deployment, you redeploy it often, and
  you do not want to use root over SSH for each rebuild.
- **Leave it off if:** you have no container, or you redeploy rarely -
  `docker compose build && docker compose up -d` by hand is the same thing
  without a standing root job. Understand the security surface before enabling
  it: this is a root-scheduled job that builds and runs whatever tarball
  happens to be newest in that directory. Anyone who can write into that
  directory and create the trigger file effectively gets a root-privileged
  build and container restart on that host.
- **What it costs:** a standing scheduled task running as root, plus a full
  Rust release build (the script's own message: roughly 10-20 minutes on
  NAS-class hardware) each time it triggers. It opens no port, downloads no
  model, adds no latency to any THOR operation, and never touches `thor.db`.
- **How to turn it on:**

  ```sh
  # 1. edit the project directory
  #    T=/path/to/your/thor-project  ->  your actual project directory
  # 2. edit the docker path. The script hardcodes /usr/local/bin/docker, which
  #    is the Synology/DSM location. On most Linux distributions it is
  #    /usr/bin/docker - check with:  command -v docker
  # 3. validate the edit (a syntax error makes it fail silently)
  sh -n deploy-watcher.sh
  # 4. register it as a root scheduled task on ~5 minute ticks
  ```

  The tarball must have `thor/` as its top-level directory. The script unpacks
  with `--strip-components=1` and excludes the member path
  `thor/deploy/docker-compose.yml`, so that your live compose file with the real
  network config and token is never overwritten. A tarball built without that
  leading `thor/` still unpacks, but the exclude no longer matches and your
  live compose file gets replaced by the repo placeholder version. Build it
  like this:

  ```sh
  git archive --prefix=thor/ -o thor-src-<rev>.tar.gz HEAD:thor
  ```

  Then trigger a deploy from your workstation:

  ```sh
  scp -O thor-src-<rev>.tar.gz <user>@<container-host>:<project-dir>/
  ssh <user>@<container-host> "touch <project-dir>/deploy-requested.flag"
  ```

  (`scp -O` is needed on Synology DSM, which has no SFTP subsystem; plain `scp`
  fails there with "subsystem request failed".)
- **How to check it worked:** read `deploy.log` in the project directory. A
  successful run contains `START`, `UNPACK` and `UNPACK_DONE`, `BUILD_DONE` and
  `UP_DONE`. Only `START`, `BUILD_DONE` and `UP_DONE` carry a timestamp - the two
  `UNPACK` lines do not, so do not go looking for one. Note also that the log is
  overwritten on every run, so it only ever shows the most recent deploy. The
  script documents its own broken
  symptom: with a syntax error it does nothing every tick, so trigger **flags
  pile up** and `deploy.log`'s modification time falls behind the flag's. If
  the flag is still sitting there minutes later, the watcher is not running or
  not parsing.
- **How to turn it off again:** remove the scheduled task and delete the
  script. Nothing is lost - it only ever touches the project directory and the
  container, never the data volume.

## Switching things off

Two of THOR's switches are not commands or settings at all: they are empty files
you create next to the store. One mutes most of what THOR says on its own; the
other adds one sentence to what it says. This section helps you decide whether
you want either, and shows exactly how to create and remove them.

Some words used below, in plain terms:

- A **hook** is a small command your AI agent runs automatically at fixed
  moments: when you send a prompt, before it runs a tool, when it wants to
  finish an answer. `thor install` writes those hook entries into your agent's
  `settings.json`. THOR itself is not running the rest of the time.
- A **flag file** is an empty file whose *name* is the whole message. Nothing is
  written inside it. THOR checks whether the file exists and behaves
  accordingly.
- **The store** is `thor.db`, THOR's database file. It lives in THOR's per-user
  data directory: `%LOCALAPPDATA%\thor\` on Windows, otherwise
  `$XDG_DATA_HOME/thor`, otherwise `$HOME/.local/share/thor`. Flag files must
  sit in that same directory, right next to `thor.db`. If you run THOR with a
  custom store path (`--db <path>`), put the flag next to *that* file instead.

### THOR-SILENT.flag

An empty file next to the store that tells THOR's hooks to say nothing. While it
exists, the recall block is not injected into your prompts, the guard advisories
stay quiet, the capture nudge stops holding your agent's final answer, and the
pre-compaction reminder is skipped. Anything you ask for yourself keeps
working: the CLI commands (`thor recall "..."`, `thor get <id>`, and the write
path `thor create <entity_id> "<body>"`) and every MCP tool, including
`remember`, are not affected. Note that `remember` exists only as an MCP tool
inside an agent session, not as a `thor remember` shell command.

- **Default:** absent. Nothing in THOR ever creates this file, so unless you
  create it yourself it is not there. What it actually mutes depends on which
  hooks you installed: a plain `thor install` wires only the Stop response
  guard, so on a default install this flag mutes that guard and its capture
  nudge. If you installed `--with-courier` it also mutes the injected recall
  block and the pre-compaction reminder; with `--with-guard` it also mutes the
  before-a-tool-runs advisories.
- **Turn it on if:** you want a stretch of work with no injected memory and no
  held turns - a demo, a screen recording, a clean-room reproduction - or you
  suspect THOR is the cause of something and want to rule it out in one step,
  with no settings edit and no restart.
- **Leave it off if:** you want to switch off exactly one surface permanently.
  This flag is all-or-nothing. The targeted move is removing that one hook entry
  from your agent's `settings.json` (`thor install` writes a
  `settings.json.thor-bak` copy before every change).
- **What it costs:** no extra process, no network port, no download, no larger
  binary, and no write of any kind - the flag only gates output, it never
  touches the store. Turning it on is creating one empty file. Each hook run
  does one filesystem existence check for it, read fresh every time and never
  cached, not even by the warm injection daemon. The repo states no measurement
  of what that check costs.
- **What it does not silence (read this before relying on it):** the
  `session-start` hook does not check the flag. If it is installed, the pinned
  brief block, the "this project is not set up in THOR yet" cue, the
  scope-review nudge and the background re-index of a known project still
  happen. The other things `thor install` can wire at session start - `thor
  warm` (pre-warming the embedder), `thor ensure-daemon` and `thor backup
  --repo ...` - do not check the flag either and still run. So this is a mute
  for the advisory surfaces, not a full stop for every automatic thing THOR
  does.
- **How to turn it on:**

  ```powershell
  # Windows PowerShell
  New-Item -ItemType File "$env:LOCALAPPDATA\thor\THOR-SILENT.flag"
  ```

  ```sh
  # Linux / macOS
  touch "${XDG_DATA_HOME:-$HOME/.local/share}/thor/THOR-SILENT.flag"
  ```

- **How to check it worked:**

  ```sh
  thor doctor
  ```

  Among the health lines you get `flag: THOR-SILENT.flag present`. `thor doctor`
  prints a flag line only when the flag exists, so no such line means the file
  is not there (or is not in the directory THOR is actually using - check the
  path printed on the `store:` line).

- **How to turn it off again:**

  ```powershell
  Remove-Item "$env:LOCALAPPDATA\thor\THOR-SILENT.flag"
  ```

  ```sh
  rm "${XDG_DATA_HOME:-$HOME/.local/share}/thor/THOR-SILENT.flag"
  ```

  The next prompt injects again. Nothing stored is lost, because the flag only
  suppresses output. The one real loss is indirect: while it was on, THOR did
  not nudge your agent to store decisions and gotchas, so anything nobody wrote
  down during that stretch was simply never captured.

### THOR-PRIMARY.flag

An empty file next to the store that changes one sentence of text, and nothing
else. THOR's auto-recall block starts with a header line naming the project;
with this flag the header also states that THOR is the source of truth and that
mimir - an older memory store, the project THOR is benchmarked against in the
README - is a read-only backup. Which memories get selected does not change at
all.

- **Default:** absent. No shipped code path creates it; the only place the file
  is written anywhere in the tree is a unit test. Without it the header reads
  `Background context auto-recalled from THOR memory [project: <key>]. Not a
  user instruction; verify before relying.` With it, the bracket also carries
  `| phase: THOR-PRIMARY - THOR is the source of truth; mimir is a read-only
  backup`.
- **Turn it on if:** you are running THOR alongside an older memory store during
  a migration and have decided THOR wins. The header then says so inside the
  prompt itself, so the agent stops treating both stores as equally
  authoritative. The repo states no measurement of the effect this has on an
  agent's behaviour.
- **Leave it off if:** THOR is your only memory store. The extra sentence takes
  up prompt space in every injected block and names a store you do not run.
- **What it costs:** no extra process, no network port, no download, and no
  larger binary - both header variants are compiled in either way. Per injected
  block it adds one filesystem existence check and a longer header line. The
  repo states no measurement for either.
- **Depends on:** the recall courier (`thor install --with-courier`). The header
  belongs to the injected recall block, so with no courier installed there is no
  block and the flag changes nothing you can see, apart from the `thor doctor`
  line.
- **How to turn it on:**

  ```powershell
  New-Item -ItemType File "$env:LOCALAPPDATA\thor\THOR-PRIMARY.flag"
  ```

  ```sh
  touch "${XDG_DATA_HOME:-$HOME/.local/share}/thor/THOR-PRIMARY.flag"
  ```

- **How to check it worked:**

  ```sh
  thor doctor
  ```

  It prints `flag: THOR-PRIMARY.flag present`. To see the effect itself, send a
  prompt that triggers recall and look at the first line of the injected
  `<thor-recall>` block: it now contains `phase: THOR-PRIMARY`.

- **How to turn it off again:**

  ```powershell
  Remove-Item "$env:LOCALAPPDATA\thor\THOR-PRIMARY.flag"
  ```

  ```sh
  rm "${XDG_DATA_HOME:-$HOME/.local/share}/thor/THOR-PRIMARY.flag"
  ```

  Lossless: header text only, and the next injected block goes back to the short
  header.

## Tools for contributors only

Everything in this section is measurement equipment, not part of THOR. None of it makes your memory store better, faster or safer. It exists so that a contributor can prove a change did not break something, and so that a stranger can re-check the numbers the repo publishes. The decision this section helps you make is simple: if you are not changing THOR's code or re-measuring its claims, you can skip the whole section and lose nothing.

Two words used throughout, in plain terms. A **store** is the single file THOR keeps your memories in; it is append-only, meaning nothing is ever overwritten or erased, only added on top. A **harness** is a small throwaway program that drives THOR's real code and prints a score.

Most of the harnesses live in `thor/examples/`. They are cargo "example targets": separate programs that live next to the crate but are not part of the `thor` binary. A plain `cargo build --release` does not build them and nothing on your machine ever runs them on its own. The shipped binary does not grow by a single byte because they exist. The last three entries below are not cargo examples at all but loose scripts under `thor/tools/`, run by hand with Python or PowerShell.

### cargo run --release --example drift_eval

Replays a corpus of scripted situations that ships with the repo, through THOR's real automatic-recall paths, and scores how often the fact that would have prevented a mistake actually got surfaced. It is the one published claim in this repo that a stranger can reproduce with no private data.

- **Default:** never built and never run. It is an example target, so `cargo build --release` skips it. The project's own CI runs it on every pull request, but nothing runs it on your machine unless you type the command.
- **Turn it on if:** you are changing recall, ranking, the courier (the piece that injects memories into a prompt automatically), the guard (the piece that warns before a risky file edit or command), scoping, or snippet truncation, and you want to know whether the safety net still catches the same cases.
- **Leave it off if:** you just want THOR to remember things. The harness measures THOR, it never improves it. It changes no setting and writes nothing to your store.
- **What it costs:** a one-time `cargo build --release` of the crate plus its test-only dependency `tempfile`. It is deliberately not feature-gated, so it builds on the plain keyword-search build with no ONNX runtime and no embedding model. Every scenario runs in a fresh throwaway temporary directory, so your real store is not touched at all. The repo states no measurement for how long a run takes.
- **How to turn it on:**

```
cd thor
cargo run --release --example drift_eval
```

  Add `-- --json` for machine-readable output, or `-- --max-noise <n>` to loosen the false-fire limit for a single run.

- **How to check it worked:** the run starts with `THOR drift eval - committed corpus (N scenarios, M silence)`, then prints one row per scenario (id / hint / courier / guard), then a per-channel summary with preventer-surfaced and full-catch percentages, then a catch / miss / noise / quiet table. README states the current build as courier 76%, guard channel 16/16, either-channel 96%, and noise 1 under a one-way limit that is only ever tightened.
- **How to turn it off again:** stop running it. Nothing is lost. It never touches the real store, it only reads the corpus, and it never writes a new baseline back.

### thor/eval/drift_scenarios.jsonl

The corpus `drift_eval` reads. It is 52 scripted cases, one JSON object per line: a task prompt, a set of seed facts and code fragments that act as decoys, the id of the fact that should have prevented the mistake, the words that must survive snippet shortening, and which channel is expected to fire. README describes it as 46 cases that should fire plus 6 that must stay silent, in English and Dutch, decoys included.

- **Default:** present in the repo and read automatically whenever `drift_eval` runs without `--live`. Nothing else in THOR ever opens it; it is test data, not runtime data.
- **Turn it on if:** nothing to turn on. Extend it when you fix a class of drift the corpus does not cover, or when you add a channel.
- **Leave it off if:** you are a user. It does nothing for you.
- **What it costs:** disk space for one text file. No process, no port, no download.
- **How to turn it on:** it is read automatically. To add a case, append one JSON object per line and re-run the harness. The harness validates every line and refuses a corpus that has shrunk below 30 scenarios, so an accidental deletion fails loudly. Each scenario is accepted only with 5 to 15 decoy seeds.
- **How to check it worked:** the harness header reports the new count: `THOR drift eval - committed corpus (N scenarios, M silence)`.
- **How to turn it off again:** not applicable. It is inert data. Deleting it only breaks `drift_eval` and the CI job that gates on it. If you run `thor ingest` over this repo, the corpus is skipped automatically: `.jsonl` is on THOR's ingest skip list, so it does not get chunked into your store.

### cargo run --release --example drift_eval -- --live `<corpus>`

The same harness pointed at your real store instead of at throwaway ones, using a scored prompt set that you write yourself. It reports per-category numbers rather than the fixed table.

- **Default:** off. Without `--live` the harness always runs the committed corpus.
- **Turn it on if:** you maintain your own labelled set (a prompt plus the fact that should have surfaced) and want to see which categories your own store actually catches.
- **Leave it off if:** you have no such file. No corpus for this mode ships. The release procedure states that the private eval corpus is deliberately kept out of releases, so without writing your own gold file this flag has nothing to read. It also needs an already-populated store, so it is useless on day one.
- **What it costs:** no daemon, no port, no download. It appends no event to your store, and it refuses to run if no store exists, so it can never create one. One honest caveat: it is not perfectly read-only on disk. When the pool of surviving hits is large enough, the ranking code consults a small counters file kept next to the store, and that file can be created if it is not there yet. Your memories are not modified.
- **How to turn it on:**

```
cd thor
cargo run --release --example drift_eval -- --live <path/to/your-corpus.json> [--cwd <project-dir>] [--json]
```

- **How to check it worked:** the header switches to `THOR drift eval - LIVE store (N scenarios, M skipped: seq not in store)` and the output becomes per-category rows. Two metrics are printed: whether the gold fact's id appeared in the injected block, and whether a served hit carried at least half of the gold's key terms. The file is explicit that the published figure was judged by a language model and that these mechanical proxies bracket it rather than reproduce it. A later revision of a gold fact does not cost you the id metric; matching is done at the level of the fact, not the revision.
- **How to turn it off again:** omit `--live`. Nothing to undo.

### cargo run --release --example hits_dump -- --queries `<in.json>` --out `<out.json>`

Reads a JSON array of query objects, runs each one through a real production recall path, and writes the same rows back out with a `hits` array attached, ready for a blind judging pass. This is the harness behind the repo's published comparison numbers.

- **Default:** absent from any THOR run. Separate example target, not part of the shipped binary.
- **Turn it on if:** you are re-measuring the benchmark before a release, or judging one ranking change against another over the same query set.
- **Leave it off if:** you are a user. It produces judging material and changes nothing about how THOR behaves.
- **What it costs:** no process, no port, no daemon, and no growth of the shipped binary. Two real costs if you do run it. First, it opens the store read-write: opening runs the schema setup and re-syncs the full-text search index, so the store file and its search index can change even though no memory event is added. Second, the `courier` channel passes a session id, so it writes to the courier's suppression ledger (the small record of what was already shown this session) and, while writing, prunes every row in that same area older than 48 hours. Running the courier channel against your live store therefore throws away real suppression state. Point it at a copy with `--db <clone.db>` if that matters. `--rerank` additionally needs the `semantic` build and the local cross-encoder model (a second, slower model that re-orders hits) present, or the run aborts.
- **How to turn it on:**

```
cd thor
cargo run --release --features semantic --example hits_dump -- --queries <in.json> --out <out.json> [--limit 5] [--scope all|global|project:<key>] [--full] [--channel fused|courier] [--cwd <dir>] [--rerank]
```

  It also builds without `--features semantic`; it then degrades to keyword-only recall, and `--rerank` is rejected.

- **How to check it worked:** it prints progress every 25 rows and ends with `wrote N rows to <out>`. With `--rerank` it also prints a `rerank latency ms: median / p90 / max` line.
- **How to turn it off again:** stop running it. The output file is yours to delete. The store-side writes described above cannot be undone, which is why a clone is the safer target.

Then there are four harnesses that only compile with the optional `semantic` build feature, because they drive the dense-vector code that a keyword-only build does not contain. All four share the same entry cost: `--features semantic` pulls in `fastembed`, which brings the ONNX runtime down as a build-time binary download and makes the build longer and the binary bigger. All four also read the store path from the Windows per-user variable and stop with an error if it is unset, so as written they are Windows-only. Build them all at once with `cargo build --release --features semantic --examples`.

### cargo run --release --features semantic --example recall_eval

Runs THOR's real deliberate-recall function over a battery of questions and reports, per category, how often the right answer landed in the top 5. It sweeps the fusion weight, which is the dial that decides how much meaning-similarity counts next to keyword matching.

- **Default:** absent on a default build. There is no default feature set in the manifest, and this example declares `required-features = ["semantic"]`, so a plain `cargo build --release --examples` does not produce it.
- **Turn it on if:** you are tuning the fusion weight or changing the ranking code, and you have your own query and gold files.
- **Leave it off if:** you are a user, or you are on Linux or macOS. Three blockers: the input files (query battery, gold map, optional content golds) are private and do not ship; it needs a local embedding model plus the vector sidecar (the precomputed file of meaning-vectors kept next to the store); and its path helper reads the Windows per-user variable and stops if it is unset.
- **What it costs:** the full `semantic` build cost described above, plus loading the embedding model at runtime. It appends no event to your store.
- **How to turn it on:**

```
cd thor
cargo run --release --features semantic --example recall_eval
```

  It expects `eval/percategory_queries.json` and `eval/golds52.json` inside the per-user data directory (`%LOCALAPPDATA%\thor\` on Windows). Neither file is in the repo; you have to write them.

- **How to check it worked:** it prints a table headed `REAL recall.rs recall_fused - normalized-fusion lambda sweep (cells = recall@5 per category)`. Read the cells as recall@5 only; the header says so. One caveat about the comparison row: the baseline arm is produced by feeding the same fused function an all-zero query vector, so the meaning term drops out, but path boosting and the coverage term stay switched on. It is a baseline, not a clean keyword-only arm.
- **How to turn it off again:** stop running it. Nothing to undo.

### cargo run --release --features semantic --example cache_correctness

Step 1 of a two-step gate for the resident cache (the in-memory copy of recall data that a running THOR keeps warm). It compares the cached path against the cold path over a set of deliberately awkward queries and demands that every returned hit is identical down to the byte. Correctness only, no timing.

- **Default:** absent on a default build, same reason as `recall_eval`.
- **Turn it on if:** you are changing the resident cache, head projection, or anything recall reads, and you need proof that a speed change did not quietly become a correctness change.
- **Leave it off if:** you are a user, and think twice even as a contributor. It appends probe events (a create, a revise, a retract) to your live store at the default location. THOR is append-only, so those events stay forever as retracted history. Run it against a copy of the store if that matters to you.
- **What it costs:** the full `semantic` build cost, plus the permanent probe events described above. No process is started and no port is opened; the cache is built inside the harness itself. Scale warning: the query battery is loaded from `percategory_queries.json` and `queries_full.json` under `eval/` in the per-user data directory, and neither file is committed anywhere in the repo. On a clean checkout you therefore get only the dozen built-in query shapes, not the large battery the numbers in the repo were measured on.
- **How to turn it on:**

```
cd thor
cargo run --release --features semantic --example cache_correctness
```

- **How to check it worked:** it ends with `GATE: N comparisons, M mismatches` and then either `RESULT: PASS - cached path byte-identical to cold path everywhere` or a FAIL listing the mismatching queries. It also refuses to pass vacuously: if the freshly built cache reports itself stale, the run stops with an explicit assertion instead of scoring a meaningless PASS.
- **How to turn it off again:** stop running it. The probe events already written cannot be deleted, by design. They remain as retracted history under a probe id.

### cargo run --release --features semantic --example cache_speed

Step 2 of the same gate, to be run only after step 1 passes. It embeds a battery of up to 60 queries up front so the embedder is excluded from the measurement, throws away a warm-up round, then times cold and warm recall alternately per query.

- **Default:** absent on a default build, same reason as above.
- **Turn it on if:** straight after `cache_correctness` passes, when you need a defensible latency number for a caching change.
- **Leave it off if:** you are a user. It reports timings, it does not make anything faster. The numbers are specific to your machine and store size, so a figure from one machine says little about another. Also leave it off on a clean checkout: it needs the same two private battery files, and if neither can be opened the query list is empty and the run stops with a panic rather than a message.
- **What it costs:** the full `semantic` build cost. It appends no event to your store, but it is not read-only either: the store, the vector sidecar and the symbol sidecar are all opened read-write and each writes on open, so those files can change.
- **How to turn it on:**

```
cd thor
cargo run --release --features semantic --example cache_speed
```

- **How to check it worked:** it first prints how many queries the battery holds, then a cold/warm table with median, p90 and mean, then a `median: X ms -> Y ms = Z% faster` line and a validation-versus-rebuild ratio.
- **How to turn it off again:** stop running it. Nothing to undo.

### cargo run --release --features semantic --example warm_ab

The gate measured on the thing that actually matters: not an internal score, but the block of text the agent receives. It sends the same prompts through both production entry points, the shipping warm path and the daemon's resident path, and compares the two injected blocks.

- **Default:** absent on a default build, same reason as above.
- **Turn it on if:** you changed the courier or the resident cache and need to prove the injected block did not move.
- **Leave it off if:** you are a user. It has the heaviest side effects in this section.
- **What it costs:** the full `semantic` build cost, plus three real runtime costs. First, it appends probe events (create, revise, retract) to your live store, which stay as retracted history. Second, if no embedding daemon (the background process that holds the model in memory) answers, it starts the real `thor` binary as a background daemon itself, and that process keeps running after the harness exits. Third, it needs that binary to exist: it looks for `thor.exe` one directory above the examples directory, which is `target/<profile>/thor.exe`, so run `cargo build --release --features semantic` first.
- **How to turn it on:**

```
cd thor
cargo build --release --features semantic
cargo run --release --features semantic --example warm_ab
```

- **How to check it worked:** it prints `embed daemon: UP` first, then per-channel `injection IDENTICAL` / `DIFFERENT` counts, a `cache usage: R reuses, B rebuilds` line and a verdict. Do not expect a full report on a fresh clone: the per-channel sections and the cold-versus-warm latency line are skipped when the prompt list for that channel is empty, and those prompt lists are not part of the repo.
- **How to turn it off again:** stop running it. The probe events stay, because the log is append-only. A daemon it started keeps running until you stop it.

### python thor/tools/gen_benchmark_chart.py

Regenerates the benchmark chart image from the measured numbers, which are written at the top of the script itself. The chart is data, not hand-drawn artwork.

- **Default:** never runs on its own. The only place it is invoked in the repo is a manual maintainer step in the release procedure.
- **Turn it on if:** you are a maintainer preparing a release after re-measuring, or you corrected a number in the benchmark document and the chart now disagrees with it.
- **Leave it off if:** you are a user, or a contributor who did not re-measure. The numbers live inside the script, so running it without new measurements just rewrites the same file. It also needs Python installed, which THOR itself does not: THOR is a single Rust binary with no external services.
- **What it costs:** a Python 3 interpreter and nothing else. Standard library only, no packages to install. It writes exactly one file, the committed chart.
- **How to turn it on:**

```
python thor/tools/gen_benchmark_chart.py
```

  Edit the measured values near the top of the script first. On many Linux and macOS systems the interpreter is called `python3`.

- **How to check it worked:** it prints `wrote <path to benchmark.svg> (height N)`. If a label would overflow the canvas it stops with an assertion naming the offending line. Note that this guard only fails the script run: nothing in the project's CI invokes the script, so a green CI run is not evidence that the chart renders.
- **How to turn it off again:** stop running it. Nothing is lost; the chart is a tracked file, so `git checkout` restores the previous version.

### python thor/tools/export_mimir.py

A migration helper. It exports the contents of a [mimir](https://github.com/MakerViking/mimir) SQLite database to a JSONL snapshot that `thor import` can read. mimir is the other local memory tool THOR is measured against in BENCHMARKS.md.

- **Default:** tracked in the repo but never run by THOR. No Rust source calls it. The only caller inside the repo is the side-by-side script below.
- **Turn it on if:** you already use mimir and want to seed a THOR store from it once. This is a migration case, not a feature.
- **Leave it off if:** you do not have a mimir database, in which case the script is inert. Note also that once a store has been seeded, a second `thor import` is refused: a marker file next to the store guards against re-imports.
- **What it costs:** a Python 3 interpreter. Standard library only (argparse, json, os, sqlite3, sys). No process, no port, no download, no binary growth, no runtime latency. It opens the source database in read-only mode, so it cannot write to it. One thing to be careful with: the snapshot it produces contains your private memories, so keep it out of any git work tree. The default output location is already outside any repo, under `seed/` in the per-user data directory.
- **How to turn it on:**

```
python thor/tools/export_mimir.py [--mimir-db <path>] [--out <snapshot.jsonl>] [--kinds memory,chunk]
thor --db <store> import <snapshot.jsonl>
```

- **How to check it worked:** it prints `exported N records to <out> (skipped M); by kind: {...}`.
- **How to turn it off again:** stop running it and delete the snapshot when you are done. The source database is untouched by construction.

### pwsh thor/tools/run_sidebyside.ps1

An end-to-end comparison rig. In one command it builds the release binary, exports a mimir database read-only to a snapshot, imports it into a THOR store, runs the store's integrity check, and then pushes a fixed set of prompts through the courier so you can read what gets injected.

- **Default:** never runs. Nothing references it: no hook, no CI workflow, no cargo target, no other document. It is a script you type by hand.
- **Turn it on if:** you are porting from mimir and want to see, prompt by prompt, what each side surfaces before you commit to the switch.
- **Leave it off if:** almost always. It assumes Windows PowerShell, Python, a Rust toolchain and a mimir database, and it is a maintainer's rig rather than part of using THOR.
- **What it costs:** read this one before you run it. The data directory defaults to the ordinary per-user THOR directory, which means the script targets your real store. With `-Reseed` it deletes that store (and the marker beside it that would otherwise refuse the import) and rebuilds it from the snapshot. Without `-Reseed`, a store that was ever seeded refuses the import outright and the run simply carries on to the next step, so nothing is merged. Either way, **always pass `-DataDir <a scratch directory>`** if you value what is in your store. Two more things worth knowing before a `-Reseed`: the sidecars next to the store (`thor-ledger.db`, which holds your pins, plus the vectors and symbols sidecars) are not removed, so they end up pointing at events that no longer exist; and mimir's column only appears when the path in `-MimirExe` actually exists - it is existence that decides, not passing the flag, so a wrong path silently gives you THOR's side only.
- **How to turn it on:**

```
pwsh thor/tools/run_sidebyside.ps1 -DataDir <a scratch directory>
```

  Optional switches: `-SkipBuild`, `-Reseed`, `-MimirDb <path>`, `-MimirExe <path>`.

- **How to check it worked:** it prints labelled sections for build, export, seed, integrity check and side-by-side recall, and for each prompt either the injected block or `(silent - gated or no hits)`.
- **How to turn it off again:** stop running it. What it already did to the store it pointed at cannot be undone, so the scratch directory is the safety measure, not an afterthought.
