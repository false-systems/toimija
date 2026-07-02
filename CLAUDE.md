@AGENTS.md

## Claude Code notes

Do not start with a broad file crawl.
If `$TOIMIJA_LIVE` is set you are wrapped by `toimija run`: read that file — it is your session's continuously-refreshed packet — and re-read it after Git-state changes. Otherwise run `toimija enter --for-diff --budget 4000` first (it shares the `context` code path; under a session it updates your session packet too). Either way, treat the packet's Git state section as the working truth of the repo. If you are wrapped, consider declaring why you are here: `toimija intent "<why>" [--scope <path>]`.

Work in small changes.
Do not add MCP, Tree-sitter, LSP, SCIP, Sykli, Vaisto, Kerto, Terraform, workspace, or checkpoint support unless a human explicitly moves it into scope.
Session coordination (per-session packets, `toimija sessions` awareness, `toimija handoff`) is now in scope per `docs/proposals/0001-sessions-awareness-handoff.md` — but stays synchronous, local, no-daemon, no-network, no-dispatch.
Run formatting, tests, and clippy before claiming completion.
