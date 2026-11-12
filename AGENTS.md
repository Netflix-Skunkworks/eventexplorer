# Agent Instructions

These instructions apply to any AI coding agent working in this repo. They
are deliberately short — the full context lives in
[`docs/architecture.md`](docs/architecture.md).

## Orientation

EventExplorer is a Rust desktop + CLI tool for analyzing FreeBSD EventLog
(`.elog`) files. Any kernel subsystem that registers as an EventLog
provider is in scope; today the `tcp` provider has deep decoding and GUI
views, the `hpts` provider contributes NIC TX-rate samples, and the
`fanout_node` user-space provider (older captures: `ella_relay`)
contributes per-channel and sampled-count views. The parser and UI are
designed to be extended to new providers as the kernel adopts them.

Two binaries and one optional web target share one parser. The codebase
is organized as a library (`src/lib.rs`) with the binaries as thin shims:

- `event_explorer` (GUI) — [`src/main.rs`](src/main.rs) + the
  [`src/app/`](src/app) module tree (`EventExplorerApp` in
  `src/app.rs` is now a directory: `state.rs`, `update.rs`, per-view
  files, and leaf helpers like `colors.rs`/`format.rs`/`plot.rs`).
- `elog_cli` (CLI) — [`src/cli.rs`](src/cli.rs) is a thin `fn main`
  shim over the [`src/cli/`](src/cli) library module
  (`cli::run` dispatch in `mod.rs`; `cmd_*` grouped into `summary.rs`,
  `rtt.rs`, `goodput.rs`, `events.rs`, `interfaces.rs`, `flame.rs`,
  `ella.rs`).
