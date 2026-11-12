#!/usr/bin/env bash
# Print the CHANGELOG.md section for <version>, else the [Unreleased] section.
set -euo pipefail
VERSION="${1:?usage: $0 <version>}"
VERSION="${VERSION#v}"
cd "$(dirname "$0")/.."

section() {
    awk -v head="$1" '
        index($0, "## [" head "]") == 1 { on = 1; next }
        on && /^## \[/ { exit }
        on { print }
    ' CHANGELOG.md
}

notes=$(section "$VERSION")
[ -n "$(echo "$notes" | tr -d '[:space:]')" ] || notes=$(section Unreleased)
[ -n "$(echo "$notes" | tr -d '[:space:]')" ] || { echo "error: no CHANGELOG notes for $VERSION" >&2; exit 1; }
printf '%s\n' "$notes" | sed -e '/./,$!d'
