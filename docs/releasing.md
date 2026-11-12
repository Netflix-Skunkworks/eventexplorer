# Release Process

Releases are built and published by CI from a version tag. Nothing is
built or uploaded by hand.

## 1. Prepare

- [`Cargo.toml`](../Cargo.toml) `version` is the version you are about
  to ship. The tag must match it (`v<version>`); CI fails the release
  otherwise.
- In [`CHANGELOG.md`](../CHANGELOG.md), rename `## [Unreleased]` to
  `## [<version>] - <YYYY-MM-DD>` and add a fresh empty `## [Unreleased]`
  above it. That section becomes the release notes (CI falls back to
  `[Unreleased]` if the version has no section). Group entries by area
  (GUI, CLI, infrastructure) under Keep a Changelog headings.
- `main` is clean and pushed, and CI on it is green.

## 2. Tag

```bash
git tag v<version>
git push origin v<version>
```

The tag's CI run (`.github/workflows/ci.yml`) then:

1. checks the tag against `Cargo.toml`, then runs fmt, clippy, tests and
   the internal-reference gate;
2. builds Linux x86_64/aarch64, macOS arm64/x86_64 (tarball with
   `EventExplorer.app`), Windows x86_64, FreeBSD x86_64 and the web
   bundle;
3. publishes release `v<version>` with every archive, a `SHA256SUMS`
   file and the CHANGELOG section as notes.

Re-running the tag's workflow re-uploads the assets and refreshes the
notes.

## 3. Bump the development version

After the release is out, bump `Cargo.toml` to the next version and
commit.

## Local builds

`./release.sh` still builds a native tarball and the web zip on the
current machine, for testing a package before tagging.
`scripts/release-notes.sh <version>` prints the notes CI will use.

## Site-specific builds

Release binaries carry no site config. A deployment that wants its own
sharing, symbol catalog or firmware links adds a `site.toml` (see
[site-config.md](site-config.md)): installed per user with
`elog_cli site install`, placed next to the binaries in a repackaged
archive, or next to `index.html` for the web bundle.

## Checklist

```
[ ] Cargo.toml version = the release version
[ ] CHANGELOG section for the version
[ ] main green
[ ] tag v<version> pushed
[ ] release published with all archives + SHA256SUMS
[ ] Cargo.toml bumped for the next cycle
```