- Web build (wasm32) — [`src/lib.rs`](src/lib.rs) exports `WebHandle`;
  same `EventExplorerApp`, runs in a browser tab. See the
  [Web target section](docs/architecture.md#web-target-wasm32) of
  architecture.md.
- Site config — [`src/site.rs`](src/site.rs): optional share server,
  symbol catalog, HTTP command, and firmware repo (`ELOG_*` env or a
  `site.toml`; see
  [`docs/site-config.md`](docs/site-config.md)). Unset keys turn those
  features off, so never hard-code an endpoint.
- Shared parser — the [`src/elog_parser/`](src/elog_parser) module tree
  (`constants.rs`, `names.rs`, `types.rs`, `bytes.rs`, `payload.rs`,
  `core.rs`, `io.rs`, `progress.rs`).
- Higher-level analytics — the [`src/analysis/`](src/analysis) module
  tree (`session.rs`, `rate.rs`, `limiter.rs`, `goodput.rs`, `cpu.rs`,
  `flame.rs`), consumed by both the GUI and CLI. The parser never
  depends on `analysis`.

Read [`docs/architecture.md`](docs/architecture.md) before making any
non-trivial change. It has a module map, data-flow diagram, and "how do I
add X" recipes.

## Build configuration

Always **build** in release mode unless explicitly asked otherwise. Do
**not** substitute `cargo check` for a build — the user needs the
actual binary to test your changes.

- Use `cargo build --release` (or `cargo run --release`) to verify any
  code change. This is the default — always run it after edits.
- Do **not** use `cargo check` (even `cargo check --release`) as the
  verification step. `check` only type-checks; it does not produce the
  `target/release/event_explorer` / `target/release/elog_cli` binaries
  the user runs to test the change.
- The only exception is when the user explicitly asks for a
  type-check-only pass (e.g. "just run cargo check"). Otherwise, build.
- Only use debug (dev) mode when the user explicitly requests it
  (e.g. "build debug", "cargo build without release").
- After committing changes, always finish with a fresh
  `cargo build --release` so `target/release/event_explorer` is the
  binary the user relaunches. Never leave the release binary stale
  behind the source.

### Web (wasm32) target

Optional. Verify with `trunk build` (or `trunk serve` for a live dev
server at http://127.0.0.1:8080/). **Do not** use raw `cargo build
--target wasm32-unknown-unknown` without `--no-default-features` — the
default feature `native-bins` requires the desktop bins which won't
compile for wasm.

Don't run `trunk build` unless the user is asking about web behavior or
has touched code in `src/lib.rs`'s `WebHandle`, the wasm-only branches
in `src/app/`, or `index.html` / `Trunk.toml`. For ordinary
parser/GUI/CLI work, `cargo build --release` is enough — the wasm
build will keep working as long as you respect the cfg pattern below.

## Pre-commit checks (required)

CI runs `cargo fmt --all -- --check`, `cargo clippy --all-targets
--locked -- -D warnings`, and `cargo test --locked`. Run all three
locally and get them green **before every commit** — most CI failures on
this repo come from skipping this step, and formatting is the most
common break.

```bash
cargo fmt --all                                    # write, don't just check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release
```

- Run `cargo fmt --all` *after* your last edit. Hand-edited code that
  looks fine still trips rustfmt's line wrapping.
- `-D warnings` means a new clippy lint fails CI. Fix the lint; don't
  `#[allow]` it without a reason in the code.
- If a check can't run in your environment, say so explicitly in your
  summary instead of assuming CI will pass.

## Common gotchas

- **New event types touch three places.** Adding a TCP event requires:
  (1) an `EVT_*` constant in `src/elog_parser/constants.rs`, (2) a
  `parse_payload` arm in `src/elog_parser/payload.rs`, (3) an
  `event_name` entry in `src/elog_parser/names.rs` — plus (optionally)
  the retained set in `retain_plot_events` (`src/analysis/session.rs`).
  See the
  [recipe in architecture.md](docs/architecture.md#add-a-new-event-type).
- **New providers are a larger effort.** Adding support for a provider
  other than `tcp` / `hpts` involves resolving its id in
  `parse_elog_core` (`src/elog_parser/core.rs`), routing its events, and
  defining whether they plug into the per-session map or a separate
  global stream. See
  [architecture.md](docs/architecture.md#add-support-for-a-new-provider).
- **RTT values ≥ `i32::MAX` are underflow artifacts.** Filter them out of
  any new RTT analytics.
- **Custom zoom only.** The per-session plots disable `egui_plot`'s built-in
  zoom/scroll. Don't re-enable it — `view_x` is the single source of truth
  for the shared time axis.
- **Every plot widget needs chrome.** Maximize button (upper right) and
  an 8 px sizer under the plot. One catalog `PlotId` wrapping several
  `Plot`s is not enough — call `finish_plot_slot` per widget. See
  [Plot chrome (required)](docs/architecture.md#plot-chrome-required).
- **`retain_plot_events` is destructive.** It (in `src/analysis/session.rs`)
  drops non-plot event variants in the GUI path. If you need a variant for
  plots, add it to the retained set; if you need it only during load,
  compute it before retention.
- **Cross-target cfg pattern.** Anything that touches the filesystem as
  paths (`std::fs::File`, mmap), spawns processes (`std::process::Command`,
  ssh), or relies on real threads (`std::thread::spawn`, rayon's
  `par_iter*`) must be `#[cfg(not(target_arch = "wasm32"))]`. If the
  function is called from cross-platform code, provide a `cfg(wasm32)`
  no-op stub. See the [Add a desktop-only feature](docs/architecture.md#add-a-desktop-only-feature)
  recipe.

## Verifying GUI changes

Don't ask the user to "re-launch and screenshot" after a GUI change.
Launch **your own** `event_explorer`, keep its PID, and capture a PNG of
**that** window. Never drive a GUI window you did not launch — the
user's already-open EventExplorer is off limits.

**Never** `pkill` / `killall` / `kill -f` by name or WM_CLASS. Never
screenshot, click, or scroll a window whose `_NET_WM_PID` is not the
PID this session launched. There is often more than one EventExplorer
running, so matching by window class or title will hit someone else's.

On X11 the flow is:

1. Launch `target/release/event_explorer <capture>` in the background
   and record its PID.
2. Find that PID's window (`xdotool search --pid <pid>`, or match
   `xprop _NET_WM_PID`).
3. Capture only that window id (`import -window <id> /tmp/ee.png`, or
   `xwd -id <id>` and convert), and send clicks/scrolls only to it.
4. When done, kill that PID only.

## Committing and shipping

This project is AI-owned. There is **no code review and no pull request
requirement** — commit your work directly to `main` and push.

- Commit to `main` and `git push` once the pre-commit checks above are
  green. Do not open a PR, do not create a feature branch, and do not
  wait for an approval that isn't coming.
- Never `git checkout -b` / `git worktree add -b` for EventExplorer
  work, and never `gh pr create`, unless the user explicitly asks for a
  branch or PR in that message.
- The checks are the gate. With no reviewer in the loop, an unverified
  commit goes straight to `main` — run them.
- Keep commits focused: one logical change each, imperative subject.

## Release process

Releases are tag-driven:

1. `Cargo.toml` `version` is the release version; give it a
   `## [<version>]` section in `CHANGELOG.md`.
2. Push tag `v<version>`. CI checks the tag against `Cargo.toml`, builds
   every platform, and publishes the release with `SHA256SUMS` and the
   CHANGELOG section as notes.
3. Then bump `Cargo.toml` to the next version and commit.

Full details and checklist in [`docs/releasing.md`](docs/releasing.md).

## Further reading

- [`docs/gui.md`](docs/gui.md) — GUI user guide.
- [`docs/cli.md`](docs/cli.md) — full CLI reference (all twelve subcommands).
- [`docs/file-format.md`](docs/file-format.md) — on-disk `.elog` layout and
  event-id catalog.
- [`docs/architecture.md`](docs/architecture.md) — codebase orientation,
  data flow, recipes.
- [`docs/site-config.md`](docs/site-config.md) — site config keys and the
  share-server / symbol-catalog APIs.
- [`docs/releasing.md`](docs/releasing.md) — release checklist.
