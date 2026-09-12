#!/bin/sh
# Extract one version's section from CHANGELOG.md: from its "## <version>"
# heading up to (not including) the next "## " heading, heading line included.
#
# Usage: changelog-section.sh <tag-or-version> [changelog-file]
#   scripts/changelog-section.sh v2.4.0
#   scripts/changelog-section.sh 2.4.0 CHANGELOG.md
#
# A leading "v" on the first argument is stripped, so it takes a git tag
# directly. Prints the matching section to stdout. When CHANGELOG.md has no
# section for that version, prints nothing to stdout, a clear reason to
# stderr, and exits 1 - the release workflow relies on that exit code to
# refuse publishing.

set -eu

if [ "$#" -lt 1 ]; then
	echo "changelog-section: usage: changelog-section.sh <tag-or-version> [changelog-file]" >&2
	exit 1
fi

version="${1#v}"
file="${2:-CHANGELOG.md}"

section=$(awk -v ver="## $version" '
	index($0, ver) == 1 && (length($0) == length(ver) || substr($0, length(ver) + 1, 1) == " ") {
		found = 1
		print
		next
	}
	found && index($0, "## ") == 1 { exit }
	found { print }
' "$file")

if [ -z "$section" ]; then
	echo "CHANGELOG.md has no section for $version - a release does not ship without saying what changed" >&2
	exit 1
fi

printf '%s\n' "$section"
