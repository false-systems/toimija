# Toimija reference

This is the manual: every command, the packet, sessions, configuration and
the guarantees behind them. For a first look, read the [README](../README.md).

Toimija has one job: it tells an agent where the work stands. It reads Git
state through the Git command line, combines it with what `.toimija/repo.yaml`
says about the repository, and works out a packet when it is asked for. It
keeps no copy of the packet and no index of its own, so nothing it says can
go stale. When something cannot be read, the packet says so.

Contents:

- [Files](#files)
- [Starting an agent: `run`](#starting-an-agent-run)
- [The packet: `enter`](#the-packet-enter)
- [Sessions](#sessions)
- [Workspaces](#workspaces)
- [Commands](#commands)
- [Configuration](#configuration)
- [Security](#security)
- [Limits](#limits)
- [Working on Toimija](#working-on-toimija)

## Files

Toimija writes only under `.toimija/` in the repository. Sessions opened
outside any repository live in a state directory on the machine.

```text
.toimija/repo.yaml        yours     what the repository is, its boundaries, validation commands
.toimija/workspace.yaml   yours     in a folder of repositories: what the folder is for and what to read first
.toimija/sessions/<id>/   Toimija   one running session's marker; removed when the session ends
.toimija/handover.json    Toimija   the note for the next agent in this working copy
.toimija/tmp/             Toimija   staging for atomic writes; it ignores itself
AGENTS.md, CLAUDE.md      yours     Toimija never writes them
```

Toimija needs no setup and installs nothing on a repository: no Git hook, no
agent document, no harness setting. Both files marked yours are optional and
written by hand. Toimija's own files are not for Git. Keep them out with two
lines in the repository's `.gitignore`:

```text
/.toimija/*
!/.toimija/repo.yaml
```

Where they are not ignored, Git lists them, and Toimija still never counts
them as the repository's work.

## Starting an agent: `run`

```bash
toimija run claude
toimija run --intent "fix the login redirect" claude --model opus
toimija run --intent "fix the login redirect" --scope src/auth/ --task "repair the expired-session redirect" -- codex
```

`run` takes the agent's command. What follows the agent's name belongs to the
agent, flags included. A `--` before the agent is accepted and not needed.

`run` does this:

1. Finds the repository and works out its packet.
2. Opens a session: writes its marker under `.toimija/sessions/<id>/`, and
   removes the markers beside it whose process is gone.
3. Prints the packet, and starts the agent directly, with no shell in
   between, with the packet in its launch prompt.
4. While the agent runs, starts no git process. Once a minute it stamps the
   session's marker, a file write.
5. When the agent exits, removes the marker, reads `git status` once, prints a
   summary and exits with the agent's status. An agent ended by a signal is
   reported the same way: SIGTERM gives 143.

`run` launches whatever state Git is in. A merge, rebase, cherry-pick, revert
or bisect in progress, and conflicted files, are in the packet for the agent
to read first.

### Options

| Option | Meaning |
| --- | --- |
| `--intent <TEXT>` | Why the session exists. Other sessions see it. Without it they see `unknown` until the agent runs `toimija intent`. |
| `--scope <PATH>` | A path the session expects to touch. Repeatable. Advisory, never a lock. |
| `--task <TASK>` | The task, delivered in the launch prompt. Pass it here, not as the agent's own argument. |
| `--briefing <BRIEFING>` | Background instructions, kept apart from the task. They go at the end of the launch prompt. |
| `--alert <TEXT>` | An urgent line that opens the launch prompt. Repeatable. |
| `--for-diff` | Add the diff-focused section to the packet. |
| `--budget <BUDGET>` | The size cap of the Markdown packet, in bytes. |

`--task`, `--briefing` and `--alert` may start with `-`. When the next
argument is one of `run`'s own flags, or `--`, the value is missing: `run`
refuses with exit 2 and starts nothing. Write text that is a flag's name with
`=`, as in `--task=--briefing`. A blank task, briefing or alert counts as
none.

Alerts appear under `## Before anything else (from the launcher)`, above the
workset contract and the packet. They are capped at 1024 bytes in total. The
rest is cut at a character boundary, the block says so, and `run` warns on
stderr. A launcher can detect `--alert` by reading `toimija run --help`.

### How the packet reaches the agent

For a known agent, `run` adds one argument to the agent's command: the launch
prompt. It holds the alerts, the workset contract, the packet at the session's
budget, the task and the briefing.

Known agents are `claude`, `codex`, `gemini`, `pi` and `opencode`, matched by
the base name of the command. `opencode` takes the prompt after `--prompt`,
because its one positional argument is a folder; `opencode run` takes it as
its message. The others take it as a positional argument.

An `agents:` entry in `.toimija/repo.yaml` tells `run` how any other command
takes a prompt: `prompt: positional`, or a flag such as `prompt: --message`.
Configuration wins over the built-in list. It never supplies an executable
path: the agent is always the command you name.

A command `run` does not know gets only the environment handshake. `run` says
so (`delivery: env-var only — not guaranteed`) and names what was not
delivered (`not delivered: packet, task, briefing`). The session marker
records the same under `undelivered`, and `sessions --json` shows it.

The launch prompt tells the agent to use the delivered facts instead of
repeating discovery, to declare scope before it reads outside the workset, and
to run `toimija enter` again for the state as it is now: the packet in the
prompt is the state at launch.

### Codex

Without `--task`, an interactive Codex launch receives the packet and the
briefing as per-session `developer_instructions` and no positional message.
Codex opens and waits for the first message. No model turn is spent on an
acknowledgement.

- Codex adds this setting to its own developer instructions, so the slot
  carries only the packet and the briefing.
- A `developer_instructions` in your own Codex configuration is replaced for
  that session. One passed on the command line (`-c developer_instructions=…`)
  is kept. The launch is then not idle: the packet goes in the first message,
  and `run` says so.
- With `--task`, or with an `agents.codex` entry, delivery follows the prompt
  style. `codex exec` and `codex resume` always get the prompt.

Codex may run tool commands in a process that is already running and does not
inherit the terminal's environment. So `run` also passes each handshake
variable as `-c shell_environment_policy.set.<name>=<value>`. This passes only
this session's identity. It changes no global Codex configuration,
inheritance policy or sandbox setting. A variable `run` removes from the
agent's environment is set to an empty string for Codex tools, because `set`
can only add. Toimija reads an empty handshake value as unset, and other
readers should too. A value that is not valid UTF-8 stops the launch.

### Environment handshake

`run` sets these for the agent. For a known agent they are a second channel
beside the launch prompt. For any other command they are the only one.

| Variable | Meaning |
| --- | --- |
| `TOIMIJA_RUN=1` | The agent runs under `run`. |
| `TOIMIJA_SESSION` | The session's id, `<unix-seconds>-<pid>`. `intent`, `enter` and `handover` use it to find the session. |
| `TOIMIJA_RUN_ID` | The same as `TOIMIJA_SESSION`. |
| `TOIMIJA_REPO_ROOT` | Absolute path of the repository root. |

### The session marker

Each session writes `.toimija/sessions/<id>/session.json`: id, pid, when the
process started (`pid_start`), the agent's command, worktree, branch and HEAD
at entry, intent, scope, when it started and when it was last seen. `toimija
sessions` reads it. `run` removes it when the agent exits, so a marker on disk
is a session that is running, or one whose `run` was killed before it could
remove it.

The entry branch and HEAD stay fixed. When Git state moves away from them, a
packet worked out for the session shows a `Session drift` block.

### The exit summary

```text
toimija run: session summary
  branch:    fix/login-redirect
  worktree:  0 staged, 1 unstaged, 0 untracked
  intent:    "fix the login redirect" (declared at launch)
  expansions: none declared
```

The summary compares Git state at entry with one `git status` at exit. A
switched branch and a moved HEAD are shown as `before → after`. The intent and
expansion lines come from the session marker, never from watching the agent.
The worktree line counts the repository's own work, never Toimija's files or
`.sykli/`, where sykli keeps its receipts.

### Outside a repository

A folder with a workspace file is a [workspace](#workspaces): `run` opens the
same kind of session there and puts the workspace packet in the launch
prompt. The rest of this section is about a folder with no workspace file.

In a folder that is not inside a Git repository, `run` opens a session with no
packet. There is no Git state to read, so there is no packet and no exit
summary, and `--for-diff` and `--budget` are refused. What remains makes the
session visible:

- The marker, in the state directory: `$TOIMIJA_STATE_DIR`, else
  `$XDG_STATE_HOME/toimija`, else `~/.local/state/toimija`. It sits at
  `<state>/.toimija/sessions/<id>/session.json`, with `worktree` set to the
  folder. Nothing is written in the folder itself.
- The same stamp once a minute, and the marker removed when the agent exits.
- `TOIMIJA_RUN`, `TOIMIJA_SESSION`, `TOIMIJA_RUN_ID` and `TOIMIJA_STATE_DIR`.
  `TOIMIJA_REPO_ROOT` is cleared, so a value from an outer session cannot leak
  in.
- For a known agent, a launch prompt that says there is no packet, then the
  task. A session with no task waits for work and spends no model calls on
  discovery.

`toimija sessions --outside-repos` lists these sessions. `toimija intent`
finds its session there even after the agent moves into a repository.
Sessions inside the repositories the agent touches do not count it as a
sibling, so the agent should declare its scope.

### After installing a new version

A running `toimija run` keeps the code it started with. `toimija --version`
names the commit a binary was built from.

## The packet: `enter`

```bash
toimija enter
toimija enter --for-diff --budget 4000
toimija enter --json
```

`enter` works out the packet and prints it: Markdown, or with `--json` the
whole packet as JSON. It writes nothing. An agent that `run` did not start
runs it first; any agent runs it again for the state as it is now. Under
`run` it carries the session's intent and scope, as the marker says them now.
`--for-diff` adds a section that lists committed, staged, unstaged, untracked
and conflicted files apart, and `--budget` caps the Markdown.

### What it holds

- **Repository**: name, role, what it is not, its kind, and other languages
  found.
- **Session**, under `run`: id, intent, scope, and drift from the entry branch
  and HEAD.
- **Last handover**: the note the previous agent left.
- **Workset contract**: the rule for reading. Work from the changed surface
  and Read first. Configured exclusions are not scanned. Reading outside the
  workset is allowed when a concrete dependency needs it, and is declared
  first with `toimija intent "<why>" --scope <path>`.
- **Git**: branch, HEAD, upstream, ahead and behind, merge-base, recent
  commits, and an operation in progress: a merge, rebase, cherry-pick, revert
  or bisect. When the repository's other worktrees include finished ones, one
  line counts them (`- finished worktrees: 2 elsewhere (`toimija sessions`
  lists them)`); see [Finished worktrees](#finished-worktrees).
- **Working tree**: staged, unstaged, untracked, renamed and conflicted paths,
  with rename origins and conflict stages where Git reports them.
- **Changed surface**: working-tree changes together with the files changed by
  commits on this branch since the merge-base. A clean tree on a feature
  branch still reports its diff.
- **Read first**: see [Read first](#read-first).
- **Validation plan**: see [Validation plan](#validation-plan).
- **Coverage** of the branch diff: `available`, `degraded` or `failed`, with a
  reason. An empty list from a failed read is never shown as an observation.
- **Warnings**, **configured exclusions** and **boundaries**.

Ahead and behind are counted from local refs, with no fetch: against the
upstream, else `origin/<default>`, else the default branch of the only remote.
`git.ahead_behind_ref` names which. In a shallow repository the counts are
null and the line says `unknown — the repository is shallow`.

`dirty` counts the repository's work. A path in a generated-state folder
(`.toimija/`, except `repo.yaml`, and `.sykli/`) never makes a state dirty.
The JSON file lists still show everything `git status` reported. The Markdown
counts generated paths apart (`- untracked: 0 (+2 generated, not counted)`),
and they are in neither the changed surface nor Read first.

### Example

`toimija enter` in a small repository with one unstaged change:

```text
# Toimija Context
- compiled-from: 2 changed file(s), 3 recent commit(s) — reading this replaces that discovery

## Repo
- name: demo
- role: service that signs users in
- not: (unset — add to .toimija/repo.yaml)
- kind: Rust

## Languages
- primary: Rust (Cargo.toml at root)
- also present: none

## Last handover
- written: 2026-11-10 14:30 UTC on fix/login-redirect at 0cb97f9
- for the next agent: the redirect is fixed; tests still to write

## Workset contract
Work from this packet's Changed surface and Read first; they are your working
set, not a preamble. A session's declared scope is intent, not evidence.
Configured exclusions are do-not-scan. You MAY read outside the
workset when a concrete unresolved dependency requires it — but you MUST
declare the expansion first: `toimija intent "<why>" --scope <path>` (or, if
you are not under a session, state the expansion and its reason in your
visible output). Expansion is never forbidden; invisible expansion is.

## Git
- branch: fix/login-redirect
- HEAD: 9edfc14e60af8fe7a655146568b120f30a63863a
- detached: false
- upstream: unavailable
- ahead/behind: unknown — no upstream and no remote default branch
- merge-base: main @ 199d5873d4180dfb0c076ac555e1686e74920b7a
- dirty: true
- staged: 0
- unstaged: 1
- untracked: 0
- conflicts: 0

## Git state
Toimija starts from Git’s state machine: HEAD, index, working tree, branch, and merge-base.

## Branch surface
- compared against: main
- merge-base: 199d5873d4180dfb0c076ac555e1686e74920b7a
- files changed since merge-base: 2

## Read first
- README.md (important doc)
- .toimija/repo.yaml (changed)
- src/auth.rs (changed)
- tests/auth.rs (matching test heuristic)

## Validation plan
- source: inferred
### Focused (from changed surface)
- derived from the changed surface — a fast first pass, not a substitute for the full run
- cargo test -p demo
### Fast
- cargo test --workspace
### Strict
- cargo fmt --check
- cargo clippy --workspace --all-targets -- -D warnings
### Full
- cargo test --workspace
### Privileged
- none
### Notes
- Validation inferred from Rust repo shape, not proven by Sykli

## Warnings
- No upstream detected. Branch divergence is unavailable.
- Validation inferred from Rust repo shape, not proven by Sykli
- Working tree has unstaged changes.

## Configured exclusions
- none

## Changed surface
- source: branch diff since merge-base
- 2 files — see Read first

## Recent commits
- 9edfc14 chore: describe the repository
- 0cb97f9 chore: toimija config
- 199d587 feat: add login

## Working tree
### Staged
- none
### Unstaged
- modified src/auth.rs
### Untracked
- none
### Conflicts
- none
```

During a merge, the Git section has one more line, `- operation: merge in
progress`.

### Read first

Read first is a list of files, built from paths. Toimija does not scan the
repository, read history for it, or parse source code. The order is:
configured `read_first` paths, important docs, changed files, then tests that
match a changed file. A path appears once. Configured and important entries
always appear; changed files and tests fill the rest of the 25-entry cap.

Matching tests are found by path, and only when the file exists:

- **Rust**: `src/foo.rs` points to `tests/foo.rs`, `tests/foo_test.rs`,
  `src/tests.rs` or `src/foo_test.rs`. `src/foo/bar.rs` points to
  `tests/foo/bar.rs` and sibling test files. `src/foo/mod.rs` points to
  `tests/foo.rs` or `tests/foo_test.rs`.
- **Elixir**: `lib/foo/bar.ex` points to `test/foo/bar_test.exs`. `mix.exs`
  points to `test/test_helper.exs`.
- **Go, Python, JavaScript and TypeScript**: `pkg/foo.go` points to
  `pkg/foo_test.go`; `src/foo.py` to `src/test_foo.py` or
  `tests/test_foo.py`; `app/foo.ts` to `app/foo.test.ts` or
  `app/foo.spec.ts`.

A file that is itself a test is never given a test hint.

### Validation plan

The packet names how to validate, and Toimija never runs it. The agent runs
the commands and reports the output.

The plan has tiers: `Focused`, `Fast`, `Strict`, `Full` and `Privileged`. They
come from the `validation` section of `.toimija/repo.yaml`, and from the kind
of repository where the configuration leaves them empty. Configured commands
are added to the inferred ones, and duplicates are removed without reordering.
`validation.replace_inferred: true` drops the inferred commands, and the plan
says so.

**A declared gate.** A repository with a `sykli.json` at its root has declared
its checks, so the plan is that gate and nothing is inferred beside it:

```bash
mkdir -p .sykli && sykli run sykli.json --json > .sykli/receipt.json && sykli verify .sykli/receipt.json --contract sykli.json
```

The plan lists the task names the contract declares, with `source:
sykli.json`. Commands in `repo.yaml` add to it. `replace_inferred: true` keeps
the gate. Only `validation.gate` changes it: `gate: false` names no gate, and
a string names that command instead, for example `mise exec -- sykli …`. A
blank string is refused when the configuration loads.

**The focused tier** is derived from the changed surface. In a Rust repository
it maps changed `.rs` files to the crate that owns them and prints `cargo test
-p <crate>`. In an Elixir umbrella it maps changed files to their app and
prints `mix test apps/<name>/test`. It is a quick first pass, not a substitute
for the full run. It is left out when only manifests or documents changed, or
when the change spans too many crates or apps.

| Language | Detected by | Support |
| --- | --- | --- |
| Rust | `Cargo.toml` | Full: inferred tiers, focused tier, test hints. |
| Elixir | `mix.exs` | Full: inferred tiers, focused tier for umbrellas, test hints. |
| Go | `go.mod` | Minimum: `go test ./...`, `go vet ./...`, test hints. |
| Python | `pyproject.toml` or `setup.py` | Minimum: `pytest`, `ruff check .`, test hints. Each command needs its tool configured. |
| JavaScript, TypeScript | `package.json` | Minimum: `npm test`, `npm run lint`, `npm run typecheck`, test hints. Each command needs a matching script. |
| Terraform | a `.tf` file at the root | Minimum: `terraform fmt -check -recursive`. |

A candidate command that cannot be shown to exist is left out with a note. A
minimum plan carries the note `minimum language pack`. A full adapter outranks
a minimum one in a repository with several languages, and configuration
outranks inference everywhere. Adapters give planning hints and do not parse
source code.

The packet also reports secondary languages. They are found by a walk for
manifests that goes three levels deep and reads at most 4096 entries, skips
`.git`, vendored and build folders and configured exclusions, and follows no
symlinks. Secondary languages are reported only and add no commands.

### Budgets and ordering

The JSON packet is always complete. The Markdown is bounded:

- The budget is an approximate cap in UTF-8 bytes, not tokens.
- Repository identity, the workset contract, Git state, Read first, the
  validation plan, warnings, exclusions and the changed-surface summary are
  mandatory. They are never cut, and they may exceed a very small budget.
- An optional section is included whole or left out whole. Left-out sections
  are named under `Budget omissions`, with item counts, and `toimija enter
  --json` has them all.
- A list is capped at 25 entries and ends with `... and N more (truncated)`.

Output is ordered so that packets can be compared:

- Changed files are sorted by path and listed once. Warnings are sorted and
  listed once.
- Top-level sections are `##`, blocks inside them are `###`, and a blank line
  precedes each top-level section.
- An empty list that is known reads `none`. A value Git could not give reads
  `unavailable`.
- Paths are read from Git as bytes. JSON keeps a UTF-8 path as a string and
  writes any other path as `{ "bytes_hex": "..." }`. Markdown escapes tabs,
  newlines, control characters, backslashes and invalid bytes, so a file name
  cannot forge structure.

### The JSON packet

The JSON packet names its schema in `schema`: `toimija-context.v4`.

- New optional fields may appear without a new schema version.
- Removing, renaming or changing a field gives a new schema version.
- A reader ignores fields it does not know and accepts both path forms.

### What a repository says it is: `--identity`

```bash
toimija enter --identity --json
```

```json
{
  "schema": "toimija-identity.v1",
  "repo": {
    "name": "widget",
    "role": "durable task contract",
    "not_roles": [
      "inbox",
      "tracker"
    ],
    "important_docs": [
      "README.md",
      "AGENTS.md"
    ],
    "boundaries": [
      "Do not add network calls."
    ]
  }
}
```

For a tool that asks only what a repository is (ohjaa reads each role from
it), `--identity` prints the packet's `repo` object under the schema
`toimija-identity.v1`: what `.toimija/repo.yaml` says, with the packet's
defaults where it is absent (`role` is `null`). It costs one git process,
which finds the root, collects no Git state, names no session and writes
nothing. It needs `--json` and takes neither `--for-diff` nor `--budget`
(exit 2). Outside a repository, or with a `repo.yaml` that does not parse,
it fails with exit 1.

## Sessions

Each `toimija run` opens its own session, so agents in one repository can see
each other. A session is a marker on disk for exactly as long as its `run`
runs.

### `sessions`

```bash
toimija sessions
toimija sessions --json
toimija sessions --outside-repos
```

```text
LIVE
  claude  1794321108-81405
    branch: fix/login-redirect
    worktree: /work/demo
    intent: fix the login redirect
    scope: src/auth.rs
    started: 3s ago
    seen: 3s ago
```

The listing groups sessions as LIVE and STALE, then prints warnings where
sessions overlap: the same branch, the same worktree, scopes that intersect,
an unknown intent. The warnings describe. They are not locks or assignments,
and Toimija never signals or stops an agent.

- **A session whose process is gone has ended, and is not listed.** Its
  process is probed with `kill -0`, and its start time is compared with the one
  the marker recorded, on Linux and macOS, so a process id that passed to
  another program counts as gone. The next `run` beside it removes its
  marker. A probe that gives no certain answer never counts as gone.
- **Live means seen.** `run` stamps `last_seen_at` once a minute while its
  agent runs, a file write that starts no process; declaring an intent stamps
  it too. A session not seen for 90 minutes whose process is not provably gone
  is stale: its `run` is stopped, or runs where this machine cannot see it.
- **`pid_check`** in the JSON is the process probe on its own: `alive` or
  `unknown`.
- Listing is read-only and never rewrites or removes a marker. The JSON also
  reports how much of the marker scan succeeded; an unreadable marker is named
  in the warnings, never silently dropped.
- Outside a repository, `sessions` lists the sessions opened outside any
  repository, and says so.

Separate worktrees give real isolation: the same code, separate sessions.

#### Finished worktrees

A worktree outlives its work: the branch is merged and deleted on the forge,
and the folder stays, with its build output. In a repository, `sessions` ends
with the worktrees whose work is done, each with the command that removes it:

```text
FINISHED WORKTREES (nothing uncommitted, no live session)
  /work/demo-login  (fix/login-redirect: branch gone on origin)
    git worktree remove /work/demo-login
  /work/demo-docs  (docs/setup: nothing origin/main lacks)
    git worktree remove /work/demo-docs
  Check that no terminal is inside one before removing it. Its branch stays; ignored files such as build output go with the folder.
```

A worktree is listed when all of these hold:

- It is a linked worktree: not the main one, and not the one the command runs
  in.
- Its branch's upstream is gone on origin (Git's `[gone]`: deleted there and
  pruned here by a fetch). "Branch gone on origin" is what Git shows, not
  proof of a merge.
- Or its branch was pushed to an upstream of its own that is still there, and
  the remote default branch already has its HEAD: a merge that kept the
  branch.
- Never a branch that still tracks the default branch or has no upstream, nor
  a detached HEAD: by Git's state alone, such a worktree cannot be told from
  one made a minute ago for work about to start.
- Nothing in it is the repository's work left uncommitted, counted as the
  packet counts `dirty`: tool state under `.toimija/` and `.sykli/` is not.
- It is not locked, and its folder is there.
- No session that is not provably over has a marker in it.

Toimija removes nothing. `--json` lists them under `finished_worktrees`, each
with `path`, `branch`, `reason` (`branch_gone` or
`in_base`) and `against` (the gone upstream, or the default branch). A listing
that could not be worked out whole adds a warning instead of looking complete.

Whether there are other worktrees at all is read from the files in the common
git dir, so a repository with none pays no git process for this. With some,
it costs one `git worktree list`, one `for-each-ref`, one `rev-list` and one
`git status` per worktree that could be finished. A workspace packet does not
list them: it runs no git process per repository.

### `intent`

```bash
toimija intent "repairing token refresh" --scope src/auth/
```

A session declares or sharpens why it is there. `intent` finds its session
through `TOIMIJA_SESSION`. `--scope` adds paths to the declared scope. Both
are advisory text on the marker. An ended session has no marker, and `intent`
says so.

### `handover`

```bash
toimija handover --unfinished "the parser's error tests" --next "the lexer is done; start at src/parse.rs"
toimija handover --surprised "the build writes to a shared folder"
```

Any agent, with or without a session, leaves a note for the next one in this
working copy. At least one of `--unfinished`, `--next` and `--surprised` is
required. The note is written to `.toimija/handover.json` and replaces the one
before it. Every packet worked out in this working copy shows it under "Last
handover", with when it was written and the branch and HEAD at the time.

## The branches in progress: `graph`

`toimija graph` draws the git graph of the base (`origin/main`, or `--base`)
and every local branch not yet in it. Beside each branch tip it puts three
facts and no detail:

- **proof**: what sykli's receipts say of the commit, asked with
  `sykli status --commit`: `proven`, `FAILED`, `not run`, `no gate` (the commit
  carries no `sykli.json`) or `unknown` (sykli could not be asked). Toimija
  runs no check.
- **review**: the level of the review sykli keeps for the commit's tree as a
  git note (`refs/notes/sykli/review`, written by `sykli review`): `fine`,
  `look`, `BLOCKING`, `overruled` (a person let a blocking review cross),
  `stale` or `none`. Findings are not shown. A review answers for the base it
  was read against. `stale` means a review is kept for the tree, but it was
  read against another commit than the one where this tip and the base part
  now, so it says nothing of the tip as it stands. Checking that costs one
  `git merge-base` for each tip that has a review.
- **who is there**: the open sessions on that branch in any worktree of the
  clone, live or stale, each with its intent.

```text
$ toimija graph
* 553a1ff  feat/review-task  proven   review: fine      claude live: "local land step"
| * 77296cc  perf/budgets    not run  review: none
|/
* abf372f  origin/main, main proven   review: none
```

`--json` prints `toimija-graph.v1`: the base, the lines in order (a tip with
its commit, branches, subject, proof, review and sessions, or a graph edge)
and `warnings` naming what could not be read. With a warning the graph is
still printed and the exit code is 4: it is not the whole picture. A branch
that was squash-merged is not an ancestor of the base, so it stays in the
graph until it is deleted. It is read-only and asks no network.

## Workspaces

A workspace is a folder that holds repositories and is not a repository
itself. It describes itself with one file, `.toimija/workspace.yaml`. In a
workspace, `enter` and `run` work from the folder, and the agent gets a packet
about the folder: its repositories, their branches, who is working where, and
what was left for the next agent.

### The file

Write `.toimija/workspace.yaml` by hand:

```yaml
role: the services of one product
read_first:
  - NOTES.md
  - docs/architecture.md
projects:
  - name: Evidence
    role: how a fact gets from an agent's machine into the store
    repositories: [recorder, store]
relationships:
  - from: recorder
    to: store
    kind: sends evidence
    contract:
      - store/docs/statement.md
```

- **`role`**: one line that says what the folder is for.
- **`read_first`**: paths, relative to the folder, that an agent starting
  here reads first.
- **`projects`**: named groups of the folder's repositories, each with a
  `name`, a `role` saying what it is, and its `repositories` by folder name.
  A repository may be in several projects. One that is named but is not in
  the folder stays listed and is named under **Not read**.
- **`relationships`**: connections you declare, each with repository folder
  names `from` and `to`, and a nonblank `kind` describing the connection.
  Missing repositories are named under **Not read**, not silently dropped.
  `contract` is optional: the files both sides must keep compatible, relative
  to the folder. One that is not there is shown as `(missing)`.

All fields are optional, and an empty file is a workspace. A field Toimija
does not know is an error that names the field.

### Discovery

Inside a Git repository the repository is nearer, so it wins: `enter` prints
the repository's packet, even when a workspace file sits above it. That
packet then says what the workspace declares about this repository (below). Outside any repository,
Toimija walks up from the current directory to the first folder that has
`.toimija/workspace.yaml`. The nearest one wins. There is no list of
workspaces anywhere else.

### The packet

`toimija enter` in a workspace prints the packet as Markdown, and `--json`
prints it as JSON with the schema `toimija-workspace.v1`. This is a workspace
of three repositories with one session in a repository and one in the folder
itself:

```text
# Toimija Workspace

## Workspace
- folder: /work
- role: the services of one product

## Read first
- NOTES.md
- docs/architecture.md (missing)

## Repositories
- api
  - role: service that signs users in
  - branch: main
  - session: claude (live): fix the expired-session redirect
  - handover: none
- web
  - role: not set
  - branch: feat/login
  - sessions: none
  - handover: the login form is done; the redirect test still fails
- docs
  - role: not set
  - branch: detached at 2e584f3
  - sessions: none
  - handover: none

## Sessions in this folder
- claude (live): plan the release

## Not in this packet
Uncommitted work and validation are per repository: run `toimija enter` inside a repository for them.
```

What it holds:

- **Workspace**: the folder, its role, and its read-first list. A path that
  does not exist is marked `(missing)`.
- **Repositories**: the entries directly inside the folder that have a `.git`
  directory or a `.git` file. Those with an open session or a waiting
  handover come first, then the rest by name. For each:
  - its role, from its own `.toimija/repo.yaml`;
  - its branch, read from its `HEAD` file. A `.git` file is followed through
    its `gitdir:` line. A detached HEAD shows as `detached at` and the short
    commit id. A `HEAD` that cannot be read shows as `unknown`;
  - its open sessions, live or stale, each with its agent, intent and scope;
  - the first line of a waiting handover.
- **Sessions in this folder**: the sessions `run` opened in the workspace
  folder or a plain folder below it, including their declared scope.
- **Repository relationships**: declared connections and inferred dependencies,
  separately labelled, each citing the file it came from. The JSON adds
  `relationships` (`from`, `to`, `kind`, `basis`, `source`) and
  `relationship_coverage`. Scope is a `scope` array on each listed session.
- **Not read**: anything that could not be read, with the reason. This
  section appears only when there is something to name.
- **Not in this packet**: one line that says what is left out.

The packet is built by reading files. It starts no git process, however many
repositories the folder holds. The one git process `enter` starts here is
the question every command asks first: is this folder inside a repository?

### Inside a member repository

When the folder directly above a repository's main working copy has a
workspace file that says something about it, the repository's packet gains a
section. A linked worktree anywhere on the machine gets the same one, since it
belongs to the same repository:

```text
## Project context
- workspace: the services of one product
- part of Evidence: how a fact gets from an agent's machine into the store
  - with: store
- recorder → store: sends evidence
  - contract: store/docs/statement.md
- store is: keeps and serves the facts
- read first: /work/NOTES.md
- declared in /work/.toimija/workspace.yaml; `toimija enter` in that folder shows every repository and the inferred dependencies
```

It holds the workspace's role, the projects this repository is in with their
other members, the declared relationships that touch it, what each repository
at the other end says it is (from its own `.toimija/repo.yaml`), and the
workspace's read-first paths. Other projects and relationships between other
repositories are left out. Inferred dependencies are not read here. The JSON
packet carries the same under `workspace`. It is read from files and costs no
git process. A workspace file that cannot be read is named in the section and
the packet is still printed; one that declares nothing adds nothing.

### Where the work stands across projects: `graph` and `side`

In a workspace folder, `toimija graph` prints one row for each repository of
the workspace's projects (`--project <name>` for one; with no project
declared, every repository of the folder):

```text
Evidence
  how a fact gets from an agent's machine into the store
● recorder
    feat/retry                      2 changed  proven
    3 ahead, 1 behind its base
    claude live: fix the retry loop
○ store
    main                            clean  none

relations
  recorder → store
    sends evidence
```

Each row has the branch, the paths with uncommitted work, how far the branch
is from its base, what sykli's receipts say of it, and the open sessions. A
filled mark means a session is open there. The declared relationships between
the repositories shown follow, with any contract file that is missing. What is
unknown is said as unknown, never as clean or proven; whatever could not be
read is listed under `not read`, and the exit code is then 4. `--json` prints
`toimija-project.v1`.

This view asks more than the workspace packet, so it costs more: one
`git status` and one `sykli status --json` for each repository shown, all
asked at once. In a repository, `graph` is the branch graph described above.

`toimija side` keeps the same view on screen in a narrow pane, from a
workspace folder or any of its repositories:

- `toimija side --open` asks for a pane on the right and runs there, beside
  the terminal it was typed in: under tmux with `tmux split-window`, in
  Ghostty on macOS through its AppleScript interface (macOS asks once for
  permission). The pane is made 38 columns wide. Elsewhere it prints the
  command to run in a split you open yourself.
- `--open` is a toggle: when the pane is already there, in this tmux window
  or this Ghostty tab, it closes it instead.
- Three views, by number: `1` where each repository of the projects stands,
  `2` the branch graph of the repository picked, `3` the declared relations
  between them. The line under the title shows which is on screen.
- Keys: `j` and `k` or the arrows pick a repository, `p` steps through the
  projects, `r` reads everything again, `q` quits.
- It is an ordinary foreground command that redraws in place. It captures no
  mouse, writes nothing and ends with its pane.
- While nothing moves it starts no process. Every second and a half it reads
  a few file times for each repository; git and sykli are asked again only
  for a repository whose commits, index, branch or receipts moved, and on a
  slow timer for uncommitted edits (20 seconds where a session is open, three
  minutes elsewhere).
- `--once` prints the view and exits, as it does whenever the output is not a
  terminal. `--width` sets the columns.

### Dependency evidence

Inference reads only each listed repository's root `Cargo.toml`, `Cargo.lock`,
`mix.lock` and `rebar.lock`, at most 1 MiB per file. It never runs Cargo, Mix,
Rebar, a build script or a network request. Symbolic links in place of those
files are not followed. Unreadable, oversized and invalid TOML files are
reported under **Not read**; other readable evidence remains available.

Cargo path dependencies are resolved against the manifest folder and matched
to the listed repositories on canonical paths. Git dependencies are shown as
repository URLs, without credentials or revision selectors. Cargo's root
dependency tables, including workspace, target and patch tables, are read;
workspace templates and patch overrides are labelled as candidates that may
be unused, not as dependencies of the package declaring them.
Lockfile Git sources may be transitive and say so. Mix and Rebar lockfiles
contribute only literal Git source tuples, never text inside comments or
quoted examples. Their build definitions are not executed; a missing lockfile
is reported when `mix.exs` or `rebar.config` exists.
Unbalanced containers and incomplete Git tuples are read gaps, not locked
dependencies. Inference requires a literal URL and revision and the expected
Git tuple shape; this structural check does not evaluate either language.
Unsupported Git source variants (such as Rebar's `git_subdir`), non-UTF-8
repository names and unresolvable repository paths are also read gaps.
Declared connections still refer to every listed repository.
Ambiguous Git URLs (including an unencoded `@` in a URL path) are read gaps
without displaying the URL, so malformed userinfo cannot expose credentials.

Coverage is always explicitly **partial**: registry packages, nested manifests,
dynamic path declarations, vendored provenance and runtime connections are not
inferred. Declare those connections when they matter. An empty relationship
list is not evidence that repositories are independent. Toimija reports the
declarations; it does not decide ownership or turn a dependency into a product role.

### Commands in a workspace

| Command | What it does |
| --- | --- |
| `enter` | Print the workspace packet. `--json` prints it as JSON. It writes nothing. |
| `run <agent>` | Open a session as in any folder outside a repository, with the workspace packet in the launch prompt of a known agent. The session is recorded in the state directory, and nothing is written in the folder. |
| `sessions` | List the sessions opened outside any repository, on this machine. |

`--for-diff` and `--budget` shape a repository's packet and are refused in a
workspace. `handover` needs a repository.

### Limits

- A workspace packet does not show uncommitted work, ahead and behind, or a
  validation plan. Run `toimija enter` inside a repository for those.
- A repository's role, a session's intent and a handover are each shown as
  one line of at most 100 characters. The JSON packet carries them whole.
- Only repositories directly inside the folder are listed. A repository two
  levels down is not.
- The branch comes from the `HEAD` file as it is on disk. Toimija does not
  ask Git to interpret it.

## Commands

| Command | What it does |
| --- | --- |
| `run <agent>` | Open a session, start the agent with the packet, end the session and exit with the agent's status. |
| `enter` | Work out the packet and print it. The first step for an agent that is not under `run`, and the way to see the state again. `--identity --json` prints only what the repository says it is. |
| `sessions` | List the sessions working here, with overlap warnings. |
| `intent "<text>"` | Declare or update this session's intent and scope. |
| `graph` | Draw the branches in progress, each tip with its proof, review level and who is there. In a workspace folder: where each project's repositories stand. |
| `side` | Keep the project view on screen in a narrow pane beside your terminal. |
| `handover` | Leave a note for the next agent in this working copy. |

`toimija <command> --help` lists each command's flags with examples.

Global options: `-v` and `-vv` for more log detail, `-q` for none,
`--log-format text|json`, and `--no-color`. Logs go to stderr and command
output goes to stdout. Text logs are coloured only when stderr is a terminal,
and never with `--no-color` or a non-empty `NO_COLOR`. When toimija cannot do
what was asked, it says why once, on stderr, and exits 1; otherwise `run`
exits with its agent's status. `-v` also logs when each command starts and
finishes. A command run in a folder that has been deleted, such as
a removed worktree, says so and names the folder.

`toimija --version` prints the version and the build: `toimija 0.9.0
(a1b2c3d)`, the short commit the binary was built from, with `, dirty` when
the built code differed from that commit. A build without Git or outside a
repository says `unknown commit`.

What each command costs in git processes is pinned by
`tests/cli_budgets.rs`: `enter` finds its worktree with one `git rev-parse`,
reads `git status` once, and reads only the refs and history its output shows,
six in all on a feature branch; `enter --identity --json` costs one. `run`
costs `enter`'s six and one `git status` when the agent exits, however long
the agent runs. `sessions` costs one, and `handover` two. `graph` is not
budgeted there: it asks sykli once for each branch tip, and checks a review's
base with one `git merge-base` for each tip that has a review.

## Configuration

`.toimija/repo.yaml` is optional and written by hand:

```yaml
repo:
  name: example
  role: service that processes customer events
  not:
    - deployment controller

context:
  default_budget: 8000
  important_docs:
    - README.md
    - AGENTS.md
  read_first:
    - docs/architecture.md
  safe_to_ignore:
    - generated/

validation:
  replace_inferred: false
  fast:
    - cargo test
  strict:
    - cargo fmt --check
    - cargo clippy --all-targets -- -D warnings
  full:
    - cargo test --workspace
  privileged: []

agents:
  aider:
    prompt: --message

boundaries:
  - Do not add network calls.
```

- **`repo`**: what the repository is and is not. It appears at the top of
  every packet.
- **`context.default_budget`**: the Markdown cap in bytes when `--budget` is
  not given.
- **`context.important_docs`**: documents every packet lists under Read first.
- **`context.read_first`**: more files to list under Read first, when they
  exist.
- **`context.safe_to_ignore`**: directory rules left out of the changed
  surface and Read first. A rule matches a path segment, or a run of segments,
  anywhere in the tree.
- **`validation`**: see [Validation plan](#validation-plan).
- **`agents`**: how a command takes a launch prompt: `positional` or a flag.
  Names match the base name of the command. A malformed entry is refused when
  the configuration loads.
- **`boundaries`**: sentences that say what the repository must not become.
  Every packet carries them.

A section this version does not read is ignored. Build and vendor folders
(`target`, `_build`, `deps`, `node_modules`, `vendor`) are left out of the
changed surface without configuration, at any depth, and so are the
generated-state folders at the root.

## Security

- Toimija loads no plugins, product code or dynamic libraries.
- The repository cannot choose what runs. `agents:` says how a prompt is
  delivered, and the agent is always the command you name.
- `run` starts the command directly with its arguments. No shell interprets
  them.
- Nothing runs after the command you started ends.
- Toimija makes no network calls.
- Nothing is written or deleted through a symlink. Every directory Toimija
  writes into or deletes from under `.toimija/` must be a real directory,
  checked from the repository root down. Files are replaced by rename, which
  replaces a symlink instead of writing through it. A `.toimija` that is a
  symlink makes `run` and `handover` fail and write nothing.
- Besides its own, `run` removes only session directories that are real
  directories under `.toimija/sessions/`, and only when the process their
  marker names is provably gone.
- Bytes are staged in `.toimija/tmp/`, which Git ignores, so a tool that lists
  untracked files never meets a file that is about to be renamed away.

## Limits

Toimija does not:

- run validation or checks, or write their receipts. That is sykli's part;
- assign, judge, close or dispatch work;
- call a model provider;
- apply patches, commit or stash;
- fetch from the network;
- observe what an agent reads or does while it works. Delivery happens at
  launch, the summary at exit, and nothing in between;
- write `AGENTS.md`, `CLAUDE.md`, Git hooks or harness settings;
- parse source code, or build an MCP, LSP, SCIP, Kythe or Tree-sitter index;
- coordinate work across repositories. A [workspace](#workspaces) shows the
  repositories of one folder and assigns nothing.

Known limits of what it does:

- Git state is read through the Git command line.
- The primary language comes from a manifest at the repository root.
- Upstream and merge-base can be unavailable in a shallow, new, detached or
  local-only repository. The packet says so.
- The packet in a launch prompt is the state at launch. An agent runs `toimija
  enter` again for the state now.
- Coverage says what Toimija read, not what is certain.

## Working on Toimija

Toimija is one Rust crate. The `xtask` crate holds development commands and is
not part of an ordinary build.

```bash
cargo build
cargo run -- enter
cargo install --path .
```

Before a change is done:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test -p xtask
```

`cargo xtask gate` runs the first three. `cargo xtask install-pre-commit-hook`
installs a Git pre-commit hook, once per clone and again after `xtask`
changes. The hook formats the staged crates, stages the result, then runs
clippy and the tests, and blocks the commit when either fails.

### Landing

Work lands with sykli's checks and a plain push, on the committed branch:

```bash
sykli run                   # prove the commit: run the declared checks, keep the receipt
sykli review                # a fresh agent reads the change and says fine, look or blocking
git push origin HEAD:main   # sykli's pre-push hook lets it through when proven and reviewed
```

- `sykli review` starts a fresh agent, another one than the author where one
  is installed, with reading tools only. A reviewer that is missing, fails or
  gives no verdict is no review, never a pass. The verdict is kept as a git
  note on the commit's tree. Only a person at a terminal overrules a blocking
  review, with `sykli review --overrule "<why>"`.
- sykli's pre-push hook (`sykli hook install --pre-push`; plain `install`
  would also take the pre-commit hook xtask installs) refuses a push to `main`
  unless the remote's tip is an ancestor of the commit, its tree has a passing
  receipt, and the review of that tree against that tip is not blocking. It
  shares the receipt and the review as git notes, then lets the push through.
  A push to `main` lands the work, so push only when the person you work for
  says so.

A tag `v*` builds the release binaries in CI.

### The sykli contract

The same checks are declared in `sykli.json`, pinned by `sykli.lock`, and run
with `sykli run`. A task whose inputs have not changed is served from cache.

| Task | Runs | After | Reads |
| --- | --- | --- | --- |
| `fmt` | `cargo fmt --check` | | every `.rs` and `Cargo.toml` |
| `clippy` | `cargo clippy --all-targets -- -D warnings` | `fmt` | the code: `.rs`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/`, and every file a target reads with `include_str!` |
| `test` | `cargo test` | `fmt` | every tracked file |
| `xtask-test` | `cargo test -p xtask` | `fmt` | every tracked file |
| `installer` | `sh -n install.sh && sh installer/test.sh` | | the installer and its test |

Inputs are exact paths, not patterns. A new tracked file goes into `test` and
`xtask-test`, and into `fmt` and `clippy` when it is code. Then `git add -A &&
sykli lock sykli.json` pins the contract again. An input that is not declared
would be served from cache, so `test` declares every tracked file except the
contract and its lock. `tests/contract.rs` checks that every file read with
`include_str!` is an input of `clippy` and `test`, and `tests/docs.rs` that
every command and flag these docs show is one `toimija --help` has.
