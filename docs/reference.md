# Toimija reference

This is the manual: every command, the packet, sessions, configuration and
the guarantees behind them. For a first look, read the [README](../README.md).

Toimija reports facts about a Git repository and never decides. It reads Git
state through the Git command line, combines it with what
`.toimija/repo.yaml` says about the repository, and writes the result where an
agent can read it. It keeps no index of its own. Snapshots are Git tree
objects, freshness is a hash of content, and when something cannot be read the
packet says so.

Contents:

- [Files](#files)
- [Setting up a repository](#setting-up-a-repository)
- [Starting an agent: `run`](#starting-an-agent-run)
- [How the packet stays current](#how-the-packet-stays-current)
- [The packet](#the-packet)
- [Checking freshness: `verify`](#checking-freshness-verify)
- [Repository points](#repository-points)
- [tree-identity.v1](#tree-identityv1)
- [Snapshots](#snapshots)
- [The seal](#the-seal)
- [Sessions](#sessions)
- [Workspaces](#workspaces)
- [Validation plans](#validation-plans)
- [`doctor`](#doctor)
- [`landed`](#landed)
- [Commands](#commands)
- [Configuration](#configuration)
- [Security](#security)
- [Limits](#limits)
- [Working on Toimija](#working-on-toimija)

## Files

Toimija writes only under `.toimija/` in the repository, plus Git objects and
refs under `refs/toimija/` for snapshots. Sessions opened outside any
repository live in a state directory on the machine.

```text
.toimija/repo.yaml       yours     what the repository is, its boundaries, validation commands
.toimija/workspace.yaml  yours     in a folder of repositories: what the folder is for and what to read first
.toimija/.gitignore      Toimija   keeps Git away from the generated files below
.toimija/current.md      Toimija   the repository-level packet, for a reader with no session
.toimija/current.json    Toimija   the same packet as JSON
.toimija/sessions/<id>/  Toimija   one session: its packet, marker, repository point and log
.toimija/handover.json   Toimija   the note for the next agent in this working copy
.toimija/history.jsonl   Toimija   one record per concluded session
.toimija/runs/           harnesses transcripts other tools keep here; `clean` expires them
.toimija/tmp/            Toimija   staging for atomic writes
AGENTS.md, CLAUDE.md     yours     Toimija never writes them
```

Toimija installs nothing on a repository: no Git hook, no agent document, no
harness setting.

## Setting up a repository

```bash
toimija init
```

`init` creates `.toimija/repo.yaml` and `.toimija/.gitignore`, and nothing
else. It never overwrites an existing file. On a new configuration it prints
the next steps: fill in `role`, `not` and `boundaries`, and tell your agents
to run `toimija enter` first or start them with `toimija run`. Only a person
can say what a repository is for and what it must not become.

The generated files are ignored by Git and are not committed.

## Starting an agent: `run`

```bash
toimija run claude
toimija run --intent "fix the login redirect" claude --model opus
toimija run --intent "fix the login redirect" --scope src/auth/ --task "repair the expired-session redirect" -- codex
```

`run` takes the agent's command. What follows the agent's name belongs to the
agent, flags included. A `--` before the agent is accepted and not needed.

`run` does this:

1. Finds the repository and reads its Git state.
2. Writes the repository point for that state.
3. Opens a session under `.toimija/sessions/<id>/` with its own packet.
4. Verifies the packet against the repository and starts the agent directly,
   with no shell in between. It retries one startup race. It refuses to start
   the agent when the packet is still stale or cannot be verified, or when Git
   is in the middle of an operation or has conflicts.
5. Refreshes the packet when the repository changes.
6. When the agent exits, reads the repository again, prints a summary and
   exits with the agent's status. An agent ended by a signal is reported the
   same way: SIGTERM gives 143.

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
budget, the path of the live packet, the task and the briefing.

Known agents are `claude`, `codex`, `gemini`, `pi` and `opencode`, matched by
the base name of the command. `opencode` takes the prompt after `--prompt`,
because its one positional argument is a folder; `opencode run` takes it as
its message. The others take it as a positional argument.

An `agents:` entry in `.toimija/repo.yaml` tells `run` how any other command
takes a prompt: `prompt: positional`, or a flag such as `prompt: --message`.
Configuration wins over the built-in list. It never supplies an executable
path: the agent is always the command you name.

A command `run` does not know gets the session's paths through environment
variables only. `run` says so (`delivery: env-var only — not guaranteed`) and
names what was not delivered (`not delivered: packet, task, briefing`). The
session marker records the same under `undelivered`, and `sessions --json`
shows it.

The launch prompt tells the agent to use the delivered facts instead of
repeating discovery, to refresh when the facts are missing, stale or
contradictory, and to read the named files for task details. Scope is still
declared before work, and `verify` still runs before a commit or a handover.

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
this session's identity and paths. It changes no global Codex configuration,
inheritance policy or sandbox setting. A variable `run` removes from the
agent's environment is set to an empty string for Codex tools, because `set`
can only add. Toimija reads an empty handshake value as unset, and other
readers should too. A value that is not valid UTF-8 stops the launch.

### Environment handshake

`run` sets these for the agent. For a known agent they are a second channel
beside the launch prompt. For any other command they are the only one.

| Variable | Meaning |
| --- | --- |
| `TOIMIJA_RUN=1` | The agent runs under `run`, and the packet is kept current. |
| `TOIMIJA_SESSION` | The session's id, `<unix-seconds>-<pid>`. `intent`, `enter` and `refresh` use it to find the session. |
| `TOIMIJA_RUN_ID` | The same as `TOIMIJA_SESSION`. |
| `TOIMIJA_LIVE` | Absolute path of the session's live `current.md`. |
| `TOIMIJA_LIVE_JSON` | Absolute path of the session's live `current.json`. |
| `TOIMIJA_CURRENT_MD` | The same as `TOIMIJA_LIVE`. |
| `TOIMIJA_CURRENT_JSON` | The same as `TOIMIJA_LIVE_JSON`. |
| `TOIMIJA_REPO_ROOT` | Absolute path of the repository root. |
| `TOIMIJA_REPO_LIVE` | Absolute path of the repository-level `current.md`. |
| `FALSE_SYSTEMS_REPOSITORY_POINT_PATH` | Absolute path of the session's repository point (`.toimija/sessions/<id>/semantic-point.json`), rewritten on every refresh. |

An agent re-reads `$TOIMIJA_LIVE` after HEAD, the branch or the index change,
or after worktree changes it did not make. Its own edits need no re-read.

### The terminal

While the agent owns the terminal, `run` points its own stdout and stderr at
`.toimija/sessions/<id>/diagnostics.log`. Watcher output, refresh warnings and
panic messages go there and cannot mix with the agent's screen. After the
agent exits, `run` reports the number of captured lines and the path.

### The session marker

Each session writes `.toimija/sessions/<id>/session.json`: id, pid, when the
process started (`pid_start`), the agent's command, worktree, branch, HEAD,
intent, scope, timestamps and status. `run` closes it on exit. `toimija
sessions` reads it.

The entry branch and HEAD stay fixed in later packets. When Git state moves
away from them, the Markdown packet shows a `Session drift` block, and the
JSON keeps both the entry state and the current `git` state.

### The exit summary

```text
toimija run: session summary
  branch:    fix/login-redirect
  worktree:  0 staged, 1 unstaged, 0 untracked
  scope:     2 file(s) changed on this branch since merge-base
  refreshed: 0 time(s) while you worked
  intent:    "fix the login redirect" (declared at launch)
  expansions: none declared
```

The summary compares Git state at entry and at exit. A switched branch and a
moved HEAD are shown as `before → after`. The intent and expansion lines come
from the session marker, never from watching the agent.

The worktree line counts the repository's own work. Toimija's files are never
counted, even in a repository where Git lists them, and neither are the other
generated-state folders (see [Repository points](#repository-points)).

### Outside a repository

A folder with a workspace file is a [workspace](#workspaces): `run` opens the
same kind of session there and puts the workspace packet in the launch
prompt. The rest of this section is about a folder with no workspace file.

In a folder that is not inside a Git repository, `run` opens a session with no
packet. There is no Git state to read, so there is no packet, watcher,
repository point or exit summary, and `--for-diff` and `--budget` are refused.
What remains makes the session visible:

- The marker, in the state directory: `$TOIMIJA_STATE_DIR`, else
  `$XDG_STATE_HOME/toimija`, else `~/.local/state/toimija`. It sits at
  `<state>/.toimija/sessions/<id>/session.json`, with `worktree` set to the
  folder. Nothing is written in the folder itself.
- A heartbeat every minute.
- `TOIMIJA_RUN`, `TOIMIJA_SESSION`, `TOIMIJA_RUN_ID` and `TOIMIJA_STATE_DIR`.
  `TOIMIJA_LIVE` and `TOIMIJA_CURRENT_MD` name a short note that says why
  there is no packet. The repository variables are cleared, so values from an
  outer session cannot leak in.
- For a known agent, a launch prompt that says there is no packet, then the
  task. A session with no task waits for work and spends no model calls on
  discovery.

`toimija sessions --outside-repos` lists these sessions. `toimija intent`
finds its session there even after the agent moves into a repository. Sessions
inside the repositories the agent touches do not count it as a sibling, so the
agent should declare its scope.

### After installing a new version

A running `toimija run` keeps the code it started with, and its watcher keeps
writing packets as that version does. Restart live sessions after installing.
`toimija --version` names the commit a binary was built from.

## How the packet stays current

`watch` and `run` refresh the packet while they run, in the foreground. They
stop when the command ends. Standalone `watch` prints its diagnostics to its
own terminal.

**Work-tree edits** arrive as filesystem events: FSEvents on macOS, inotify on
Linux. The watcher reports `armed (native)` only after it has seen a test
event. Events that only read a file are dropped, so a refresh never triggers
itself. When the native watcher cannot start, macOS falls back to polling
(`armed (poll fallback)`) and Linux to one `git status` per tick (`armed (git
polling)`), never a recursive scan of the tree. The fallback is logged once.

**Git changes** are found without starting a git process. Every 500 ms the
watcher compares the file metadata of what a packet depends on: HEAD, the
index, the operation markers, the refs (files or reftable, including a linked
worktree's own), packed-refs, config, `shallow`, `info/grafts`,
`objects/info/alternates`, `info/exclude` and `.toimija/repo.yaml`. A moved
ref, config, graft or alternates file refreshes the packet. A moved HEAD or
index is checked first with one `git status`, because other tools rewrite the
index without changing anything the packet shows. This is how staging,
commits, branch switches and a fetch reach the packet, also in a linked
worktree.

**Build folders.** Events under a directory named `target`, `_build`, `deps`
or `node_modules`, at any depth, prompt one `git status` instead of a refresh.
A tracked file there still shows as modified and refreshes the packet.
Untracked build output costs only the check. Only a whole name counts:
`src/targeted.rs` is source.

**Cost.** A refresh reads one `git status`. While the refs have not moved, the
facts derived from them (the base, the merge-base, the branch diff, recent
commits, ahead and behind) are carried over from the last read. An idle tick
starts no git process. `run` costs what `enter` costs plus nine git processes,
however long the agent works. `tests/cli_budgets.rs` pins the number of git
processes each command starts.

**Safety net.** Once a minute the watcher compares one `git status`, and the
changed files' contents, with the packet. A missed event leaves a packet stale
for a minute at most. Two cases are slow: a change to a global excludes file
waits for that minute or the next real change, and a change to global Git
configuration waits for the next refresh that reads the refs. The debounce
waits for a 300 ms gap in events, so steady churn, such as a long build,
delays refreshes until it pauses.

**Failed refresh.** When a refresh fails, for example on a `repo.yaml` that
does not parse, the Markdown packet gets a notice at the top that names the
failure and since when, above the last good content. The next successful
refresh removes it. The failure is logged once per distinct error. The refresh
is retried on the next change, and otherwise after half a second, then after a
delay that doubles up to one minute.

**After sleep.** When the machine wakes, the next tick refreshes the packet
and the session's heartbeat.

## The packet

The packet is one artifact in two forms:

```text
.toimija/current.md                   Markdown, for a reader with no session
.toimija/current.json                 the same, as JSON
.toimija/sessions/<id>/current.md     one session's packet
.toimija/sessions/<id>/current.json
```

A session's packet is the one its agent trusts. The repository-level pair is a
summary written by whoever refreshed last. It carries a session block only
while that session runs.

`enter`, `refresh`, `watch` and `run` write the packet. `context` prints it
without writing. `--json` prints JSON, `--for-diff` adds a section that lists
committed, staged, unstaged, untracked and conflicted files apart, and
`--budget` caps the Markdown.

### What it holds

- **Repository**: name, role, what it is not, its kind, and other languages
  found.
- **Workset contract**: the rule for reading. Work from the changed surface
  and Read first. Configured exclusions are not scanned. Reading outside the
  workset is allowed when a concrete dependency needs it, and is declared
  first with `toimija intent "<why>" --scope <path>`.
- **Git**: branch, HEAD, upstream, ahead and behind, merge-base, recent
  commits, and whether an operation such as a merge or rebase is in progress.
- **Working tree**: staged, unstaged, untracked, renamed and conflicted paths,
  with rename origins and conflict stages where Git reports them.
- **Changed surface**: working-tree changes together with the files changed by
  commits on this branch since the merge-base. A clean tree on a feature
  branch still reports its diff.
- **Read first**: see [Read first](#read-first).
- **Validation plan**: see [Validation plans](#validation-plans).
- **Coverage** of the branch diff: `available`, `degraded` or `failed`, with a
  reason. An empty list from a failed read is never shown as an observation.
- **Warnings**, **configured exclusions** and **boundaries**.
- **Last handover**: the note the previous agent left.
- **Session**, in a session's packet: id, intent, scope, the entry branch and
  HEAD, and while the session runs an `execution` block with the session's
  repository point. The last packet written as a session exits has no
  `execution` block.
- **Consistency token**: a hash of the Git state the packet describes. See
  [Checking freshness](#checking-freshness-verify).
- **Refresh token**: a hash of the collected Git state. It uses repository
  state only, so one repository gives one token from any process.
- **Seal**: a digest of the packet and where it came from. See
  [The seal](#the-seal).
- **Artifact id**, shared by the Markdown and JSON pair. Each file is replaced
  atomically. When an interrupted refresh leaves two ids, the JSON is right.

Ahead and behind are counted from local refs, with no fetch: against the
upstream, else `origin/<default>`, else the default branch of the only remote.
`git.ahead_behind_ref` names which. In a shallow repository the counts are
null and the line says `unknown — the repository is shallow`.

`dirty` reads what the consistency token reads. A path in a generated-state
folder never makes a state dirty. The JSON file lists still show everything
`git status` reported. The Markdown counts generated paths apart (`-
untracked: 0 (+9 generated, not counted)`), and they are in neither the
changed surface nor Read first.

### Example

`toimija context` in a small repository with one unstaged change:

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

### Budgets and ordering

The JSON packet is always complete. The Markdown is bounded:

- The budget is an approximate cap in UTF-8 bytes, not tokens.
- Repository identity, the workset contract, Git state, Read first, the
  validation plan, warnings, exclusions and the changed-surface summary are
  mandatory. They are never cut, and they may exceed a very small budget.
- An optional section is included whole or left out whole. Left-out sections
  are named under `Budget omissions`, with item counts and where the JSON
  holds them.
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

### Compatibility

The JSON packet is the contract for tools. It names its schema in `schema`,
and again in `seal.schema`. The schema is `toimija-context.v3`.

- New optional fields may appear without a new schema version.
- Removing, renaming or changing a field gives a new schema version.
- A reader ignores fields it does not know, tolerates provenance kinds it does
  not know, and accepts both path forms.
- A reader verifies the seal's digest over the raw JSON before it fills in
  defaults.
- `verify` reads only its own schema. A packet with another schema is
  `invalid_packet`, and the message names `toimija refresh`, which replaces
  it.

## Checking freshness: `verify`

```bash
toimija verify
toimija verify --json || toimija refresh
toimija verify --packet .toimija/current.json --json
```

A packet is useful only while it matches the repository. Every packet carries
a consistency token: a hash, by content, of what `git status` lists. That is
HEAD, the index, changed tracked files, untracked files, conflicts and
operations in progress. It uses no clocks, modification times or file sizes:
the same content gives the same token.

`verify` computes the token again from the live repository and compares.
Under `run` it checks the session's packet; otherwise the repository-level
one. `--packet` selects a file. Exit 0 means verified. Any other exit means
the packet is not to be trusted, and the output says why.

With `--json`, a verified packet reports `"status": "verified"`, the
`packet_path`, the consistency `algorithm` and `token`, and the
`artifact_digest`. The other statuses are:

| Status | Meaning |
| --- | --- |
| `missing_packet` | There is no packet at that path. |
| `invalid_packet` | The file is not a packet this version reads. |
| `missing_consistency` | The packet has no consistency block. |
| `not_comparable` | The token's version is one this version does not compute. The packet is neither fresh nor stale. |
| `unsupported_consistency_algorithm` | The token names an algorithm Toimija does not know. |
| `stale_packet` | The repository has changed. When only ref facts moved (upstream, base, ahead and behind), the message says so. |
| `session_not_live` | The packet briefs a session that has closed or whose process is gone. |
| `seal_digest_mismatch` | The packet's content does not match its seal. |
| `unsafe_git_operation` | Git is in the middle of an operation, or has conflicts. |
| `unverifiable_repository` | The repository's state cannot be read. |
| `io_error` | The file cannot be read. |

A packet with no seal, or a seal whose schema and anchor do not match the
packet's schema and token, is refused.

The token is a freshness check, not an identity. What `git status` does not
list, the token does not see. [Known gaps](#known-gaps) lists the cases.

## Repository points

A repository point names one exact state of a repository: the worktree written
as a Git tree, with the consistency token beside it.

```bash
toimija -q repository-point --json
```

```json
{
  "protocol": { "major": 1, "minor": 0 },
  "repository": {
    "id": "rpoint_v3_48f8052f7b046d137d2b6cd2d71558bece4706e3877388246e3c805acfad18bd",
    "repository": "repo_78f24503915d687826f623de5de1cd204958fe7086f56c8b30833d41990cb6ff",
    "consistency_token": "sha256:36f3ee63d6a14669238371a467f17a8c8ce4d4422e2ef4e3b299996e35e098be",
    "tree_oid": "e009288e3f8f775b96c883b83a32a243bfc07a09",
    "branch": "fix/login-redirect",
    "dirty": true,
    "captured_at": "2026-11-10T14:30:57.253835Z"
  },
  "extensions": {
    "consistency_algorithm": "toimija-consistency-v3",
    "tree_algorithm": "tree-identity.v1"
  }
}
```

The output is a
[`false-agent-protocol`](https://github.com/false-systems/false-agent-protocol)
v1 `RepositoryPointEnvelope`. Inside a session the same point is at
`$FALSE_SYSTEMS_REPOSITORY_POINT_PATH`, and the session's packet carries it as
`session.execution.repository`.

The tree is [tree-identity.v1](#tree-identityv1): every tracked path and every
untracked path that is not ignored, without the generated-state folders, and
with `.toimija/repo.yaml` always included. Snapshots, leases, repository
points and session packets all carry this one tree and name its algorithm.

### What a point guarantees

**Equal point ids mean equal trees.** The id is `rpoint_v3_` followed by the
SHA-256 of six fields, each ended by a NUL byte: the marker `rpoint_v3_`, the
repository ref, the token's version, the token, the tree's algorithm and the
tree. The reverse does not hold: two points can share a tree and differ in
HEAD, the index or the branch.

**The token is a freshness check, not an identity.** Equal tokens do not mean
equal trees. The token leaves out exactly what tree-identity.v1 leaves out, so
a tool that writes into a generated-state folder moves neither the tree nor
the token.

**The token is read again after the tree is written.** If it moved, the point
is read afresh after a short pause, up to three times in all, and then
refused. A point never pairs a tree with a token from a different moment that
the token can see.

**`dirty: false` means `git status` reported nothing outside the
generated-state folders.** It does not mean the tree equals `HEAD^{tree}`.
Several of the cases below print `dirty: false` with a tree that differs from
HEAD's.

A reader that needs one exact state compares tree ids or point ids, never
tokens.

### Known gaps

Each of these changes the tree and not the token.
`tests/cli_point_identity.rs` has one test for each. Point ids always differ
between the state before and the state after. Freshness is blind: after the
change, `verify` still passes the packet and a held point still looks current.

| Change | Point ids | Token |
| --- | --- | --- |
| Edit or create an ignored, untracked `.toimija/repo.yaml` | differ | blind |
| Commit again inside a submodule that is already ahead | differ | blind |
| Commit inside a nested, untracked repository | differ | blind |
| Edit a file marked `assume-unchanged` | differ | blind |
| Edit a file marked `skip-worktree` | differ | blind |
| A same-size edit with the modification time put back, under `core.checkStat=minimal` and `core.trustctime=false` | differ | blind |
| One commit in a sparse and in a full checkout | differ | blind |
| A change made and undone while the tree is written | the point names the tree it wrote | blind |

Three edge cases belong to tree-identity.v1 itself. An ignored
`.toimija/repo.yaml` outside a sparse cone makes the tree write fail. So does
a symlinked `.toimija/`. And "not ignored" depends on each machine's global
ignore file, so one content can have two trees on two machines.

### Versions

Points and tokens carry their version. The token is `toimija-consistency-v3`
and point ids begin `rpoint_v3_`. A token or id of another version keeps the
meaning it was written with. Toimija never recomputes or relabels it, and a
comparison across versions says **not comparable**, never equal or different.

The token has the shape `sha256:<hex>`. Its hash covers the version name, so
tokens of two versions never coincide.

## tree-identity.v1

Tree identity says which content makes up one version of a repository's work.
It is how Toimija names an exact state. The rule is written down here and
implemented in `src/tree_identity.rs`.

**Definition.** The tree identity is the Git tree id of the repository's work
content: every tracked path and every untracked path that is not ignored,
without the namespaces of generated tool state that the version fixes, and
with the tool configuration that the version forces in, whether or not Git
ignores it.

- **Excluded namespaces.** `.toimija`, `.teko` and `.sykli`: three folders of
  generated tool state at the repository root.
- **Forced-in configuration.** `.toimija/repo.yaml`. It is configuration, so
  it is part of the work.

### The procedure

The steps run in this order against a throwaway index:

| Step | Git operation |
| --- | --- |
| Seed from HEAD | `read-tree HEAD`. A failure is tolerated: a repository with no commit has no tracked files. |
| Overlay the worktree | `add -A -- <excludes>`, with one exclude pathspec for each namespace, such as `:(exclude,top,glob)[.]toimija/**`. |
| Drop the namespaces | `rm --cached -r -q --ignore-unmatch .toimija .teko .sykli`. This removes namespace content that is tracked. |
| Force-add the configuration | `add -f -- .toimija/repo.yaml`, only when the file exists in the worktree. |
| Write the tree | `write-tree`. Its output is the tree identity. |

Any tool can run these steps with plain `git` and get the same tree.
`tests/tree_identity_conformance.rs` does exactly that.

### Rules

- **Frozen.** The namespaces, the configuration path and the step order are
  part of the version. Changing one changes every identity already recorded,
  so it needs a new version. A test, `v1_definition_is_frozen`, pins all
  three, and an edit to any of them fails the build.
- **No registration.** The namespaces are not read from configuration.
  Configuration that changes later must not change what an earlier identity
  means.
- **Only real tool state is left out.** A namespace is in the list because a
  tool writes generated state there. A folder that nothing writes to is
  ordinary work content, and it moves the identity.
- **The id is recorded.** Every record that carries a tree names its
  algorithm, `tree-identity.v1`, beside it.
- **A tree identity is not the whole state of a repository.** HEAD, the index
  and an operation in progress are outside it. Two repositories can share a
  tree and differ in the rest.

## Snapshots

```bash
toimija snapshot             # write the tree, print its id
toimija snapshot --lease     # also pin it with a ref
toimija snapshot --list      # held leases
toimija snapshot --release <ID>
```

`verify` says whether a packet is fresh now. A reader that needs one version
to stay readable while the worktree keeps moving takes a snapshot.
`snapshot` writes the working tree, untracked files included, as a Git tree
object under tree-identity.v1. Files are then readable at that version with
`git cat-file`.

- `--lease` pins the snapshot with a Git ref under `refs/toimija/`, so
  `git gc` keeps it until `--release`. Under `run` the lease uses the
  session's id.
- A lease id is 1 to 128 ASCII letters, digits, `.`, `-` or `_`, and does not
  start with `.`. Anything else is refused before anything is touched. A lease
  id is a name, never a path.
- `--json` prints the record. `tree_algorithm` names the tree's algorithm and
  `algorithm` the token's version. A lease records `token_algorithm`.
- A lease marker that names no tree algorithm lists as `toimija-legacy`, and
  one that names no token version lists as `token version unrecorded`.
- `snapshot` refuses while Git is in the middle of an operation, and when the
  worktree moves while the tree is written.
- The listing shortens tree ids at character boundaries. It displays a marker
  and does not validate the tree id the marker claims.

## The seal

Every packet carries a seal under the key `seal`. It answers three questions:
what is this, what was it built from, and has it changed. The envelope format
is named `toimija-seal.v1`, carried in the seal as `envelope`, and
[`docs/schemas/toimija-seal.v1.schema.json`](schemas/toimija-seal.v1.schema.json)
fixes its fields. A seal that names any other format is refused.

- **`digest`**: SHA-256 over the packet's canonical form, which is compact
  JSON with object keys sorted at every level and arrays in order. The digest
  covers the whole packet, the seal included, without the seal's own `digest`
  field. Any tool in any language can compute it again.
- **`provenance`**: typed links to what the packet was built from: a Git
  state, a consistency token, another artifact. The set of kinds is open, and
  a reader tolerates kinds it does not know.
- **`producer`** and **`anchored_at`**: who wrote it, and the consistency
  token it is pinned to.

Because a seal is anchored to a consistency token, an artifact from another
tool that names the same token can be tied to the packet, and through it to
the exact Git state. Joining that chain means writing one JSON block. There is
no service and no index: files and a reader.

`cargo xtask why <digest|token|session-id>` is a development tool in this
repository. It verifies the sealed packets under `.toimija/`, prints the
verified links as an indented chain, tolerates unknown provenance kinds, and
reports a digest mismatch without walking through it.

## Sessions

Each `toimija run` opens its own session with its own packet, so agents in one
repository do not write over each other's context.

### `sessions`

```bash
toimija sessions
toimija sessions --json
toimija sessions --prune
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
    packet: .toimija/sessions/1794321108-81405/current.md
```

The listing groups sessions as LIVE, STALE and CLOSED, then prints warnings
where sessions overlap: the same branch, the same worktree, scopes that
intersect, an unknown intent. The warnings describe. They are not locks or
assignments, and Toimija never signals or stops an agent.

- **Stale means inactive, not old.** The watcher stamps `last_seen_at` on the
  marker at every refresh. A session is stale when its process is gone or it
  has shown no activity for 90 minutes. A session that works all day stays
  live.
- **`pid_check`** in the JSON is the process probe on its own: `alive`, `dead`
  or `unknown`. A stale session with a dead process is over. One with an alive
  or unknown process is uncertain.
- **`pid_start`** records when the process started, on Linux and macOS, so a
  process id that passed to another program reads `dead`. On other systems a
  reused id can keep an old session uncertain.
- Listing is read-only and never rewrites a marker. The JSON also reports how
  much of the marker scan succeeded.
- Outside a repository, `sessions` lists the sessions opened outside any
  repository, and says so.

Separate worktrees give real isolation: the same code, separate sessions.

### `intent`

```bash
toimija intent "repairing token refresh" --scope src/auth/
```

A session declares or sharpens why it is there. `intent` finds its session
through `TOIMIJA_SESSION`. `--scope` adds paths to the declared scope. Both
are advisory text on the marker.

### `handover`

```bash
toimija handover --unfinished "the parser's error tests" --next "the lexer is done; start at src/parse.rs"
toimija handover --surprised "the build writes to a shared folder"
```

Any agent, with or without a session, leaves a note for the next one in this
working copy. At least one of `--unfinished`, `--next` and `--surprised` is
required. The note is written to `.toimija/handover.json` and replaces the one
before it. Every packet built in this working copy shows it under "Last
handover", with when it was written and the branch and HEAD at the time.
`handover` refreshes the live packet as it writes.

### `history`

```bash
toimija history
toimija history --json
```

Concluded sessions are kept as records in `.toimija/history.jsonl`, one JSON
object per line. `run` records a clean exit at once. Pruning records a closed
or stale session before it removes the directory, and keeps the directory when
the record cannot be written. `history` prints the newest first, and `--json`
prints every valid record as an array. Malformed lines are counted and
skipped. The record's shape is
[`docs/schemas/history-record.v1.schema.json`](schemas/history-record.v1.schema.json).
Its `handoffs` and `handoffs_total` fields are always empty: a handover is a
note per working copy, not a history record.

### `clean`

```bash
toimija clean --dry-run
toimija clean
```

One pass removes what has expired: closed or stale sessions past retention,
old transcripts under `.toimija/runs/`, and leftover temporary files of
Toimija's own atomic writes that are older than one hour. Live sessions and
unreadable sessions are never touched. `--dry-run` lists the same candidates
and changes nothing. `sessions --prune` runs the same pass.

`enter` and `run` run this pass at the end of setup when `auto_sweep` is on.
An automatic pass logs one warning on failure and never fails the command. An
explicit `clean` reports its errors.

## Workspaces

A workspace is a folder that holds repositories and is not a repository
itself. It describes itself with one file, `.toimija/workspace.yaml`. In a
workspace, `context`, `enter` and `run` work from the folder, and the agent
gets a packet about the folder: its repositories, their branches, who is
working where, and what was left for the next agent.

### The file

```bash
toimija init --workspace
```

`init --workspace` writes `.toimija/workspace.yaml` as a commented template,
and `.toimija/.gitignore`. It refuses inside a Git repository, where `init`
seeds the repository's own configuration, and it refuses when the file is
already there.

```yaml
role: the services of one product
read_first:
  - NOTES.md
  - docs/architecture.md
```

- **`role`**: one line that says what the folder is for.
- **`read_first`**: paths, relative to the folder, that an agent starting
  here reads first.

Both fields are optional, and an empty file is a workspace. A field Toimija
does not know is an error that names the field.

### Discovery

Inside a Git repository nothing changes: the repository is nearer, so it
wins, even when a workspace file sits above it. Outside any repository,
Toimija walks up from the current directory to the first folder that has
`.toimija/workspace.yaml`. The nearest one wins. There is no list of
workspaces anywhere else.

### The packet

`toimija context` in a workspace prints the packet as Markdown, and
`--json` prints it as JSON with the schema `toimija-workspace.v1`. This is a
workspace of three repositories with one session in a repository and one in
the folder itself:

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
Uncommitted work and validation are per repository: run `toimija context` inside a repository for them.
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
  - its open sessions, live or stale, each with its agent and intent;
  - the first line of a waiting handover.
- **Sessions in this folder**: the sessions `run` opened in the workspace
  folder or a plain folder below it.
- **Not read**: anything that could not be read, with the reason. This
  section appears only when there is something to name.
- **Not in this packet**: one line that says what is left out.

The packet is built by reading files. It starts no git process, however many
repositories the folder holds. The one git process a command starts here is
the question every command asks first: is this folder inside a repository?

### Commands in a workspace

| Command | What it does |
| --- | --- |
| `init --workspace` | Write `.toimija/workspace.yaml`. |
| `context` | Print the workspace packet. `--json` prints it as JSON. |
| `enter` | Print the workspace packet. It writes nothing. |
| `run <agent>` | Open a session as in any folder outside a repository, with the workspace packet in the launch prompt of a known agent. The session is recorded in the state directory, and nothing is written in the folder. |
| `sessions` | List the sessions opened outside any repository, on this machine. |
| `doctor` | Check that the workspace file parses and that its read-first paths exist. |

`--for-diff`, `--budget` and `context --identity` shape a repository's packet
and are refused in a workspace. The other commands need a repository.

### Limits

- A workspace packet does not show uncommitted work, ahead and behind, or a
  validation plan. Run `toimija context` inside a repository for those.
- A repository's role, a session's intent and a handover are each shown as
  one line of at most 100 characters. The JSON packet carries them whole.
- Only repositories directly inside the folder are listed. A repository two
  levels down is not.
- The branch comes from the `HEAD` file as it is on disk. Toimija does not
  ask Git to interpret it.
- The packet is not written to disk, has no seal and is not checked by
  `verify`. Print it again to get the current state.

## Validation plans

```bash
toimija validate --plan
```

`validate --plan` prints commands and where each comes from. It never runs
them. The agent runs them and reports the output.

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

### Languages

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

Any other language works through configuration:

```yaml
validation:
  fast:
    - ./gradlew test
  strict:
    - ./gradlew check
  full:
    - ./gradlew build
```

## `doctor`

```bash
toimija doctor
toimija doctor --strict
```

`doctor` reports setup gaps as warnings and exits 0. `doctor --strict` turns
every actionable warning into a failure, so a CI step or a pre-commit hook can
stop drift. The check compares text. It checks that:

- `.toimija/repo.yaml` exists and parses;
- `repo.name`, `repo.role` and `boundaries` are not empty;
- there is a validation plan, configured or inferred;
- Git ignores every generated path that `.toimija/.gitignore` names;
- every `context.important_docs` path exists;
- a `CLAUDE.md`, where there is one, imports `AGENTS.md` with `@AGENTS.md`;
- an agent document sends agents through `toimija enter`;
- each boundary sentence appears in the agent documents (`AGENTS.md`,
  `CLAUDE.md` or the important docs), compared without regard to case or
  line wrapping.

A failing run ends, on stderr, with the names of the checks that failed.

## `graph`

`toimija graph` draws the git graph of the base (`origin/main`, or `--base`)
and every local branch not yet in it. Beside each branch tip it puts three
facts and no detail:

- **proof**: what sykli's receipts say of the commit, asked with
  `sykli status --commit`: `proven`, `FAILED`, `not run`, `no gate` (the commit
  carries no `sykli.json`) or `unknown` (sykli could not be asked). Toimija
  runs no check.
- **review**: the level of the review kept for the commit's tree as a git note
  (`refs/notes/toimija/review`): `fine`, `look`, `BLOCKING`, `overruled` (a
  person let a blocking review cross) or `none`. Findings are not shown.
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
graph until it is deleted; `toimija landed` answers that by content. It is
read-only and asks no network.


## `landed`

```bash
git fetch
toimija landed --base origin/main
```

```text
Toimija landed

- base: origin/main
- branch: feat/doctor-strict-drift-guard (a13296a)
- status: landed
- reason: all 7 touched path(s) are reflected in `origin/main`
- reflected: 7/7
```

`landed` says whether a branch's work is in a base ref, by content. Ancestry
cannot answer this after a squash merge: the squash commit has no link to the
branch tip, and patch ids fail too, because the commits are fused.

For each path the branch touched, `landed` asks Git whether the branch's
change still applies to the base (not there yet), applies in reverse (there),
or neither (the base has diverged on that file). The diverged case is reported
as ambiguous, never as landed. A line comparison is shown as evidence on an
ambiguous path and does not decide the status.

| Status | Meaning |
| --- | --- |
| `landed` | Every touched path's change is in the base, or HEAD is in the base and the worktree holds no uncommitted work. |
| `not landed` | No change is in the base, or HEAD is in the base and the worktree holds uncommitted work. |
| `partially landed` | Some paths are in the base and some are not. |
| `ambiguous` | The base diverged on at least one path. The paths and their evidence are listed. |
| `unknown` | There is no merge-base, or HEAD's own commits change nothing. |

Away from the base, only commits are compared. The check uses a scratch index
and never touches your index or working tree. `--base` defaults to
`origin/main`, and `--json` prints the report as JSON.

## Commands

| Command | What it does |
| --- | --- |
| `init` | Create `.toimija/repo.yaml` and `.toimija/.gitignore` when missing. `--workspace` creates `.toimija/workspace.yaml` in a folder of repositories. |
| `run <agent>` | Open a session, start the agent with the packet, keep the packet current, exit with the agent's status. |
| `enter` | Write the packet and print it. This is the first step for an agent that is not under `run`. |
| `context` | Print the packet without writing it. `--identity --json` prints only what `repo.yaml` says the repository is, under schema `toimija-identity.v1`, with one git process. |
| `refresh` | Write the packet again. |
| `watch` | Keep the packet current until stopped, in the foreground. |
| `verify` | Check that a packet still matches the repository. |
| `sessions` | List the sessions in this repository, with overlap warnings. |
| `intent "<text>"` | Declare or update this session's intent and scope. |
| `handover` | Leave a note for the next agent in this working copy. |
| `history` | List the records of concluded sessions. |
| `clean` | Remove expired sessions, transcripts and temporary files. |
| `validate --plan` | Print the validation plan. |
| `repository-point` | Print the exact current repository point. |
| `snapshot` | Write the working tree as a Git tree object, and optionally pin it. |
| `landed` | Say whether this branch's work is in a base ref, by content. |
| `graph` | Show the branches not yet in the base, each with proof, review level and open sessions. |
| `doctor` | Check the setup; `--strict` fails on drift. |

`toimija --help` marks `watch`, `run`, `sessions` and `snapshot` as
experimental. `toimija <command> --help` lists each command's flags with
examples.

Global options: `-v` and `-vv` for more log detail, `-q` for none,
`--log-format text|json`, and `--no-color`. Logs go to stderr and command
output goes to stdout. Text logs are coloured only when stderr is a terminal,
and never with `--no-color` or a non-empty `NO_COLOR`.

`toimija --version` prints the version and the build: `toimija 0.2.0
(a1b2c3d)`, the short commit the binary was built from, with `, dirty` when
the built code differed from that commit. A build without Git or outside a
repository says `unknown commit`.

Every command finds its worktree with one `git rev-parse`, reads `git status`
once, and reads nothing its output does not show.

## Configuration

`.toimija/repo.yaml`:

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

stewardship:
  auto_sweep: true
  sessions_retention_days: 7
  runs_retention_days: 14
  history_max_records: 500

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
  `doctor` checks that they exist.
- **`context.read_first`**: more files to list under Read first, when they
  exist.
- **`context.safe_to_ignore`**: directory rules left out of the changed
  surface and Read first. A rule matches a path segment, or a run of segments,
  anywhere in the tree.
- **`validation`**: see [Validation plans](#validation-plans).
- **`stewardship`**: what `clean` keeps. The values above are the defaults.
  Retention and history limits must be greater than zero; other values are
  refused and `doctor` reports them. `auto_sweep: false` turns off only the
  automatic pass in `enter` and `run`.
- **`agents`**: how a command takes a launch prompt: `positional` or a flag.
  Names match the base name of the command. A malformed entry is refused when
  the configuration loads.
- **`boundaries`**: sentences that say what the repository must not become.
  `doctor --strict` checks that the agent documents repeat them.

Build and vendor folders (`target`, `_build`, `deps`, `node_modules`,
`vendor`) are left out of the changed surface without configuration, at any
depth, and so are the generated-state folders at the root.

## Security

- Toimija loads no plugins, product code or dynamic libraries.
- The repository cannot choose what runs. `agents:` says how a prompt is
  delivered, and the agent is always the command you name.
- `run` starts the command directly with its arguments. No shell interprets
  them.
- Nothing runs after the command you started ends.
- Toimija makes no network calls.
- Nothing is written, appended or deleted through a symlink. Every directory
  Toimija writes into under `.toimija/` must be a real directory, checked from
  the repository root down. Files are opened without following links and
  replaced by rename. A `.toimija` that is a symlink makes `enter`, `refresh`,
  `run`, `init`, `snapshot --lease` and `clean` fail and write nothing.
- `clean` deletes nothing unless its parent resolves inside the repository's
  own `.toimija/`.
- Packet bytes are staged in `.toimija/tmp/`, which Git ignores, so a tool
  that lists untracked files never meets a file that is about to be renamed
  away.

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
- Coverage says what Toimija read, not what is certain.

## Working on Toimija

Toimija is one Rust crate. The `xtask` crate holds development commands and is
not part of an ordinary build.

```bash
cargo build
cargo run -- context
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

### Landing without a pull request (a trial)

Two development commands in `xtask` try out landing work without a pull
request. They are not part of the `toimija` binary.

- `cargo xtask review` starts a fresh agent on the committed branch, another
  one than the author where one is installed, with reading tools only. It
  answers `fine`, `look` or `blocking`. A reviewer that is missing, fails or
  gives no verdict is no review, never a pass. The verdict is kept as a git
  note on the commit's tree (`refs/notes/toimija/review`). Only a person at a
  terminal overrules a blocking review, with `--overrule "<why>"`.
- `cargo xtask land` pushes the branch to `main` when `origin/main` is an
  ancestor of the commit, sykli says its tree passed, and the review of that
  tree against that `main` is not blocking. It shares the review and the
  sykli receipt as git notes on the tree, then pushes.

The `false-agent-protocol` dependency is public and fetched over HTTPS. A tag
`v*` builds a Linux aarch64 binary in CI.

### The sykli contract

The same checks are declared in `sykli.json`, pinned by `sykli.lock`, and run
with `sykli run`. A task whose inputs have not changed is served from cache.

| Task | Runs | After | Reads |
| --- | --- | --- | --- |
| `fmt` | `cargo fmt --check` | | every `.rs` and `Cargo.toml` |
| `clippy` | `cargo clippy --all-targets -- -D warnings` | `fmt` | the code: `.rs`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/` |
| `test` | `cargo test` | `fmt` | every tracked file |
| `xtask-test` | `cargo test -p xtask` | `fmt` | every tracked file |
| `doctor` | `cargo run --quiet -- doctor --strict` | `test` | every tracked file |

Inputs are exact paths, not patterns. A new tracked file goes into `test`,
`xtask-test` and `doctor`, and into `fmt` and `clippy` when it is code. Then `git add -A &&
sykli lock sykli.json` pins the contract again. An input that is not declared
would be served from cache, so `test` and `doctor` declare every tracked file
except the contract and its lock.
