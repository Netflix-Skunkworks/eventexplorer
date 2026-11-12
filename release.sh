#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_ROOT"

NATIVE_ONLY=false
case "${1:-}" in
    --native-only) NATIVE_ONLY=true ;;
    "") ;;
    *) echo "Usage: $0 [--native-only]" >&2; exit 1 ;;
esac
if [ "$#" -gt 1 ]; then
    echo "Usage: $0 [--native-only]" >&2
    exit 1
fi

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
ARCH=$(uname -m)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
TAG="v${VERSION}"

# ---------------------------------------------------------------------------
# 1. Native release tarball
# ---------------------------------------------------------------------------

NAME="eventexplorer-${VERSION}-${OS}-${ARCH}"
STAGING="/tmp/${NAME}"
TARBALL="${REPO_ROOT}/${NAME}.tar.gz"

echo "==> Building native release (v${VERSION}, ${ARCH})"
cargo build --release

echo "==> Assembling ${NAME}"
rm -rf "$STAGING"
mkdir -p "$STAGING/assets"

cp target/release/event_explorer "$STAGING/"
cp target/release/elog_cli        "$STAGING/"
cp assets/icon_*.png              "$STAGING/assets/"
if [ "$OS" = darwin ]; then
    ./scripts/package-macos.sh "$STAGING/EventExplorer.app"
else
    cp event-explorer.desktop "$STAGING/"
fi
cp README.md                      "$STAGING/"
cp LICENSE                        "$STAGING/"

strip "$STAGING/event_explorer" "$STAGING/elog_cli"
if [ "$OS" = darwin ]; then
    # strip invalidates Mach-O signatures, including the mandatory signature
    # on Apple Silicon. Sign the standalone CLI/GUI copies as well as the app.
    codesign --force --sign - "$STAGING/event_explorer"
    codesign --force --sign - "$STAGING/elog_cli"
fi

echo "==> Creating ${TARBALL}"
tar -czf "$TARBALL" -C /tmp "$NAME"
rm -rf "$STAGING"

if [ "$NATIVE_ONLY" = true ]; then
    echo "==> Native tarball: ${TARBALL}"
    exit 0
fi

# ---------------------------------------------------------------------------
# 2. Web bundle (wasm32) — packaged as a `.zip` since static-site
#    distribution is web-conventional zip rather than tar.gz.
# ---------------------------------------------------------------------------

WEB_NAME="eventexplorer-${VERSION}-web"
WEB_STAGING="/tmp/${WEB_NAME}"
WEB_ZIP="${REPO_ROOT}/${WEB_NAME}.zip"

echo "==> Building web bundle (wasm32)"
if ! command -v trunk >/dev/null 2>&1; then
    echo "Error: trunk not found in PATH."
    echo "Install with:"
    echo "  rustup target add wasm32-unknown-unknown"
    echo "  cargo install --locked trunk"
    exit 1
fi

# A concurrent `trunk serve` (the dev server) silently races our build
# for ownership of `dist/`: the dev server's file-watcher rebuilds on
# every Cargo.toml / source touch, which can leave `dist/.stage/`
# half-applied or overwrite our artifacts mid-zip. The result is a
# subtly broken release zip — typically missing the wasm-bindgen `.js`
# glue or shipping the source `index.html` instead of the trunk-
# processed one. Refuse to run while one is up.
trunk_serve_pids=$(pgrep -f "^trunk serve" || true)
if [ -n "$trunk_serve_pids" ]; then
    echo "Error: a 'trunk serve' is already running (pid: $trunk_serve_pids)."
    echo "Stop it first — its file-watcher rebuilds dist/ concurrently"
    echo "and corrupts the release zip:"
    echo "    kill $trunk_serve_pids"
    exit 1
fi

rm -rf "$REPO_ROOT/dist"
NO_COLOR=false trunk build --release

# Sanity-check the build actually produced what we expect at the root
# of dist/: a wasm-bindgen `.js` glue file, a `_bg.wasm`, and a
# processed `index.html`. Catches any future trunk regression where
# the asset-merge step skips a file. (`.stage/` is allowed to linger
# empty — trunk doesn't always remove the directory.)
if ! compgen -G "$REPO_ROOT/dist/*.js" >/dev/null \
   || ! compgen -G "$REPO_ROOT/dist/*_bg.wasm" >/dev/null \
   || ! [ -f "$REPO_ROOT/dist/index.html" ]; then
    echo "Error: trunk build did not emit a complete dist/ tree."
    ls -la "$REPO_ROOT/dist/" || true
    exit 1
fi

echo "==> Assembling ${WEB_NAME}"
rm -rf "$WEB_STAGING"
mkdir -p "$WEB_STAGING"
cp -r "$REPO_ROOT/dist/." "$WEB_STAGING/"
# Use the bundled web README (how-to-run guide) rather than the
# project's main README.
cp assets/web-README.md  "$WEB_STAGING/README.md"
cp LICENSE               "$WEB_STAGING/"

echo "==> Creating ${WEB_ZIP}"
rm -f "$WEB_ZIP"
( cd /tmp && zip -qr "$WEB_ZIP" "$WEB_NAME" )
rm -rf "$WEB_STAGING"

# ---------------------------------------------------------------------------
# 3. Summary
# ---------------------------------------------------------------------------

echo ""
echo "==> Native tarball:  ${TARBALL}"
echo "==> Web bundle zip:  ${WEB_ZIP}"
echo ""
echo "These are local test packages. Releases are published by CI when a"
echo "v${VERSION} tag is pushed; see docs/releasing.md."
