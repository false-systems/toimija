# Agent guide

This repo is Toimija, an internal False Systems Rust CLI.

Read `.toimija/current.md` before reading anything else (under `toimija run` your launch prompt carries your session packet inline and names the live copy — re-read that copy after HEAD, branch, index, or unfamiliar worktree changes, not after your own expected source edits; run `toimija verify` before commit or handoff). To refresh it by hand, `toimija enter --for-diff --budget 4000`. Toimija starts from Git state — branch, HEAD, index, merge-base, staged/unstaged/untracked files — so use the packet, and especially its Git and Branch intent sections, before reading the repo cold. Its Read first list names the important docs first, then the changed files and their companion tests — advisory, path-derived, but use it to read the right files first.

Keep changes small and reviewable. Preserve the v0.0 scope: Git-aware local context compilation, config, repo detection, validation plan rendering, observability, and tests.

Do not add MCP, Tree-sitter, LSP, SCIP, Kythe, Sykli delegation, Vaisto integration, Kerto integration, background daemons, or network calls. (The `watch`/`run` commands are foreground-only and run solely while explicitly invoked.) Other products read Toimija's facts through its CLI and files — `repository-point --json`, `sessions --json`, the packet — never through product imports, plugins, or dynamic libraries.

## Boundaries

These mirror `.toimija/repo.yaml` verbatim; `toimija doctor --strict` fails if
they drift apart. Edit both together.

- Keep v0.0 as one Rust crate.
- Do not add MCP, LSP, SCIP, Tree-sitter, Kythe, Vaisto, or Kerto integration yet. Sykli is in scope only as this repo's CI pipeline (the sykli.json contract and its CI job); Sykli delegation inside toimija's own commands stays out of scope.
- Do not add network calls or background daemons; the watch and run commands run only while explicitly invoked.
- Agent coordination — per-session live packets and sibling-session awareness — is in scope, but only as synchronous reads and writes during an explicitly-invoked command. Still no background daemons, no network, no task dispatch or assignment.
- A session outside any Git repository is in scope since 2026-09-26 (issue #225): `run` opens a visible session (marker, heartbeat, handshake; no packet) in the machine-local state directory, never a `.toimija/` folder where the agent works. Workspace context compilation stays out of scope.
- Toimija reports facts and refuses to decide. No gates, checks, or receipts (Evidence belongs to Sykli), no work providers or plugins, and no dispatch, assignment, or closure of work.

Before claiming completion, run:

```bash
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
```

or equivalently `sykli run` — the same gate as a typed execution contract
(`sykli.json`; README, "Declared work graph"), with cached task results and
structured failures — or
`cargo xtask gate`, the same three checks behind the single entrypoint
harnesses invoke (guardrails R-R.1).


Install the git pre-commit hook once per clone (it lives in `.git/`, which is
not tracked, so it does not arrive with a fresh checkout):

```bash
cargo xtask install-pre-commit-hook
```

The hook runs rustfmt on the staged crates, re-stages the formatted result,
then runs `cargo clippy --all-targets -- -D warnings` followed by `cargo test`,
aborting the commit if either fails. It mirrors CI so failures surface locally.
Re-run the install command after changing `xtask` (the hook is a copied binary
snapshot).

Update README.md when CLI behavior changes.

If you change repo boundaries, validation commands, major docs, or project
structure, update `.toimija/repo.yaml` to match, then run `toimija doctor
--strict` — the drift guard fails when the config and the agent docs fall out of
sync.

## Working with other agents

Several agents may work this repo at once and fast. Coordinate through Git and
GitHub primitives, not assumptions:

- **Claim before you work.** Before starting an issue, assign it to yourself
  (`gh issue edit <n> --add-assignee @me`) or comment that you are taking it.
  First check whether it is already assigned or has an open PR — do not
  duplicate work.
- **Re-orient before you act.** Run `toimija context`, then
  `git fetch && git log --oneline origin/main && gh pr list`. Never branch from
  a stale `main`.
- **Small, short-lived branches.** One unit of work → one small PR → merge →
  others pull. Long-lived branches drift and rot.
- **Rebase on `origin/main` right before opening or merging**, not only at
  branch creation.
- **Merge bottom-up; delete only leaf branches.** Never delete a branch another
  PR is stacked on — it closes the stacked PR.
- **After any merge, pull and reinstall.** `git pull && cargo install --path .`
  — a stale local binary lies about the code.

Coordination state — who is doing what right now — lives in issue assignments,
draft PRs, and labels, not in Git. Toimija reports Git work state and surfaces
live sibling sessions (see `docs/proposals/0001-sessions-awareness-handoff.md`).
It still does not *coordinate* agents — no dispatch, no assignment, no
orchestration. Claiming work stays in GitHub issues, draft PRs, and labels; the
human remains the orchestrator.

## Languages

Code in this repository is **Rust, or Elixir/Erlang**. Never add Python, Go
or Node (JavaScript/TypeScript): not for tools, scripts, CI helpers, tests,
dashboards or glue. A thin shell step in a CI workflow is fine; anything with
logic is Rust. Existing Python or Node files are debt to be rewritten, not
precedent to follow.
