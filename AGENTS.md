# Agent guide

This repo is Toimija, an internal False Systems Rust CLI.

Before making changes, run:

```bash
cargo run -- context --for-diff --budget 4000
```

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
