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
# /data/events.jsonl, restore it (proven bit-identical replay + hash check). If
# THOR_REPLICA_URL and THOR_TOKEN are BOTH set, also background the reconcile ship
# loop so this authority keeps a PC replica in sync (see SYNC-DEPLOY.md); with
# them unset it is MCP-only, unchanged. Then serve Streamable-HTTP MCP. The MCP
# transport carries NO auth - keep 8078 off the LAN and front it with the
# Cloudflare Access gate (see DEPLOY.md); the ship loop authenticates with the token.
CMD ["sh","-c","if [ ! -f /data/thor.db ] && [ -f /data/events.jsonl ]; then thor --db /data/thor.db restore --from /data/events.jsonl; fi; if [ -n \"$THOR_REPLICA_URL\" ] && [ -n \"$THOR_TOKEN\" ]; then thor --db /data/thor.db ship --to \"$THOR_REPLICA_URL\" --watch --interval 60 & fi; exec thor --db /data/thor.db mcp --http 0.0.0.0:8078"]
