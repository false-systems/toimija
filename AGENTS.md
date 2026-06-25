# Agent guide

This repo is Toimija, an internal False Systems Rust CLI.

Before inspecting files broadly, run `toimija context` (or `cargo run -- context --for-diff --budget 4000`). Toimija starts from Git state — branch, HEAD, index, merge-base, staged/unstaged/untracked files — so use that packet, and especially its Git and Branch intent sections, before reading the repo cold. The packet also carries Git-derived relevance hints (Scope, Read order, Coupling, Risk) mined from bounded Git history — advisory, not semantic truth, but use them to read the right files first.

Keep changes small and reviewable. Preserve the v0.0 scope: Git-aware local context compilation, config, repo detection, validation plan rendering, observability, and tests.

Do not add MCP, Tree-sitter, LSP, SCIP, Kythe, Sykli delegation, Vaisto integration, Kerto integration, background tasks, network calls, or a plugin system yet.

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
draft PRs, and labels, not in Git. Toimija reports Git work state; it does not
coordinate agents, and should not be made to.
