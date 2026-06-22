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

Update README.md when CLI behavior changes.
