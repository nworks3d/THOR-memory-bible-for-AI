#!/bin/sh
# CI gate: the workspace version in thor2/Cargo.toml must have a matching
# CHANGELOG.md section, so a version bump cannot land without saying what
# changed. Reuses changelog-section.sh - the one place that knows the
# CHANGELOG.md heading format - rather than re-parsing it here.
#
# Usage: changelog-check.sh [cargo-toml] [changelog-file]
#   scripts/changelog-check.sh
#   scripts/changelog-check.sh thor2/Cargo.toml CHANGELOG.md

set -eu

cargo_toml="${1:-thor2/Cargo.toml}"
changelog="${2:-CHANGELOG.md}"
script_dir=$(dirname -- "$0")

version=$(grep -m1 '^version = ' "$cargo_toml" | sed -E 's/^version = "(.*)"$/\1/')

if [ -z "$version" ]; then
	echo "changelog-check: no version line found in $cargo_toml" >&2
	exit 1
fi

if ! sh "$script_dir/changelog-section.sh" "$version" "$changelog" >/dev/null 2>&1; then
	echo "thor2/Cargo.toml says $version but CHANGELOG.md has no section for it - write what changed before bumping" >&2
	exit 1
fi

echo "changelog-check: $changelog has a section for $version"
