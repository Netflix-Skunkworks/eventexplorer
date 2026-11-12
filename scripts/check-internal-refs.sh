#!/usr/bin/env bash
# Fail when tracked files name internal hosts, tools or tickets. Site-specific
# values belong in a site config (docs/site-config.md), not in the tree.
set -euo pipefail
cd "$(dirname "$0")/.."

PATTERN='netflix\.net|nflxvideo|metatron|tseriesui|ocdata|elog-access|sde-oc-batch|ocadev|full-tunnel|\bNCD-[0-9]|github\.netflix|corp/[a-z]'
if git grep -nIiE "$PATTERN" -- . ':!Cargo.lock' ':!scripts/check-internal-refs.sh'; then
    echo "error: internal references above; move them to a site config" >&2
    exit 1
fi
