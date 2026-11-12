# Release Process

This is the single source of truth for cutting a new EventExplorer release.
It combines the previous README "Releasing" section with the release rules
from [`AGENTS.md`](../AGENTS.md).

## Pre-flight

- The version in [`Cargo.toml`](../Cargo.toml) is the version you are about
  to ship. **Do not bump it before publishing** — the bump happens after
  the tarball is up, as the first commit of the next development cycle.
- Make sure `main` is clean: `git status` shows nothing, and everything
  you want in the release is pushed.

## 1. Build the artifacts

From the repo root:

```bash
./release.sh
```

By default this script produces a native tarball and a web zip. Use
`./release.sh --native-only` on macOS or FreeBSD when only the native
artifact is needed; this does not require Trunk or the wasm target.

**Native tarball** — `eventexplorer-<version>-<os>-<arch>.tar.gz`
(e.g. `linux-x86_64`, `freebsd-amd64`, `darwin-arm64`, or `darwin-x86_64`):

1. Builds both binaries with `cargo build --release`.
2. Strips them.
3. Stages them plus `assets/icon_*.png`, `event-explorer.desktop`
   (Linux/FreeBSD), `README.md`, and `LICENSE` under
   `/tmp/eventexplorer-<version>-<os>-<arch>/`.
4. Packages the staging directory into
   `eventexplorer-<version>-<os>-<arch>.tar.gz`.

On macOS it also runs [`scripts/package-macos.sh`](../scripts/package-macos.sh)
to include `EventExplorer.app`, with an `Info.plist` and Retina icon, and
ad-hoc signs the app and standalone binaries **after** stripping. Package
each architecture on its native Mac. This does not produce a universal
binary or an Apple-notarized release. Developer ID signing and notarization
are separate distribution steps if a release needs them.

**Web bundle zip** — `eventexplorer-<version>-web.zip`:

1. Builds the wasm bundle with `trunk build` (requires
   `rustup target add wasm32-unknown-unknown` and
   `cargo install --locked trunk` — script aborts with a help message
   if `trunk` isn't on `PATH`).
2. Stages `dist/` contents (the static site) plus
   [`assets/web-README.md`](../assets/web-README.md) (renamed to
   `README.md`) and `LICENSE`.
3. Packages the staging directory into `eventexplorer-<version>-web.zip`.

The web bundle is a `.zip` rather than a `.tar.gz` because static-site
distribution is web-conventional zip; the README inside walks the user
through the "you must serve this over HTTP" gotcha.

The script prints both artifact paths plus a ready-to-edit
`gh release create` command when it's done.

**FreeBSD tarball** — run `./release.sh --native-only` on a
FreeBSD host (or `cargo build --release`, then stage/strip/tar as
`eventexplorer-<version>-freebsd-<arch>.tar.gz`). Skip the web zip
there; the wasm bundle is produced once on Linux. Attach the FreeBSD
tarball to the same GitHub release as the Linux and web artifacts.

**macOS tarballs** — run `./release.sh --native-only` on an Apple Silicon
and/or Intel Mac. Attach the resulting `darwin-arm64` / `darwin-x86_64`
tarballs to the same release.

## 2. Write the changelog

Identify what changed since the previous release tag:

```bash
git log v<previous_version>..HEAD --oneline
```

Write a hand-written changelog grouped by area (GUI, CLI, parsing,
infrastructure/build) summarizing new features, bug fixes, and notable
changes. **Do not** use `--generate-notes` — the auto-generated list is
noisy and doesn't match our structure.

A typical layout:

```text
## GUI
- Add goodput span hover showing active-time vs wall-time ratio.
- Fix zoom desync when switching session tabs mid-drag.

## CLI
- New `gpcheck` subcommand to inspect GP_CHECK decisions in a time window.
- `rttanalysis` now prints per-session correlation distribution.

## Infrastructure
- Build and tarball moved to `./release.sh` at repo root.
```

## 3. Publish

```bash
GH_HOST=<host> gh release create v<version> \
  --repo <org>/<repo> \
  --title "v<version>" \
  --notes "<changelog text>" \
  eventexplorer-<version>-linux-x86_64.tar.gz \
  eventexplorer-<version>-freebsd-amd64.tar.gz \
  eventexplorer-<version>-web.zip
```

Replace `<host>`, `<org>/<repo>`, and `<version>` for your environment.
The end-of-run output from `./release.sh` prints a starting command you
can copy and amend. Both artifacts are attached to the same release.

## 4. Bump the development version

*Only after the release is up:*

1. Edit [`Cargo.toml`](../Cargo.toml) — increment the `version` field to
   the next development version (typically patch + 1).
2. `cargo check --release` to refresh `Cargo.lock`.
3. Commit with a message like `Bump version to <next> for next development cycle`.
4. Push.

## Checklist

```
[ ] Cargo.toml version matches the release I want to ship
[ ] Tree is clean and pushed
[ ] ./release.sh completed successfully (both tarball and web zip produced)
[ ] Changelog written by hand, grouped by area
[ ] gh release create … succeeded with both artifacts attached
[ ] Cargo.toml version bumped for next dev cycle
[ ] Bump commit pushed
```
