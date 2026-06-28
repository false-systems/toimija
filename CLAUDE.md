@AGENTS.md

## Claude Code notes

Do not start with a broad file crawl.
Run `toimija enter --for-diff --budget 4000` first (it shares the `context` code path) and treat the Git state section as the working truth of the repo.

Work in small changes.
Do not add MCP, Tree-sitter, LSP, SCIP, Sykli, Vaisto, Kerto, Terraform, workspace, or checkpoint support unless a human explicitly moves it into scope.
Session coordination (per-session packets, `toimija sessions` awareness, `toimija handoff`) is now in scope per `docs/proposals/0001-sessions-awareness-handoff.md` — but stays synchronous, local, no-daemon, no-network, no-dispatch.
Run formatting, tests, and clippy before claiming completion.
