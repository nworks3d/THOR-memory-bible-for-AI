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
# absent - nworks3d, N-Works 3D, NoizieWorks and the PayPal/YouTube links in
# README.md and .github/FUNDING.yml are meant to be public, so listing them
# would make this gate cry wolf and get ignored.
#
# When it fires, use the neutral stand-ins the 2026-07-23 rewrite established:
#     NW3D-Business      -> acme-shop
#     a real username    -> drop it (C:/Users/dev/...)
#     /volume1           -> /srv
#     ssh <name>@host    -> ssh admin@host
#     a private project  -> Investments, Sensor-Board, acme-shop

set -eu

PATTERN='yves|de boitselier|thevault|192\.168|printfarm|nw3d|bitwarden|/volume1|beleggingen|filament-station'

# This script names every token it forbids, so it must never scan itself.
if hits=$(git grep -nEi "$PATTERN" -- . ':(exclude)scripts/check-private-data.sh'); then
	echo "check-private-data: private identifiers in tracked files" >&2
	echo >&2
	echo "$hits" >&2
	echo >&2
	echo "These must not reach the public repository. See the header of" >&2
	echo "scripts/check-private-data.sh for the neutral stand-ins to use." >&2
	exit 1
fi

echo "check-private-data: clean"
