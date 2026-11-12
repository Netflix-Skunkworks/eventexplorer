# Contributing to EventExplorer

Thanks for your interest in improving EventExplorer! This guide covers how to
get set up, the bar a pull request needs to clear, and how we think about
AI-assisted contributions.

## Quick start

```bash
# Build both binaries (release mode is the project default).
cargo build --release

# Run the GUI / CLI against a capture.
cargo run --release --bin event_explorer -- path/to/capture.elog.gz
cargo run --release --bin elog_cli       -- path/to/capture.elog.gz summary

# Before you commit (all three must be green):
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

The web (wasm) build is optional and only relevant if you touch `src/lib.rs`'s
`WebHandle`, the wasm branches in `src/app/`, or `index.html` / `Trunk.toml`:

```bash
rustup target add wasm32-unknown-unknown
cargo install --locked trunk
trunk build      # static bundle in ./dist/
```

## Before you start

- **Check existing issues** so you're not duplicating work.
- **Discuss large or architectural changes first** by opening an issue. New
  *providers* (kernel subsystems beyond `tcp` / `hpts`) and parser-format
  changes are the kind of thing worth aligning on before you build it.
- Read [`docs/architecture.md`](docs/architecture.md) for the module map and the
  "how do I add X" recipes, and [`AGENTS.md`](AGENTS.md) for repo conventions.

## How changes land

EventExplorer is AI-owned and maintained. **If you have push access, commit
directly to `master` — no pull request and no code review is required.**
AI agents must not open a branch or PR unless Nick asks for one in that
message. Outside contributors without push access: fork, then open a PR.

Because nothing gates a direct commit, the checks below are the quality bar.
Run them locally, every time, before you commit:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

CI runs the same three checks plus a wasm build, so a skipped local run turns
into a red `master`. Formatting is the most common break — run `cargo fmt --all`
after your last edit, not before it.

## Change requirements

1. **Tests are mandatory for bug fixes.** A bug fix must include a test that
   fails before your change and passes after it. This is our most important
   requirement.
2. **Tests are expected for new features.** If you're unsure what to test, say
   so in the commit message.
3. **Keep changes focused.** One logical change per commit; split unrelated
   work.
4. **Pass the checks.** See [How changes land](#how-changes-land) above.
5. **Match the local style.** Mirror the conventions of the file you're editing;
   keep comments lean and purposeful (explain non-obvious intent, not the
   obvious "what").

## AI Tool Usage Policy

**EventExplorer was developed primarily using AI coding agents, and
AI-written or AI-assisted contributions are welcome and expected.**

In exchange, a few things are non-negotiable:

- **You are accountable for everything you submit.** You must read, understand,
  and be able to explain and maintain any code you land. "The AI wrote it" is
  not a defense for bugs, hallucinated APIs, or unreviewed output. With direct
  commits and no review, this is the only safeguard there is.
- **The quality bar is identical regardless of authorship.** AI-generated code
  must build, pass `cargo fmt` / `clippy -D warnings`, include tests, and follow
  the conventions in [`AGENTS.md`](AGENTS.md). Run the checks locally before
  committing — don't use CI as your first test run.
- **When a change is substantially AI-generated, note that in the commit
  message** (a `Co-authored-by:` trailer for the agent is enough). Landing a
  change means you're certifying that you have the right to contribute the code
  under this project's license, and that responsibility doesn't change based on
  which tools helped you write it.
- **Respect licensing.** Don't include code under a license incompatible with
  Apache-2.0. All contributions are made under the project's Apache-2.0 license.

If you're using an agent, point it at [`AGENTS.md`](AGENTS.md) (and
[`CLAUDE.md`](CLAUDE.md)) — they are the source of truth for build/test
commands, the module layout, and the common gotchas.

## PR description template

Only needed if you're opening a PR (outside contributors, or by choice):

```
## What & why
<What does this change do, and why? Link any related issue.>

## How
<Key implementation notes / decisions a reviewer should know.>

## Testing
<What tests did you add or run? How did you verify the change
 (e.g. CLI output against a capture, a GUI screenshot)?>
```

## Commit guidelines

- Write imperative, present-tense subjects ("Add limiter overlay", not "Added").
- Group related changes; avoid mixing refactors with behavior changes.
- State in the body how you verified the change (which checks you ran).

## Reporting bugs and requesting features

Use the issue templates under
[`.github/ISSUE_TEMPLATE`](.github/ISSUE_TEMPLATE). For security issues, see
[`SECURITY.md`](SECURITY.md) instead of filing a public issue.
