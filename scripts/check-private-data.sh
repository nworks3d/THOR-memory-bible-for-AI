#!/bin/sh
# Publication gate: refuse to let the maintainer's private identifiers reach the
# public repository.
#
# Why this is a gate and not a rule. On 2026-07-23 the entire git history was
# rewritten to scrub exactly these tokens. Three days later the same class was
# back, in fresh test fixtures. That is not carelessness: this project's
# realistic test data IS the maintainer's real memory store, so every fixture
# author is looking at private facts while typing. In a normal repository you
# have to go out of your way to leak; here you have to go out of your way not
# to. A habit loses that fight over time. A check that runs every time does not.
#
# Run it directly:
#     sh scripts/check-private-data.sh
#
# Install it as a pre-commit hook (do this once per clone):
#     ln -sf ../../scripts/check-private-data.sh .git/hooks/pre-commit
#
# Adding a token: add it to PATTERN below. Public brand names are deliberately
# absent - nworks3d, N-Works 3D, NoizieWorks and the Ko-fi/YouTube links in
# README.md and .github/FUNDING.yml are meant to be public, so listing them
# would make this gate cry wolf and get ignored. A SUBDOMAIN of that brand is a
# different thing: the name is public, but thor-mcp.nworks3d.be is a live
# endpoint, so the pattern matches <anything>.nworks3d.be and never the bare
# brand. Verified 2026-08-07: that form catches the real hosts and leaves every
# public link in README.md and .github/FUNDING.yml alone.
#
# When it fires, use the neutral stand-ins the 2026-07-23 rewrite established:
#     NW3D-Business      -> acme-shop
#     a real username    -> drop it (C:/Users/dev/...)
#     /volume1           -> /srv
#     ssh <name>@host    -> ssh admin@host
#     a private project  -> Investments, Sensor-Board, acme-shop

set -eu

# Scan the WHOLE repository, wherever this was invoked from. `git grep` limits
# itself to the current directory's subtree, so running this from thor2/ made
# it report "clean" while a store id sat in thor/src/cli.rs, unread. Same class
# of blind spot as the --untracked one below, and just as dangerous: a gate
# that passes on what it never looked at.
cd "$(git rev-parse --show-toplevel)"

PATTERN='yves|de boitselier|thevault|192\.168|printfarm|nw3d|bitwarden|/volume1|beleggingen|filament-station|[a-z0-9-]+\.nworks3d\.be'

# --untracked, not just the index. Found the hard way on 2026-08-07: the 2.0
# tree arrived as 161 files that were not committed yet, this gate reported
# clean, and it had not read a single one of them. A tree you are about to
# publish is exactly a tree that is not in the index yet, so scanning only what
# git already tracks checks the one state that was never the risk. Ignored
# files stay out either way - those never reach the remote.
#
# This script names every token it forbids, so it must never scan itself.
if hits=$(git grep -nEi --untracked "$PATTERN" -- . ':(exclude)scripts/check-private-data.sh'); then
	echo "check-private-data: private identifiers in files that would be published" >&2
	echo >&2
	echo "$hits" >&2
	echo >&2
	echo "These must not reach the public repository. See the header of" >&2
	echo "scripts/check-private-data.sh for the neutral stand-ins to use." >&2
	exit 1
fi

# Store entity ids, checked separately from PATTERN because they need different
# rules. An id is a pointer into the maintainer's private memory: it means
# nothing to a reader and everything to anyone who later gets a copy of the
# store. Case-SENSITIVE on purpose - Crockford base32 is uppercase, and folding
# case here would match ordinary lowercase identifiers and cry wolf.
#
# The ULID specification's own example value is excluded. It appears in this
# project's tests as documentation, not as data, and a gate that flags the
# spec's own example is a gate people learn to skip.
ULID_SHAPE='\b[0-9][0-9A-HJKMNP-TV-Z]{25}\b'
ULID_SPEC_EXAMPLE='01ARZ3NDEKTSV4RRFFQ69G5FAV'
if ids=$(git grep -nE --untracked "$ULID_SHAPE" -- . ':(exclude)scripts/check-private-data.sh' \
	| grep -v "$ULID_SPEC_EXAMPLE"); then
	echo "check-private-data: store entity ids in files that would be published" >&2
	echo >&2
	echo "$ids" >&2
	echo >&2
	echo "An id points into a private memory store. Say what was measured" >&2
	echo "without naming the entity, or redact it to <entity-id>." >&2
	exit 1
fi

echo "check-private-data: clean"
