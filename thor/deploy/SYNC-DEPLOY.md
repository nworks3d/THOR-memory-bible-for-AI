# THOR live replication (log shipping)

THOR keeps one **authority** store on a machine's local disk. Other machines run
a **replica** that the authority pushes its event log into, so they serve fresh
recall without ever sharing a database file over the network (a shared network
`thor.db` corrupts the WAL - `thor` refuses UNC/NFS paths for exactly this).

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

Set exactly one. Leave the other empty.

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
thor status --to http://<container-host>:<recv-port>
```

It prints the local tip, the replica tip, and the current lag (or that the
replica is unreachable - an honest degraded RPO). Lag should settle at 0. Confirm
from the connector side that recall now returns the authority's newest facts.

## Topology B: the container is the authority

Mirror image: set `THOR_REPLICA_URL` on the container (not `THOR_RECV_BIND`), run
`thor recv` on the replica machine, and re-seed the replica from the container's
export. Same token, same verification.

## Caveats

- **The replica is effectively read-oriented.** If clients write to the replica's
  MCP (e.g. a `remember` from the mobile/web connector) while the authority also
  ships, the two hash chains diverge and the next reconcile is rejected until the
  replica is re-seeded. In an authority+replica setup, route durable writes to the
  authority. A future bidirectional mode would need both ends to ship+recv with
  conflict handling; that is out of scope here.
- **Token hygiene.** The token is the only auth on the transport. Keep it out of
  git (this repo carries placeholders), rotate it by setting the new value on both
  ends, and keep the recv port off the public internet.
- **Never a shared network `thor.db`.** Replication ships the *log*; it never opens
  the same database file from two machines. That path silently corrupts the WAL.
