#!/usr/bin/env bash
# Package one CI build: package-ci.sh <platform-name> <cargo-release-dir>.
# Writes dist-ci/eventexplorer-<version>-<platform>.{tar.gz,zip}.
set -euo pipefail

PLATFORM="$1"
BIN_DIR="$2"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
NAME="eventexplorer-${VERSION}-${PLATFORM}"
STAGE="dist-ci/${NAME}"
EXE=""
case "$PLATFORM" in windows-*) EXE=".exe" ;; esac

rm -rf "$STAGE"
mkdir -p "$STAGE/assets"
cp "$BIN_DIR/event_explorer$EXE" "$BIN_DIR/elog_cli$EXE" "$STAGE/"
cp assets/icon_*.png "$STAGE/assets/"
cp README.md LICENSE "$STAGE/"

case "$PLATFORM" in
    macos-*)
        strip "$STAGE/event_explorer" "$STAGE/elog_cli"
        codesign --force --sign - "$STAGE/event_explorer"
        codesign --force --sign - "$STAGE/elog_cli"
        # package-macos.sh reads the GUI binary from target/release.
        if [ "$BIN_DIR" != target/release ]; then
            mkdir -p target/release
            cp "$BIN_DIR/event_explorer" target/release/event_explorer
        fi
        ./scripts/package-macos.sh "$STAGE/EventExplorer.app"
        ;;
    windows-*) ;;
    *)
        strip "$STAGE/event_explorer" "$STAGE/elog_cli"
        cp event-explorer.desktop "$STAGE/"
        ;;
esac

case "$PLATFORM" in
    windows-*) (cd dist-ci && 7z a -tzip -bso0 "${NAME}.zip" "$NAME") ;;
    *) tar -czf "dist-ci/${NAME}.tar.gz" -C dist-ci "$NAME" ;;
esac
rm -rf "$STAGE"
