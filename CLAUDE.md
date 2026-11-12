# CLAUDE.md

This project's conventions for AI coding agents live in
[`AGENTS.md`](AGENTS.md). Read it first — it is the source of truth for the
module map, build/test commands, the cross-target (native + wasm) cfg pattern,
and the common gotchas.

Three rules worth repeating here:

- **Run the checks before every commit.** `cargo fmt --all`, `cargo clippy
 --all-targets --locked -- -D warnings`, and `cargo test --locked` must all be
 green locally. See
 [Pre-commit checks](AGENTS.md#pre-commit-checks-required).
- **Commit directly to `main`.** This project is AI-owned; no pull
 request, feature branch, or code review is required. See
 [Committing and shipping](AGENTS.md#committing-and-shipping).
- **Never drive a GUI window you did not launch.** Launch your own window,
 keep that PID, and only screenshot/click/kill that process. See
 [Verifying GUI changes](AGENTS.md#verifying-gui-changes).

See also:

- [`docs/architecture.md`](docs/architecture.md) — codebase orientation, data
  flow, and "how do I add X" recipes.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — the contribution workflow and the AI
  Tool Usage Policy.
