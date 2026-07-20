# THOR remote MCP server for the NAS. Its own container - independent of mimir.
# Multi-stage: build the binary, then a slim runtime. Build context = the thor/
# crate (see docker-compose.yml: context is the parent of this deploy/ folder).
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --bin thor

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/thor /usr/local/bin/thor
VOLUME /data
EXPOSE 8078
# On first start, if there is no store yet but a backup export is mounted at
# /data/events.jsonl, restore it (proven bit-identical replay + hash check).
# Two optional replication roles, each gated on its env plus THOR_TOKEN (see
# SYNC-DEPLOY.md):
#   - AUTHORITY: set THOR_REPLICA_URL -> background `thor ship --watch` so this
#     store keeps a remote replica in sync (this container is the source).
#   - REPLICA: set THOR_RECV_BIND (e.g. 0.0.0.0:5555) -> background `thor recv`
#     so a remote authority can push its log into THIS store (this container is
#     the replica). This is the setup when a PC is the authority.
# Nothing enforces the "exactly one" rule: the two checks below are independent,
# so setting both starts a shipper AND a receiver in the same container.
# With neither set it is MCP-only, unchanged. Then serve Streamable-HTTP MCP. The
# MCP transport carries NO auth - keep 8078 off the LAN and front it with an
# authenticating reverse proxy; ship/recv both authenticate with the token.
#
# Both roles run in a restart loop rather than as a bare `&`. The MCP server is
# exec'd as PID 1, so after that nothing supervises a backgrounded role: if the
# receiver died the container stayed "healthy" while replication had silently
# stopped, which is the worst failure shape for a replica - it keeps answering
# recall, just with an ever more stale store. The loop restarts the role after a
# 5 second pause and prints one line per restart, so the reason is in the
# container log instead of being invisible. Restarting the CONTAINER is not an
# option here: that would take the MCP endpoint down with it.
CMD ["sh","-c","if [ ! -f /data/thor.db ] && [ -f /data/events.jsonl ]; then thor --db /data/thor.db restore --from /data/events.jsonl; fi; if [ -n \"$THOR_REPLICA_URL\" ] && [ -n \"$THOR_TOKEN\" ]; then (while : ; do thor --db /data/thor.db ship --to \"$THOR_REPLICA_URL\" --watch --interval 60; echo \"thor: shipper exited ($?), restarting in 5s\" >&2; sleep 5; done) & fi; if [ -n \"$THOR_RECV_BIND\" ] && [ -n \"$THOR_TOKEN\" ]; then (while : ; do thor --db /data/thor.db recv --http \"$THOR_RECV_BIND\"; echo \"thor: receiver exited ($?), restarting in 5s\" >&2; sleep 5; done) & fi; exec thor --db /data/thor.db mcp --http 0.0.0.0:8078"]
