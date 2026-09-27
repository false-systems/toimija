<!-- toimija:adopt:v1:begin -->
Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
<!-- toimija:adopt:v1:end -->

@AGENTS.md

## Claude Code notes

Do not start with a broad file crawl.
Read `.toimija/current.md` first (the preamble above already says so). Under `toimija run` your launch prompt carries your session packet inline and names the live copy — re-read that copy after Git-state changes; refresh by hand with `toimija enter --for-diff --budget 4000`. Treat the packet's Git state section as the working truth of the repo, honor its Workset contract, and declare why you are here: `toimija intent "<why>" [--scope <path>]`.

Work in small changes.
Do not add MCP, Tree-sitter, LSP, SCIP, Vaisto, Kerto, Terraform, workspace, or checkpoint support unless a human explicitly moves it into scope. (Sykli-as-CI is in scope since 2026-07-16: the sykli.json contract runs the gate; no deeper Sykli integration. A session outside any repository — marker, heartbeat and handshake, no packet — is in scope since 2026-09-26, issue #225; workspace *context* is not.)
Session coordination (per-session packets, `toimija sessions` awareness, `toimija handoff`) is now in scope per `docs/proposals/0001-sessions-awareness-handoff.md` — but stays synchronous, local, no-daemon, no-network, no-dispatch.
Run formatting, tests, and clippy before claiming completion.
