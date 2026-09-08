#!/bin/sh
# THOR 2.0's memory server, container entry point. Built into
# deploy/Dockerfile.mcp; not used by deploy/Dockerfile (the NAS replica
# image), which keeps its own default of starting the receiver - see that
# Dockerfile's header for why, and deploy/docker-compose.yml's thor2-recv
# service for the explicit command that pins the receiver role regardless of
# either image's own default.
#
# WHAT THIS FIXES. `mcp` opens an EXISTING store and refuses a missing one on
# purpose (see mcp/src/bin/mcp.rs) - the same refusal a typo'd --db path
# deserves. A container starts from an empty volume, so without this step,
# `docker run` against a fresh volume answered nothing a tool-calling client
# could use: "no THOR store at ... this command never creates one".
#
# `initstore` runs first and does exactly what `install` does to a brand new
# store - ensure_store, seed_working_contract, seed_response_rulebook, via
# ops::install::ensure_and_seed_store - and nothing else: no settings.json, no
# tool-server registration, nothing that touches an assistant's own
# configuration. A store that is already there is opened by nothing here and
# left completely alone; only the first start against an empty volume takes
# this branch at all.
#
# initstore's own status lines go to stderr (see its own doc comment): stdout
# is `mcp`'s protocol channel from the moment exec below hands it over, and a
# plain-text line ahead of the first JSON-RPC message would be a stray line on
# a stream a client reads as nothing but JSON-RPC.
set -e

if [ ! -f "$THOR_DB" ]; then
  mkdir -p "$(dirname "$THOR_DB")"
  /usr/local/bin/initstore --db "$THOR_DB" 1>&2
fi

exec /usr/local/bin/mcp --db "$THOR_DB" "$@"
