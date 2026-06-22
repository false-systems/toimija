# Toimija

Toimija is an internal False Systems CLI.

It is a Git-aware local context compiler for coding agents. It gives an agent a compact packet about the current repository state before the agent edits code.

Core line:

> Git gives us the graph of what happened. Toimija turns that graph into operational context for agents.

## What It Is Not

Toimija v0.0 is not a platform, dashboard, dispatcher, queue, graph database, Tree-sitter indexer, LSP client, MCP server, or replacement for Git, Sykli, Vaisto, Kerto, or BaseMind.

Default posture is advisory. It warns and explains. It does not block humans.

## Build From Source

```bash
cargo build
```

Run locally:

```bash
cargo run -- context
```

Or install the binary from this checkout:

```bash
cargo install --path .
```

## Commands

```bash
toimija init
toimija doctor
toimija context
toimija context --json
toimija context --for-diff
toimija context --budget 4000
toimija validate --plan
```

Global options:

```bash
-v / --verbose
-q / --quiet
--log-format text|json
--no-color
```

Logs go to stderr. Command output goes to stdout.

## Example Context Output

```text
# Toimija Context

Repo:
- name: toimija
- role: internal CLI that compiles local Git context for coding agents
- not: platform, dashboard, dispatcher
- kind: Rust

Git:
- branch: main
- HEAD: unavailable
- detached: false
- dirty: true
- staged count: 0
- unstaged count: 2
- untracked count: 1
- merge-base: unavailable

Changed surface:
- src/git.rs
- README.md

Read first:
- src/git.rs (changed)
- README.md (important doc)

Validation plan

Source: inferred

Fast:
- cargo test --workspace

Strict:
- cargo fmt --check
- cargo clippy --workspace --all-targets -- -D warnings

Full:
- cargo test --workspace

Notes:
- validation inferred from Rust repo shape, not proven by Sykli
```

## Config

`toimija init` creates `.toimija/repo.yaml` if missing and refuses to overwrite existing files.

```yaml
repo:
  name: toimija
  role: internal CLI that compiles local Git context for coding agents
  not:
    - platform
    - dashboard

context:
  default_budget: 4000
  important_docs:
    - README.md
    - AGENTS.md

validation:
  fast: []
  strict: []
  full: []
  privileged: []

boundaries:
  - Keep the core as a single Rust crate for v0.0.
  - Do not add MCP, LSP, SCIP, Tree-sitter, background tasks, or network calls yet.
```

## v0.0 Limitations

- Git state is collected through the Git CLI, not Git internals.
- Merge-base is attempted against the selected base candidate.
- Validation plans are rendered only; commands are not executed.
- Rust and Elixir detection use simple marker files.
- Context budgets are approximate character caps, not token counts.
- Impact analysis is unavailable in v0.0.
- Upstream and merge-base may be unavailable in shallow, unborn, detached, or local-only repos.

## Output Stability

Context packets use explicit ordering rules so agents can diff and consume them predictably:

- changed files are sorted by path and deduplicated in the changed surface
- read-first entries are grouped as changed files, matching-test hints, then configured docs
- warnings are sorted and deduplicated
- Markdown and pretty JSON output have exact regression tests

## Read-First Heuristics

Read-first hints are bounded and path-derived. Toimija does not scan the whole repo or parse source code in v0.0.

Rust:

- changed Rust files are included directly
- `src/foo.rs` may point to `tests/foo.rs`, `tests/foo_test.rs`, `src/tests.rs`, or `src/foo_test.rs` if present
- `src/foo/bar.rs` may point to `tests/foo/bar.rs` and nearby sibling test files if present
- `src/foo/mod.rs` may point to `tests/foo.rs` or `tests/foo_test.rs` if present

Elixir:

- changed Elixir files are included directly
- `lib/foo/bar.ex` may point to `test/foo/bar_test.exs` if present
- `mix.exs` may point to `test/test_helper.exs` if present

## Future Drawer

These are intentionally not implemented in v0.0:

- MCP server
- Tree-sitter, LSP, SCIP, Kythe
- Sykli, Vaisto, or Kerto integration
- session fleet, handoff, checkpoint, worktree management
- dynamic plugins
- graph database
- background tasks
