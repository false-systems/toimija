@AGENTS.md

## Claude Code notes

Do not start with a broad file crawl.
Use `toimija context` first and treat the Git state section as the working truth of the repo.

Work in small changes.
Do not add MCP, Tree-sitter, LSP, SCIP, Sykli, Vaisto, Kerto, Terraform, workspace, fleet, handoff, or checkpoint support unless a human explicitly moves it into scope.
Run formatting, tests, and clippy before claiming completion.
