# Agent guide

This repo is Toimija, an internal False Systems Rust CLI.

Before inspecting files broadly: if `$TOIMIJA_LIVE` is set you are wrapped by `toimija run` — read that file (your session's continuously-refreshed packet) and re-read it after Git-state changes; otherwise run `toimija enter --for-diff --budget 4000` (the recommended agent preflight; it shares the `toimija context` code path, and under a session it targets your session packet). Toimija starts from Git state — branch, HEAD, index, merge-base, staged/unstaged/untracked files — so use that packet, and especially its Git and Branch intent sections, before reading the repo cold. The packet also carries Git-derived relevance hints (Scope, Read order, Coupling, Risk) mined from bounded Git history — advisory, not semantic truth, but use them to read the right files first.

Keep changes small and reviewable. Preserve the v0.0 scope: Git-aware local context compilation, config, repo detection, validation plan rendering, observability, and tests.

Do not add MCP, Tree-sitter, LSP, SCIP, Kythe, Sykli delegation, Vaisto integration, Kerto integration, background daemons, network calls, or a plugin system yet. (The `watch`/`run` commands are foreground-only and run solely while explicitly invoked.)

## Boundaries

These mirror `.toimija/repo.yaml` verbatim; `toimija doctor --strict` fails if
they drift apart. Edit both together.

- Keep v0.0 as one Rust crate.
- Do not add MCP, LSP, SCIP, Tree-sitter, Kythe, Sykli, Vaisto, or Kerto integration yet.
- Do not add network calls or background daemons; the watch and run commands run only while explicitly invoked.
- Agent coordination — per-session live packets, sibling-session awareness, and session handoff records — is in scope, but only as synchronous reads and writes during an explicitly-invoked command. Still no background daemons, no network, no task dispatch or assignment.

Before claiming completion, run:

```bash
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
```

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
draft PRs, and labels, not in Git. Toimija reports Git work state, surfaces live
sibling sessions, and can record a session handoff (see
`docs/proposals/0001-sessions-awareness-handoff.md`). It still does not
*coordinate* agents — no dispatch, no assignment, no orchestration. Claiming work
stays in GitHub issues, draft PRs, and labels; the human remains the orchestrator.
