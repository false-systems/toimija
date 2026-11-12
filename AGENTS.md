# Agent guide

This repo is Toimija, a Rust CLI with one job: it tells an agent where the work stands. It reports facts and refuses to decide.

Run `toimija enter` first. Under `toimija run` your launch prompt carries your packet; run `toimija enter` again for the state now, after HEAD, branch, index or unfamiliar worktree changes (not after your own edits). Its Git state is the working truth, and its Read first list names what to read. Say why you are here with `toimija intent "<why>"`.

Keep changes small and reviewable. Toimija has five commands: `run`, `enter`, `sessions`, `intent` and `handover`. Other tools read its facts through its CLI (`sessions --json`, the packet, the launch prompt), never through imports, plugins or dynamic libraries.

## Boundaries

- Keep Toimija one Rust crate with one job: where the work stands — Git state, changed files, who else is working here and why, and the last handover. A command outside that job does not belong here.
- Work the packet out when it is asked for; never write it to disk. Nothing Toimija says may need refreshing or verifying.
- Start no git process while an agent runs under `run`, and keep git processes per command minimal; `tests/cli_budgets.rs` pins them.
- No network calls, no background daemons. `run` lives only as long as its agent, and a session's marker only as long as its `run`.
- Sessions outside any Git repository live in the machine-local state directory, never a `.toimija/` folder where the agent works. A workspace (`.toimija/workspace.yaml`) is read from files only: no git process per repository, nothing written.
- No MCP, LSP, SCIP, Tree-sitter or Kythe integration. Toimija runs no checks and writes no receipts (that is sykli's), and does no dispatch, assignment or closure of work.

## Checks

Before claiming completion: `cargo fmt`, `cargo test`, `cargo test -p xtask`, `cargo clippy --all-targets -- -D warnings`; or the same as `sykli run` (`sykli.json`, cached, with structured failures) or `cargo xtask gate`. Every new tracked file goes into the inputs of `test` and `xtask-test` in `sykli.json`, and of `fmt` and `clippy` if it is code, and of `clippy` if a target reads it with `include_str!` (`tests/contract.rs` checks); stage it, then `sykli lock sykli.json`.

Landing: on the committed branch run `sykli run`, then `sykli review`, then `git push origin HEAD:main`. Review starts a fresh agent, another one than the author where one is installed, which reads the change from the merge base with `origin/main` to HEAD with reading tools only and says `fine`, `look` or `blocking`. A reviewer that is missing, fails or gives no verdict is no review (exit 2), never a pass; only a person overrules a blocking one (`sykli review --overrule "<why>"`, at a terminal). sykli's pre-push hook refuses the push unless the remote's tip is an ancestor of the commit, its tree has a passing receipt and a review of it against that tip is not blocking; then it shares the receipt and the review as git notes on the tree. A push to `main` lands the work, so do it only when the person you work for says so.

Install the pre-commit hook once per clone, and again after changing `xtask`: `cargo xtask install-pre-commit-hook`. It formats the staged crates, then runs clippy and the tests. Update README.md and docs/reference.md when CLI behaviour changes; `tests/docs.rs` fails when they show a command or flag `toimija --help` does not have. When the boundaries change, update `.toimija/repo.yaml` too.

## Working with other agents

- Claim an issue before you start (assign it or comment), once you have checked it has no owner and no open PR.
- Re-orient first: `toimija enter`, `toimija sessions`, `git fetch`, `git log --oneline origin/main`, `gh pr list`. Never branch from a stale main.
- One unit of work, one small PR. To update a branch under review, merge `origin/main` into it; never rebase or force-push it. Merge bottom-up, and delete only leaf branches.
- After a merge, `git pull && cargo install --path .`: a stale binary lies about the code.
- Who does what lives in issues, draft PRs and labels. Toimija shows live sibling sessions; it never coordinates them.

Code is Rust or Elixir/Erlang; never add Python, Go or Node.
