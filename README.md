# Toimija

Toimija is an internal False Systems CLI.

It is a Git-aware local context compiler for coding agents. It gives an agent a compact packet about the current repository state before the agent edits code.

Core line:

> Git gives us the graph of what happened. Toimija turns that graph into operational context for agents.

## Git as the starting point

Most agent tools make the model rediscover the repo from files.

Toimija starts from the one truth database every repo already has: Git. HEAD, refs, index, merge-base, staged and unstaged changes, dirty state, branch divergence, and reflog are not just metadata. They are the repo's working state machine.

Toimija reads that state machine first, then compiles it into a small context packet: what changed, what is risky, what to read, what to ignore, and what validation can honestly prove.

Git is not a save button here.

Git is the substrate.

> We do not ask the model to understand the repo first. We ask Git what state the repo is in.

`toimija context` makes that state explicit: the `Git:` block (HEAD, branch, detached, upstream, merge-base, dirty, and staged/unstaged/untracked/conflict counts), a `Branch intent:` section (what the whole branch changed since the merge-base, distinct from dirty state), and a `Changed surface:` whose `source:` line names whether it came from the branch diff or only the working tree and index.

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

Release artifacts are built by GitHub Actions when a `v*` tag is pushed. The v0.0 workflow runs on False Systems ARC/Hetzner ARM64 runners and uploads a Linux aarch64 binary as a workflow artifact; it does not publish a GitHub Release yet.

### Pre-commit hook

A `xtask` crate provides a git pre-commit hook via
[`devx-pre-commit`](https://docs.rs/devx-pre-commit). It is dev-only and kept
out of the shipped binary by `default-members = ["."]` in the workspace, so
bare `cargo build`/`test`/`clippy` and the release build never compile it.
Install the hook once per clone:

```bash
cargo xtask install-pre-commit-hook
```

The hook formats the staged Rust crates, re-stages the result, then runs
`cargo clippy --all-targets -- -D warnings` followed by `cargo test`, blocking
the commit if either fails — the same gates CI enforces. Re-run the command
after editing `xtask`.

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

## Repo
- name: toimija
- role: internal CLI that compiles local Git context for coding agents
- not: platform, dashboard, dispatcher
- kind: Rust

## Git
- branch: feat/git-ceramics
- HEAD: abc123
- detached: false
- upstream: unavailable
- merge-base: origin/main @ def456
- dirty: true
- staged: 0
- unstaged: 2
- untracked: 1
- conflicts: 0

## Git state
Toimija starts from Git's state machine: HEAD, index, working tree, branch, and merge-base.

## Branch intent
- compared against: origin/main
- merge-base: def456
- files changed since merge-base: 3

## Working tree
### Staged
- none
### Unstaged
- modified src/git.rs
- modified src/render.rs
### Untracked
- tests/git_state.rs
### Conflicts
- none

## Changed surface
- source: branch diff since merge-base
- files:
  - README.md
  - src/git.rs
  - src/render.rs
  - tests/git_state.rs

## Read first
- src/git.rs (changed)
- tests/git_state.rs (changed)
- README.md (important doc)

## Validation plan
- source: inferred
### Fast
- cargo test --workspace
### Notes
- validation inferred from Rust repo shape, not proven by Sykli

## Warnings
- No upstream detected. Branch divergence is unavailable.
- Untracked files exist. They will not be committed unless added.
- Working tree has unstaged changes.
```

## Config

`toimija init` creates `.toimija/repo.yaml` if missing and refuses to overwrite existing files.

On a fresh config it prints a **Next steps** line pointing at the identity fields (`role`, `not`, `boundaries`) you still need to fill — Git supplies the work state, but only you can supply the repo's purpose and boundaries. Until those are set, `toimija context` renders them as `(unset — add to .toimija/repo.yaml)` rather than pretending they are unavailable.

Build and vendor directories (`target`, `_build`, `deps`, `node_modules`) are filtered from the changed surface automatically, matched as a path segment anywhere in the tree (so nested `test_projects/app/target/` is caught too). This is applied live each run — not written into `repo.yaml` — so it tracks the repo as its kind changes. `context.safe_to_ignore` is for your own additional rules on top.

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
  read_first:
    - docs/architecture.md
  safe_to_ignore:
    - target/
    - _build/
    - deps/

validation:
  fast: []
  strict: []
  full: []
  privileged: []

boundaries:
  - Keep the core as a single Rust crate for v0.0.
  - Do not add MCP, LSP, SCIP, Tree-sitter, background tasks, or network calls yet.
```

`context.read_first` adds repo-specific files to the Read first section when they exist.
`context.safe_to_ignore` lists directory rules that are omitted from the changed surface and read-first hints. Each rule is matched as a path segment (or contiguous segment sequence) anywhere in the tree, not just at the repo root.

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

- the changed surface unions working-tree changes (staged, unstaged, untracked) with files changed by commits on this branch since the merge-base, so a clean tree on a feature branch still reports its diff
- changed files are sorted by path and deduplicated in the changed surface
- read-first entries are grouped as changed files, matching-test hints, then configured docs
- warnings are sorted and deduplicated
- Markdown output uses a fixed heading hierarchy: top-level sections are `##`, sub-blocks (e.g. Staged/Unstaged under Working tree) are `###`, and a blank line precedes every top-level section
- empty values are rendered honestly: `none` for an empty-but-known list, `unavailable` for a value Git could not determine
- Markdown and pretty JSON output have exact regression tests

`toimija context --for-diff` adds a diff-focused Markdown section that separates:

- committed files changed since the selected merge-base
- staged files
- unstaged files
- untracked files
- conflicted files

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
