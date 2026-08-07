# THOR 2.0 replica for the NAS: the receiving half of the log-shipping
# transport, plus the connector a session away from the main machine talks to.
#
# WHY THERE IS A WRITE SURFACE HERE NOW, WHEN THERE DELIBERATELY WAS NOT.
# A replica's job is to hold a byte-identical copy of the authority's chain,
# and giving it a surface that APPLIES writes is how a replica becomes a
# second authority and forks that chain - the failure that blocked sync in
# July 2026, and the reason this image shipped the receiver alone.
#
# The connector added here does not apply writes. `mcp --http` refuses to
# open a port without `--capture-inbox`, and with one it never touches the
# store: remember, revise and retract are appended to that file, and the
# authority drains it (`sync drain`) and replays them into ITS log. So the
# chain here is still only ever written by `sync recv`, exactly as before.
# The old comment said a replica must not be able to write; that is still
# true, and it is now enforced by the binary rather than by leaving it out.
#
# No `semantic` feature: that pulls in ONNX, which does not run on this NAS
# (the same reason 1.0's image is built without it). Meaning-based search is
# a lookup surface on the authority; a replica answers on the letter.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# The receiver and the connector. Still not the whole workspace: that would
# drag in the model runtime for no reason on a machine that will never run it.
RUN cargo build --release --bin sync --bin mcp

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/sync /usr/local/bin/sync
COPY --from=build /src/target/release/mcp /usr/local/bin/mcp
VOLUME /data

# The shared secret arrives as THOR_SYNC_TOKEN (env, never a CLI flag: a
# secret on the command line ends up in `ps` and in shell history). The
# receiver refuses to start without it rather than listening unauthenticated.
ENV THOR_SYNC_TOKEN=""
EXPOSE 5555 5557

# Default role: the receiver. The connector runs from the same image with its
# command overridden - see deploy/docker-compose.yml.
#
# The store is created on first receipt; nothing here creates an empty one.
# A shipment that does not continue this chain is refused with a reason and
# nothing is written - see ops::transport.
#
# --inbox is what makes captured writes reachable: without it this receiver
# says plainly that it captures nothing, and a drain against it is refused
# rather than reporting a healthy zero forever.
CMD ["/bin/sh", "-c", "exec /usr/local/bin/sync recv --db /data/thor.db --bind 0.0.0.0:5555 --inbox /data/inbox.jsonl"]
