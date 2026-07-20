# THOR live replication (log shipping)

THOR keeps one **authority** store on a machine's local disk. Other machines run
a **replica** that the authority pushes its event log into, so they serve fresh
recall without ever sharing a database file over the network (a shared network
`thor.db` corrupts the WAL). On Windows `thor` refuses to open a store over a UNC
path for exactly this reason; on Linux and macOS there is no such check, so an
NFS or SMB mount is your responsibility - never point a store at one.

Replication is a thin, append-only reconcile: the authority's `thor ship` sends
new events; the replica's `thor recv` verifies each hash and appends. Both ends
authenticate with one shared bearer token. The transport has no other auth, so
the recv port must only be reachable by the authority (LAN/tailnet is fine
because of the token; do not expose it to the open internet).

## Roles

The container (`deploy/Dockerfile` + `deploy/docker-compose.yml`) runs one of two
optional roles, each gated on its env var **plus** `THOR_TOKEN`:

| Role | Env to set | It runs | Meaning |
|------|-----------|---------|---------|
| AUTHORITY | `THOR_REPLICA_URL` | `thor ship --watch --to <url>` | this store is the source; it pushes to a remote replica |
| REPLICA | `THOR_RECV_BIND` | `thor recv --http <bind>` | a remote authority pushes into this store |
| (neither) | - | MCP only | unchanged, no replication |

Set exactly one. Leave the other empty. Nothing enforces this: the entrypoint
checks the two variables independently, so setting both starts a shipper AND a
receiver in the same container. Keeping them exclusive is up to you.

## Topology A: a PC is the authority, the container is the replica

This is the common case: real work happens on a workstation (its local `thor mcp`
stdio server + the courier/Guard hooks write to the PC's `thor.db`), and the
container exists to serve a remote MCP connector (mobile/web) with fresh recall.

```
  PC (authority)                         container (replica)
  thor.db  --- thor ship --watch --->    thor recv  ---> /data/thor.db
  (local stdio MCP + hooks write here)   (thor mcp --http serves recall from here)
```

### 1. Generate a shared token (once)

Any long random string. Put it in the container env **on the host only** and pass
it to the PC's `thor ship`. Never commit it.

### 2. Container = replica

In your host-local `docker-compose.override.yml` (or the compose you keep on the
host, gitignored), set:

```yaml
services:
  thor-mcp:
    environment:
      THOR_TOKEN: "<shared-token>"
      THOR_RECV_BIND: "0.0.0.0:<recv-port>"
      THOR_REPLICA_URL: ""     # keep empty in replica mode
    ports:
      - "<recv-port>:<recv-port>"   # reachable by the PC; bearer-gated
```

### 3. Re-seed the replica so histories match (important, one time)

Log shipping only appends when the replica's log is a **prefix** of the
authority's. A replica seeded from a different export (or that took its own local
writes) has a divergent hash chain and `recv` will reject the push. Reset it to
the authority's current state first:

1. On the PC (authority), export the log:
   `thor export --out events.jsonl`
2. Stop the container. Replace the replica store with a fresh restore of that
   export: remove `/data/thor.db*` (the `.db`, `-wal`, `-shm`) and drop the fresh
   `events.jsonl` at `/data/events.jsonl`. On next start the entrypoint restores
   it (bit-identical replay + hash check) because `thor.db` is now absent.
3. Start the container. It restores, then `recv` is ready and in sync.

### 4. PC = authority: run `thor ship --watch`

Run this as a background service / scheduled task on the PC so new events keep
flowing (token via `--token` or the `THOR_TOKEN` env - never on a shared shell
history in the clear):

```
thor ship --to http://<container-host>:<recv-port> --watch --interval 60
```

### 5. Verify

From the PC:

```
thor status --to http://<container-host>:<recv-port> --token <shared-token>
```

Pass the token (or export `THOR_TOKEN` first): without it the receiver answers
401 and `status` reports the replica as UNREACHABLE, which looks like a network
problem but is not one.

It prints the local tip, the replica tip, and the current lag (or that the
replica is unreachable - an honest degraded RPO). Once the replica has caught up
the line reads `replica: contiguous_seq <n> (reachable) - in sync`. Confirm from
the connector side that recall now returns the authority's newest facts.

## Topology B: the container is the authority

Mirror image: set `THOR_REPLICA_URL` on the container (not `THOR_RECV_BIND`), run
`thor recv` on the replica machine, and re-seed the replica from the container's
export. Same token, same verification.

## Capture inbox: writing from a replica without forking

The replica's log must stay a strict prefix of the authority's - that is what makes
`recv` a verbatim, hash-verified copy. So a `remember` / `revise` / `retract` sent
to the replica's MCP (e.g. from a phone whose only always-on endpoint is the
container) would fork the chain and block the next ship. The capture inbox lets
those writes happen anyway without ever touching the log:

- Set `THOR_CAPTURE_INBOX=/data/inbox.jsonl` on the **replica** container. Its MCP
  server then *diverts* every write to that append-only file instead of appending
  to `thor.db`, and answers the client `queued to capture inbox (pending sync)`.
  Reads (recall / get) are unchanged. The stdio (local authority) server never
  diverts - only the HTTP server reads this env.
- On the **authority**, drain the inbox back into the real log with
  `thor drain-inbox`: it replays each captured op as a proper event, preserving
  the entity id so revisions chain correctly, and re-running the same duplicate
  check `remember` does (so a re-drain skips what is already there).

Wire it into the authority's ship job so captures round-trip automatically. Because
the drain mints the facts on the authority, the next ship replicates them straight
back to the container the normal, non-forking way.

**Use the HTTP route.** One command does the whole round trip over the same
bearer-gated port the ship already uses, so the job needs no ssh, no file share
and no second credential:

```
thor drain-inbox --from http://<container-host>:<recv-port> --token <shared-token>
thor ship --to http://<container-host>:<recv-port> --token <shared-token>
```

`--from` rotates the replica's inbox, pulls the pending batch, applies it locally,
and only then acknowledges - so a batch that fails to apply is served again on the
next run rather than lost.

By hand, if you cannot reach the port (the same five steps the `--from` route does
for you):

1. rotate the inbox on the replica so new captures land in a fresh file:
   `mv /data/inbox.jsonl /data/inbox.draining.jsonl` (skip if absent/empty)
2. fetch `inbox.draining.jsonl` to the authority
3. `thor drain-inbox --inbox inbox.draining.jsonl`
4. on success, delete `inbox.draining.jsonl` on the replica
5. ship as usual

**Trade-off:** a capture is not visible in the replica's own recall until the next
drain+ship (bounded by the ship interval). It is a capture channel, not a live
write - the price of keeping one lossless, hash-verified chain.

## Caveats

- **The replica is effectively read-oriented.** A write sent straight to the
  replica's MCP (e.g. a `remember` from the mobile/web connector) while the
  authority also ships would diverge the two hash chains and block the next
  reconcile until the replica is re-seeded. Either route durable writes to the
  authority, or use the capture inbox above so replica-side writes are queued and
  replayed on the authority instead of forking the log. A fully bidirectional mode
  (both ends ship+recv with conflict handling) is out of scope here.
- **Token hygiene.** The token is the only auth on the transport. Keep it out of
  git (this repo carries placeholders), rotate it by setting the new value on both
  ends, and keep the recv port off the public internet.
- **Never a shared network `thor.db`.** Replication ships the *log*; it never opens
  the same database file from two machines. That path silently corrupts the WAL.
