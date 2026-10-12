#!/bin/sh
# Print the CHANGELOG.md entries of one version, without the heading.
#
#   changelog.sh 0.2.0            entries under "## [0.2.0] - ..."
#   changelog.sh Unreleased       entries under "## [Unreleased]"
#   changelog.sh 0.2.0 other.md   read another file
#
# Exits 1 when the section is missing or has nothing in it, so the release
# workflow refuses to publish a version whose changes were never written down.
set -eu

version="${1:?usage: changelog.sh <version|Unreleased> [file]}"
file="${2:-CHANGELOG.md}"
version="${version#v}"

section=$(awk -v heading="## [$version]" '
  /^## / {
    if (found) { exit }
    if (index($0, heading) == 1) { found = 1 }
    next
  }
  found { print }
' "$file")

if [ -z "$(printf '%s' "$section" | tr -d '[:space:]')" ]; then
  echo "$file has no entries under [$version]" >&2
  exit 1
fi
printf '%s\n' "$section"
