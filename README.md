![The AI Memory Bible](assets/banner.png)

# THOR - a central, lossless memory for AI coding agents

THOR is a from-scratch persistent memory for AI coding agents (such as Claude
Code). It remembers decisions, gotchas and context across your projects and gives
them back **automatically at the right moment**, so a session does not have to
start from zero every time. It is a single Rust binary: no external services, no
git required, and it never loses a write.

## What it does

- **Lossless append-only store.** Every fact is an event in a hash-chained,
  append-only SQLite log. A concurrent conflicting edit *branches* (both heads are
  kept and surfaced) instead of silently overwriting - nothing is ever lost. A
  built-in `fsck` recomputes the chain, so tampering is detectable.
- **Automatic recall.** A per-prompt hook (the *courier*) searches memory for the
  current prompt and injects the top hits, so the agent starts each turn with the
  relevant context. Lexical bm25 (FTS5) by default; an optional semantic
  score-fusion layer improves recall on paraphrased questions (see below).
- **Guard.** A moment-of-action hook (`PreToolUse`) that emits an advisory when a
  tool call matches a risk rulebook (fail-open, never blocks).
- **Cross-machine sync.** Log-shipping (`thor ship` / `thor recv`) replicates the
  event log to another machine, verbatim and hash-identical.
- **Backup + restore.** `thor export` writes the log as canonical JSONL; `thor
  restore` replays it to an identical tip hash and verifies every recomputed hash.
- **Runs anywhere.** Local CLI + hooks, or a remote MCP server (Streamable-HTTP)
  behind an auth gate.

## Quick start

```sh
cd thor
cargo test            # run the test suite
cargo build --release # build the binary (target/release/thor)
```

Install the hooks into your agent's settings (backs up first, only adds THOR
entries, idempotent):

```sh
thor install --with-courier          # auto-recall on every prompt
thor install --with-guard            # + the moment-of-action guard
```

Use it:

```sh
thor remember "<a durable fact>"     # (via the MCP tool in an agent session)
thor recall "how does X work"        # search memory
thor get <entity_id>                 # the authoritative head(s) for one fact
thor fsck                            # verify chain integrity
```

The courier runs automatically per prompt and injects a `<thor-recall>` block.

## Semantic recall (optional, off by default)

Lexical bm25 is the always-on default. A dense **score-fusion** layer adds
meaning-based retrieval so a paraphrased question still finds the right memory. It
is a compile-time feature, OFF by default, and degrades to bm25 whenever anything
is missing - it can never make recall worse.

```sh
cargo build --release --features semantic
```

- Put the embedding model files under `%LOCALAPPDATA%\thor\model\` (or point
  `thor vectors build --model-dir <dir>` at them). Any local ONNX sentence-
  embedding model with its tokenizer works; a multilingual MiniLM is a good
  default.
- Build the precomputed vector sidecar, then check it:
  ```sh
  thor vectors build      # embed every stored fact once
  thor vectors status
  ```
- Recall now fuses lexical and dense candidates: `fused = bm_norm + LAMBDA*cos`,
  with the bm25 leg min-max normalized per query. The per-prompt courier never
  pays the model load cost - a warm `thor embed-daemon` holds the model and the
  courier falls back to bm25 (and warms the daemon) if it is not up.
- `thor vectors sync` embeds only new facts (index maintenance).

The dense sidecar (`thor-vectors.db`) is derived and deletable: remove it and
recall silently returns to bm25.

## Sync (optional)

Replicate the log to another machine over the LAN/tailnet, bearer-token gated:

```sh
# on the replica:
THOR_TOKEN=<shared-token> thor recv --http 0.0.0.0:5555
# on the authority:
thor ship --to http://<replica>:5555 --token <shared-token> --watch
thor status --to http://<replica>:5555 --token <shared-token>
```

Keep the authority's `thor.db` on a **local disk** - it is never opened over a
network share (SQLite WAL requires real shared memory). Other machines get a
replica via ship/recv, never a shared network file.

## Deploy as a remote MCP server

`thor/deploy/` contains a `Dockerfile` and `docker-compose.yml` template. Run
`thor mcp --http 0.0.0.0:<port>` in the container, bind it to localhost/an
internal network, and front it with an authenticating reverse proxy (the
transport itself has no auth). Fill in the `<placeholder>` values in the compose
file for your own network and route.

## Command reference

| command | what |
|---|---|
| `thor remember` / `recall` / `get` / `history` | write / search / read facts |
| `thor courier` | per-prompt recall hook (reads hook JSON on stdin) |
| `thor guard` / `thor stop-guard` | moment-of-action / response advisories |
| `thor install` | write the hooks into settings.json |
| `thor vectors build \| sync \| status` | semantic sidecar (feature `semantic`) |
| `thor embed-daemon` | warm embedder for the courier (feature `semantic`) |
| `thor export` / `restore` / `backup` | JSONL backup + verified restore |
| `thor ship` / `recv` / `status` | cross-machine log-shipping sync |
| `thor fsck` | verify chain integrity + FTS projection |
| `thor mcp [--http <bind>]` | run as an MCP server (stdio or Streamable-HTTP) |

## Build features

- default: pure lexical (bm25) - no ML runtime, no extra dependencies.
- `semantic`: adds the dense score-fusion recall layer (ONNX embedder, warm
  daemon, precomputed sidecar). Client-only; a server/deploy build can stay on the
  default and never pull the ONNX runtime.

## Layout

```
thor/
  src/            the Rust crate (event store, recall, guard, sync, mcp, courier)
  examples/       recall_eval.rs - measure recall over a query battery
  deploy/         Dockerfile + docker-compose template
  tools/          helper scripts (repo ingest, side-by-side eval)
  *.example.json  guard rulebook templates (copy + fill in)
```

## License

GPLv3.
