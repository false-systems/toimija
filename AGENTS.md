# Agent guide

This repo is Toimija, an internal False Systems Rust CLI: Git-aware local context for agents. It reports facts and refuses to decide.

Read `.toimija/current.md` first. Under `toimija run` your launch prompt carries your session packet and names its live copy: re-read it after HEAD, branch, index or unfamiliar worktree changes (not after your own edits), or refresh it with `toimija enter --for-diff --budget 4000`. Its Git state is the working truth, and its Read first list names what to read. Say why you are here with `toimija intent "<why>"`, and run `toimija verify` before you commit or hand off.

Keep changes small and reviewable, within v0.0: Git-aware local context compilation, config, repo detection, validation plan rendering, observability, and tests. Other products read Toimija's facts through its CLI and files (`repository-point --json`, `sessions --json`, the packet), never through imports, plugins or dynamic libraries.

## Boundaries

These mirror `.toimija/repo.yaml` verbatim; `toimija doctor --strict` fails if they drift apart. Edit both together.

- Keep v0.0 as one Rust crate.
- Do not add MCP, LSP, SCIP, Tree-sitter, or Kythe integration yet. Sykli is in scope only as this repo's CI pipeline (the sykli.json contract and its CI job); Sykli delegation inside toimija's own commands stays out of scope.
- Do not add network calls or background daemons; the watch and run commands run only while explicitly invoked.
- Agent coordination — per-session live packets and sibling-session awareness — is in scope, but only as synchronous reads and writes during an explicitly-invoked command. Still no background daemons, no network, no task dispatch or assignment.
- A session outside any Git repository is in scope since 2026-09-26 (issue #225): `run` opens a visible session (marker, heartbeat, handshake; no packet) in the machine-local state directory, never a `.toimija/` folder where the agent works. Workspace context compilation stays out of scope.
- Toimija reports facts and refuses to decide. No gates, checks, or receipts (Evidence belongs to Sykli), no work providers or plugins, and no dispatch, assignment, or closure of work.

## Checks

Before claiming completion: `cargo fmt`, `cargo test`, `cargo clippy --all-targets -- -D warnings`; or the same as `sykli run` (`sykli.json`, cached, with structured failures) or `cargo xtask gate`. Every new tracked file goes into the inputs of `test` and `doctor` in `sykli.json`, and of `fmt` and `clippy` if it is code; stage it, then `sykli lock sykli.json`.

Install the pre-commit hook once per clone, and again after changing `xtask`: `cargo xtask install-pre-commit-hook`. It formats the staged crates, then runs clippy and the tests. Update README.md when CLI behaviour changes; when boundaries, validation commands, major docs or structure change, update `.toimija/repo.yaml` too and run `toimija doctor --strict`.

## Working with other agents

- Claim an issue before you start (assign it or comment), once you have checked it has no owner and no open PR.
- Re-orient first: `toimija context`, `git fetch`, `git log --oneline origin/main`, `gh pr list`. Never branch from a stale main.
- One unit of work, one small PR. To update a branch under review, merge `origin/main` into it; never rebase or force-push it. Merge bottom-up, and delete only leaf branches.
- After a merge, `git pull && cargo install --path .`: a stale binary lies about the code.
- Who does what lives in issues, draft PRs and labels. Toimija shows live sibling sessions; it never coordinates them.

Code is Rust or Elixir/Erlang; never add Python, Go or Node.
