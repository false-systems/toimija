# Toimija

**The live entrance for agents into a repo.**

![Rust](https://img.shields.io/badge/rust-single_crate-orange)
![daemons](https://img.shields.io/badge/daemons-zero-success)
![network](https://img.shields.io/badge/network-never-success)
![posture](https://img.shields.io/badge/posture-advisory-blue)
![source of truth](https://img.shields.io/badge/source_of_truth-git-8b5cf6)

*Toimija* is Finnish for "the one who acts" — an actor, an agent, an operative.
This Toimija is the layer between a repo and the agents acting on it.

Instead of running `codex` directly, run `toimija run -- codex`.
Instead of running `claude` directly, run `toimija run -- claude`.

Toimija refreshes the repo context, writes `.toimija/current.md` and
`.toimija/current.json`, starts a watcher, launches the agent, keeps the packet
fresh while the agent works, and exits with the wrapped command's status.

> We do not ask the model to understand the repo first. We ask Git what state the repo is in.

## Toimija in 30 seconds

```bash
cargo install --path .          # or grab the release artifact
cd /path/to/repo
toimija init                    # once per repo: seed .toimija/repo.yaml
toimija run -- claude           # every time: wrap the agent
```

```text
                ┌───────────────────────────────────────────────┐
                │                     Git                       │
                │  HEAD · index · merge-base · branch · history │
                └──────────────────────┬────────────────────────┘
                                       │ read (git CLI, bounded)
                                       ▼
  .toimija/repo.yaml  ──────►   toimija compile   ◄────── language adapters
  (durable meaning:                    │                  (path heuristics,
   role, boundaries)                   │                   no parsing)
                                       ▼
                ┌───────────────────────────────────────────────┐
                │     .toimija/current.md  /  current.json      │
                │   scope · read order · coupling · risk ·      │
                │   validation plan · consistency token         │
                └──────────────────────┬────────────────────────┘
                                       │ verify-before-read
                                       ▼
                        the agent (claude, codex, …)
```

## Why

Every coding agent enters a repo the same way: glob, grep, read, guess. That
cold crawl is slow, costs tokens, and is wrong the moment the repo moves. Yet
every repo already ships a database that knows exactly what is going on — Git.
Toimija reads it once, compiles it into a small packet, keeps that packet
provably fresh, and hands it to the agent. The agent starts oriented instead of
archaeologizing.

What you get:

| | |
|---|---|
| **A live work packet** | Branch, HEAD, merge-base, changed surface, warnings — refreshed on every relevant change while the agent works. |
| **Git-derived relevance** | What to read first, what co-changes, what is risky, what is mechanical dep-bump ripple to skip. No parsing, no index — just Git, used well. |
| **Verifiable freshness** | Every packet carries a content-based consistency token; `toimija verify` proves it still matches the repo before anyone trusts it. |
| **Multi-agent awareness** | Per-session packets, `toimija sessions` presence + overlap warnings, 7-day GC. Visibility, never orchestration. |
| **Honest validation** | `validate --plan` prints the repo's real gates (including a focused tier derived from the diff). Toimija never runs them for you. |
| **Squash-proof landing checks** | `toimija landed` answers "did this branch land?" by content, where ancestry lies. |

## What Toimija is

Toimija is a Git-aware local context compiler and live entrypoint for coding
agents.

It combines:

- durable repo meaning from `.toimija/repo.yaml`
- live Git state
- Git-derived relevance
- language adapter hints
- validation plans

into:

- `.toimija/current.md` — the readable live packet
- `.toimija/current.json` — the machine-readable live packet

Agents should work from those files instead of reading the repo cold.

## Git as the starting point

Most agent tools make the model rediscover the repo from files.

Toimija starts from the one truth database every repo already has: Git. HEAD, refs, index, merge-base, staged and unstaged changes, dirty state, branch divergence, and reflog are not just metadata. They are the repo's working state machine.

Toimija reads that state machine first, then compiles it into a small context packet: what changed, what is risky, what to read, what to ignore, and what validation can honestly prove.

Git is not a save button here. Git is the substrate.

> Git gives us the graph of what happened. Toimija turns that graph into operational context for agents.

## The normal workflow

```bash
cd /path/to/repo
toimija run -- codex
```

```bash
cd /path/to/repo
toimija run -- claude
```

`run` is the recommended agent entrypoint. It runs `enter`, starts `watch`,
launches the agent command, stops watching when the agent exits, and returns the
agent command's exit status (signal termination is preserved too, e.g. SIGTERM →
143).

```text
toimija run --intent "fix #51" -- codex
 │
 ├─ 0  GC     prune closed/stale sessions past the 7-day retention (best-effort)
 ├─ 1  enter  open .toimija/sessions/<id>/ · write the session packet + marker
 ├─ 2  gate   verify the entry packet against the live repo before launch
 ├─ 3  watch  refresh the packet on every relevant Git/fs change (foreground)
 ├─ 4  exec   codex, with the TOIMIJA_* handshake in its environment
 └─ 5  exit   close the marker · print a Git entry→exit summary · adopt codex's
              exit code (128+signal preserved)
```

Each `run` opens a **session** with its own live packet under
`.toimija/sessions/<id>/`, so concurrent wrapped agents do not race on a single
shared file. While the agent works, that session packet stays fresh: every
relevant filesystem or Git-state change re-runs the refresh after a short
debounce. The watcher is foreground-only and stops the moment the agent exits —
no daemon survives the command. The repo-level `.toimija/current.*` is kept too,
demoted to a last-writer-wins "latest" summary for sessionless readers.

So the wrapped agent can *find* its live packet, `run` hands it an environment
handshake — the only signal `run` injects into the child:

| Variable | Meaning |
| --- | --- |
| `TOIMIJA_RUN=1` | The agent is under `run`; a watcher is keeping the packet fresh. |
| `TOIMIJA_SESSION` | This session's id (`<unix-secs>-<pid>`). `toimija intent`, `enter`, and `refresh` use it to find the session. |
| `TOIMIJA_LIVE` | Absolute path to this session's live `current.md` (authoritative). |
| `TOIMIJA_LIVE_JSON` | Absolute path to this session's live `current.json`. |
| `TOIMIJA_CURRENT_MD` | Alias of `TOIMIJA_LIVE`, for harnesses that key on the file name. |
| `TOIMIJA_CURRENT_JSON` | Alias of `TOIMIJA_LIVE_JSON`. |
| `TOIMIJA_REPO_ROOT` | Absolute path of the repo root the session runs in. |
| `TOIMIJA_RUN_ID` | Alias of `TOIMIJA_SESSION`. |
| `TOIMIJA_REPO_LIVE` | Absolute path to the repo-level `current.md` summary. |

An agent that honors the handshake re-reads `$TOIMIJA_LIVE` after Git-state
changes instead of re-running `enter`. `toimija snippet preflight` already tells
agents to do this.

Each session also writes a `.toimija/sessions/<id>/session.json` marker (id, pid,
agent argv, worktree, branch, HEAD, intent, scope, timestamps, status) that `run`
auto-closes on exit. The marker is what `toimija sessions` reads (below) and the
foundation for the `toimija handoff` record designed in
`docs/proposals/0001-sessions-awareness-handoff.md`.

## Multi-agent sessions

Several agents can work one repo at once without trampling each other's context.
Each wrapped agent gets its **own** session packet, and `toimija sessions` makes
the others visible.

Declare why you're here (and optionally what you'll touch) when you launch — it's
advisory, never an assignment or a lock:

```bash
toimija run --intent "add sessions command" --scope src/sessions.rs -- codex
```

The wrapped agent can also speak for itself, mid-session — it finds its own
marker through the `TOIMIJA_SESSION` handshake:

```bash
toimija intent "migrating the auth module" --scope src/auth/
```

Launch-time `--intent` belongs to the human; `toimija intent` lets the agent
declare (or sharpen) it once it actually knows what it is doing. Same rules:
advisory text on the session marker, never an assignment, never a lock.

Then, from anywhere in the repo, see who else is live:

```bash
toimija sessions          # human listing: LIVE / STALE / CLOSED + warnings
toimija sessions --json   # same, machine-readable
toimija sessions --prune  # remove closed/stale sessions older than 7 days
```

```
LIVE
  claude  1782675584-99172
    branch: feat/sessions-awareness
    worktree: /Users/yair/projects/toimija
    intent: add sessions command
    scope: src/sessions.rs
    started: 12m ago
    seen: 8s ago
    packet: .toimija/sessions/1782675584-99172/current.md

WARNINGS
  - 2 live sessions on branch feat/sessions-awareness: claude (…), codex (…)
```

`sessions` is read-only and local: it reports presence, resolves best-effort
liveness (`active` / `stale` / `closed`), and warns when sessions overlap (same
branch, same worktree, intersecting declared scope, unknown intent, …).
Staleness measures **inactivity, not lifetime**: `run`'s watcher stamps a
`last_seen_at` heartbeat on the marker at every packet refresh, and a session
is stale when its process is gone or it has shown no activity for 90 minutes —
so a day-long working session stays `LIVE`, while a frozen one decays honestly
(see `docs/proposals/0002-heartbeat-staleness.md`). The
warnings are **descriptive, never prescriptive** — Toimija coordinates
visibility, not decisions. It never assigns work, signals or kills an agent, or
touches the network. The human remains the orchestrator. Prefer separate
worktrees for true isolation: same codebase, separate sessions.

Session directories do not accumulate forever: `toimija sessions --prune`
removes closed or stale sessions older than a 7-day retention window (live
sessions are never touched), and `toimija run` performs the same cleanup
opportunistically at startup — synchronous, best-effort, never blocking the
run.

When the agent exits, `run` prints a one-glance session summary to stderr —
diffing the Git state from entry to exit so you see what the session actually
did: a switched branch, a moved HEAD (commits landed), the working tree it
leaves behind, branch scope since the merge-base, and how many times the live
packet refreshed:

```
toimija run: session summary
  branch:    main → feature/login
  HEAD:      a1b2c3d → e4f5a6b (moved)
  worktree:  2 staged, 1 unstaged, 0 untracked
  scope:     4 file(s) changed on this branch since merge-base
  refreshed: 12 time(s) while you worked
```

## One-time repo setup

```bash
toimija init
```

`init` creates Toimija-owned files only:

- `.toimija/repo.yaml` — the durable repo manifest. Fill in `role`, `not`, and
  `boundaries`; only a human can supply the repo's purpose and limits.
- `.toimija/current.md` / `.toimija/current.json` — generated on every run. Add
  them to `.gitignore`; they are not committed.

Toimija does not own root agent policy files by default. To seed
`AGENTS.md`/`CLAUDE.md`, run `toimija snippet preflight` and paste the output into
whichever doc your agents read; `toimija snippet script` prints a
`scripts/agent-enter` stub.

> repo.yaml gives durable meaning. Git gives live work state. Toimija compiles both into current context.

## Current context

`.toimija/current.md` is the readable live packet. `.toimija/current.json` is the
machine-readable live packet. Both are regenerated by `refresh`, `enter`, `watch`,
and `run`, and are git-ignored.

The packet includes:

- branch / HEAD / dirty state
- staged / unstaged / untracked files
- changed surface
- scope
- read order
- coupling
- risk
- validation plan
- warnings
- a **consistency** block — a versioned `toimija-consistency-v2` freshness token
  over HEAD, staged/index state, unstaged tracked state, untracked state,
  conflict state, and unsafe Git operation state

See **Verify before read** for what that token buys you, and **Example context
output** below for a full rendering.

## Verify before read — the packet is a claim, not a hope

A context packet is only useful while it still matches the repo. A map an agent
trusts *after* the territory moved is worse than no map. So Toimija makes
freshness checkable instead of assumed, in three escalating moves:

1. **Fingerprint.** Every packet embeds a `toimija-consistency-v2` token — a
   deterministic, content-based fingerprint of the repo's work state (HEAD,
   index, unstaged tracked content, untracked content, conflicts, in-flight Git
   operations). No clocks, no mtimes, no file sizes: same content, same token.
   Toimija's own output (`current.*`, `sessions/`) is excluded, so a refresh
   never invalidates itself.
2. **Gate.** `toimija verify` (or `toimija current --verify --json` for
   machines) recomputes the fingerprint against the live repo and exits
   non-zero, with reasons, when the packet is stale, missing, unsupported,
   unsafe, or unverifiable. A harness calls this before *every* packet read:
   pass → trust the packet; fail → refresh first. `run` performs the same check
   on its own entry packet before launching the agent, so a startup race is
   surfaced, never silent.
3. **Freeze.** When "fresh right now" is not enough — a reader that must see
   one coherent version while the worktree keeps moving — `toimija snapshot`
   materializes the working tree (untracked included) as an immutable,
   content-addressed git tree object. Files are then readable *at that version*
   via `git cat-file`, and `--lease` pins the snapshot against `git gc` until
   released.

The token is a transaction version, not a timestamp: verify tells you whether
the world changed, snapshot gives you a world that cannot change. Either way,
nothing reads repo state on faith.

## Commands

Ordered by workflow:

```bash
toimija run -- codex       # agent entrypoint
toimija run -- claude
toimija run --intent "..." --scope src/x.rs -- codex  # declare why / what (advisory)
toimija sessions           # who else is live in this repo (read-only awareness)
toimija intent "..."       # wrapped agent declares why it is here (via TOIMIJA_SESSION)
toimija enter              # refresh + print the packet as agent preflight
toimija refresh            # recompute current.md/current.json
toimija watch              # keep current.* fresh on Git/filesystem changes
toimija context            # print the packet without wrapping an agent
toimija context --json
toimija current --write    # synchronously write current.md/current.json
toimija current --verify --json  # machine-readable verify-before-read gate
toimija verify             # is current.json still consistent with the live repo?
toimija snapshot           # freeze the worktree as an immutable git tree (content @ version)
toimija validate --plan    # print the validation plan; never runs it
toimija doctor             # check Toimija setup
toimija init               # create .toimija/repo.yaml if missing
```

| Command | What it does |
|---|---|
| `run -- <cmd>` | Agent entrypoint: enter, watch, launch command, return command status. `--intent`/`--scope` declare why/what (advisory). |
| `sessions` | List sibling sessions live in this repo (read-only awareness); `--json` for machine output; `--prune` removes closed/stale sessions past the 7-day retention. |
| `intent "<text>"` | Wrapped agent declares/updates its own session's intent (and `--scope` paths) via the `TOIMIJA_SESSION` handshake. Advisory, never a lock. |
| `enter` | Refresh and print the current packet as agent preflight. |
| `refresh` | Recompute `current.md`/`current.json`. |
| `current --write` | Joskus-facing alias for synchronous packet refresh. |
| `current --verify --json` | Joskus-facing structured verification command. |
| `watch` | Keep `current.md`/`current.json` fresh on Git/filesystem changes. |
| `context` | Print the context packet without wrapping an agent. |
| `verify` | Check that `current.json` still matches the live repo; exits non-zero when not verified. `--json` emits structured status. |
| `snapshot` | Materialize the working tree as an immutable git tree object (content addressed at the current token), so a reader can read files *at a version*. `--lease`/`--release <id>`/`--list` pin a snapshot (a git ref) so `git gc` retains it while held. Refuses in an unsafe git state. |
| `validate --plan` | Print the validation plan; never runs validation. |
| `doctor` | Check Toimija setup (`--strict` gates on config/doc drift). |
| `landed` | Report whether a branch's work is present in a base ref, by content. |
| `init` | Create `.toimija/repo.yaml` if missing. |
| `snippet preflight\|script` | Print suggested preflight text / entry script; writes no files. |

`--for-diff` and `--budget` tune the packet; `--json` switches the print commands
(`context`, `enter`, `refresh`) to JSON. Global options: `-v/--verbose`,
`-q/--quiet`, `--log-format text|json`, `--no-color`. Logs go to stderr; command
output goes to stdout.

## Boundaries / non-goals

The boundaries are the feature: Toimija stays fast, predictable, and safe to
wrap around anything because of what it refuses to do. They are structural, not
aspirational — `doctor --strict` fails the build when the config and the agent
docs drift apart on them.

Toimija does not:

- run validation commands (`validate --plan` only prints them)
- auto-commit or auto-stash
- fetch from the network
- run as a hidden daemon (`watch`/`run` are foreground-only and explicitly invoked)
- own `AGENTS.md` / `CLAUDE.md` by default
- parse code with Tree-sitter
- use LSP / SCIP / Kythe / MCP in the current workflow

Default posture is advisory: it warns and explains; it does not block humans. The
one opt-in gate is `doctor --strict`.

## Ownership: `.toimija/` is Toimija's, root files are yours

Toimija should not own agent policy; it should make agent work cheaper by keeping
a live, repo-aware context layer an agent asks for before it works.

```text
.toimija/repo.yaml      owned by you        durable repo meaning (boundary + validation hints)
.toimija/current.md     owned by Toimija    repo-level live packet / latest summary (regenerated, git-ignored)
.toimija/current.json   owned by Toimija    repo-level machine-readable packet (regenerated, git-ignored)
.toimija/sessions/      owned by Toimija    per-session live packets + markers from `run` (regenerated, git-ignored)
AGENTS.md               owned by you        agent policy — Toimija only suggests text
CLAUDE.md               owned by you        agent policy — Toimija only suggests text
scripts/                owned by you        e.g. agent-enter — Toimija only suggests text
```

`toimija init` writes `.toimija/repo.yaml` and nothing else. It does **not**
create `AGENTS.md` or `CLAUDE.md` — those are human-owned policy. On a fresh
config it prints a **Next steps** line pointing at the identity fields you still
need to fill, and at `toimija snippet preflight` to seed your agent docs.

## Git-derived relevance

Toimija does not only report Git state. It also uses bounded Git history to help
an agent decide what to read first and what to ignore:

- changed areas since merge-base, and areas untouched by the branch (`Scope`)
- changed files ranked by likely relevance with a short reason (`Read order`);
  source churn corroborated as dependency-bump fallout (a changed manifest whose
  ecosystem lockfile changed in the same diff, e.g. `Cargo.toml` + `Cargo.lock`)
  is tagged `mechanical ripple` and ranked dead last — read the intent, skip the
  ripple unless it fails to compile. Conservative by design: a manifest or
  lockfile edit alone tags nothing, and files the branch itself added are always
  authored intent, never ripple.
- companion files that are missing from the current diff (`Coupling`)
- files with high churn, young surface, or stable/load-bearing history, plus a
  concrete action hint (`Risk`)

> This is not semantic code understanding. It is Git-derived work relevance.

> Tree-sitter can help after you know where to look. Toimija uses Git to decide where looking is worth it.

These hints are advisory. They come from `git ls-tree`, `git log`, `git diff`, and language adapter path heuristics (no source parsing, no network, no index), and every hint carries an honest source label. History mining is bounded: the last 200 non-merge commits, starting from the merge-base so the branch does not vote on its own relevance. Historical coupling needs a missing partner to co-change at least twice and in at least 60% of the changed file's edits; existing companion tests are surfaced through path heuristics. "High churn" means at least 8 commits touched the file in that window. Sections are capped (read order grouped, coupling and risk at 5). On a shallow clone or a repo with no history these sections are omitted and a `Relevance history unavailable` warning is emitted.

## Validation

`toimija validate --plan` prints commands. It never runs them.

The plan has tiers — `Focused` (derived from the changed surface), `Fast`,
`Strict`, `Full`, and `Privileged` — drawn from `.toimija/repo.yaml`'s
`validation` section, or inferred from the repo kind (Rust/Elixir) when the config
leaves them empty. The agent runs the commands itself and reports the exact output.

The `Focused (from changed surface)` tier is derived, not configured: for a Rust repo it maps changed `.rs` files to their owning crate (the nearest enclosing `Cargo.toml`) and emits `cargo test -p <crate>` for a fast first pass, deduped per crate and rendered above the authoritative `Fast` tier. For an Elixir umbrella it maps changed `.ex`/`.exs` files to their owning app (`apps/<name>/`) and emits `mix test apps/<name>/test`; a single-app mix repo gets no focused tier, since `mix test` already is the whole run. It is advisory and additive — never a substitute for the full run. When only manifests or docs change, or a change spans more units than the cap, the tier is omitted and the whole-repo command stands.

## `doctor --strict` — the drift guard

Plain `toimija doctor` is advisory: it reports setup gaps as warnings and exits
`0`. `toimija doctor --strict` escalates every actionable warning to a failure and
exits non-zero, so a CI step or pre-commit hook can block drift. It is a lexical
drift guard — no AI, no semantic comparison. It checks:

- `.toimija/repo.yaml` exists and parses
- `repo.name`, `repo.role`, and `boundaries` are non-empty
- a validation plan exists (configured or inferable from the repo kind)
- every `context.important_docs` path exists on disk
- an agent doc still routes work through the `toimija enter` preflight
- each boundary sentence still appears in the agent docs
  (`AGENTS.md` / `CLAUDE.md` / `important_docs`), matched case- and
  whitespace-insensitively so re-wrapping a line does not trip the gate

```bash
toimija doctor --strict
```

## Did the work land? (`toimija landed`)

"Did my branch land?" is a **content** question, not an ancestry question. Under
squash merges this matters: a squash-merge synthesizes a fresh commit with no edge
back to your branch tip, so `git merge-base --is-ancestor <tip> origin/main`
reports `false` even when every line landed. Patch-id checks (`git cherry`) fail
too, because squash fuses commits. Only tree/content comparison survives both.

`toimija landed` answers per touched path: is the branch's *own change* to it
already present in the base ref (default `origin/main`)? For each path it asks git
whether the branch's change applies cleanly forward onto base (so it is still
absent → not reflected), reverse-applies (so it is exactly present → reflected), or
neither — meaning base has diverged on that file. The diverged case is reported as
**ambiguous**, never silently as landed: a false "landed" is the dangerous
direction for branch cleanup. A line-containment heuristic (are the lines the
branch added present, and the lines it removed gone?) rides along as *evidence* on
the ambiguous entry, but does not decide the verdict.

```bash
git fetch                         # re-validate against live state first
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

Status is `landed` (every touched path's change is proven present), `not landed`
(all proven absent), `partially landed` (a clean split of reflected and not),
`ambiguous` (base diverged on at least one path, so landing could not be proven —
the paths and their heuristic evidence are listed), or `unknown` (no merge-base —
unrelated histories or an unfetched base). The check runs against a scratch index
seeded from the base, so it never touches your index or working tree. Useful for
stacked PRs: after a parent merges via squash, check a child with `toimija landed`
rather than trusting ancestry.

## Example context output

`toimija context` (and the `.toimija/current.md` written by `refresh`/`enter`)
renders like this:

```text
# Toimija Context

## Repo
- name: toimija
- role: internal CLI that compiles local Git context for coding agents
- not: platform, dashboard, dispatcher
- kind: Rust

## Git
- branch: feat/git-ceramics
- HEAD: abc123
- detached: false
- upstream: unavailable
- merge-base: origin/main @ def456
- dirty: true
- staged: 0
- unstaged: 2
- untracked: 1
- conflicts: 0

## Git state
Toimija starts from Git's state machine: HEAD, index, working tree, branch, and merge-base.

## Branch intent
- compared against: origin/main
- merge-base: def456
- files changed since merge-base: 3

## Recent commits
- abc123 feat: make Git state explicit
- def456 feat: add config-driven context rules

## Scope
- source: branch diff since merge-base
- in play: README.md, src, tests
- out of scope this branch: .github, fixtures, xtask

## Read order
1. README.md — changed docs
2. tests/git_state.rs — tests
3. src/git.rs, src/render.rs — source files

## Read first
- README.md (important doc)

## Validation plan
- source: inferred
### Focused (from changed surface)
- derived from the changed surface — a fast first pass, not a substitute for the full run
- cargo test -p toimija
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
- validation inferred from Rust repo shape, not proven by Sykli

## Warnings
- No upstream detected. Branch divergence is unavailable.
- Untracked files exist. They will not be committed unless added.
- Working tree has unstaged changes.

## Coupling
- src/git.rs changed, but tests/git_state.rs was not — companion test exists but is not in this diff — confirm intentional

## Risk
- src/render.rs — high churn — 9 commits in the last 200 commits; hot file — keep the change small and well-tested
- tests/git_state.rs — added on this branch — new surface; new surface without a changed companion test — consider adding coverage

## Safe to ignore
- target/

## Working tree
### Staged
- none
### Unstaged
- modified src/git.rs
- modified src/render.rs
### Untracked
- tests/git_state.rs
### Conflicts
- none

## Changed surface
- source: branch diff since merge-base
- 4 files — see Read order
```

## Config

`.toimija/repo.yaml` is the durable repo manifest. Build and vendor directories (`target`, `_build`, `deps`, `node_modules`) are filtered from the changed surface automatically, matched as a path segment anywhere in the tree (so nested `test_projects/app/target/` is caught too). This is applied live each run — not written into `repo.yaml` — so it tracks the repo as its kind changes. `context.safe_to_ignore` is for your own additional rules on top.

```yaml
repo:
  name: toimija
  role: internal CLI that compiles local Git context for coding agents
  not:
    - platform
    - dashboard

context:
  default_budget: 8000
  important_docs:
    - README.md
    - AGENTS.md
  read_first:
    - docs/architecture.md
  safe_to_ignore:
    - target/
    - _build/
    - deps/

validation:
  fast: []
  strict: []
  full: []
  privileged: []

boundaries:
  - Keep the core as a single Rust crate.
  - Do not add MCP, LSP, SCIP, Tree-sitter, network calls, or background daemons yet.
```

`context.read_first` adds repo-specific files to the Read first section when they exist.
`context.safe_to_ignore` lists directory rules that are omitted from the changed surface and read-first hints. Each rule is matched as a path segment (or contiguous segment sequence) anywhere in the tree, not just at the repo root.

## Build from source & development

```bash
cargo build
cargo run -- context        # run locally
cargo install --path .      # install the binary from this checkout
```

Release artifacts are built by GitHub Actions when a `v*` tag is pushed. The
workflow runs on False Systems ARC/Hetzner ARM64 runners and uploads a Linux
aarch64 binary as a workflow artifact.

Run the standard gate before claiming completion:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

### Pre-commit hook

A `xtask` crate provides a git pre-commit hook via
[`devx-pre-commit`](https://docs.rs/devx-pre-commit). It is dev-only and kept out
of the shipped binary by `default-members = ["."]` in the workspace, so bare
`cargo build`/`test`/`clippy` and the release build never compile it. Install the
hook once per clone:

```bash
cargo xtask install-pre-commit-hook
```

The hook formats the staged Rust crates, re-stages the result, then runs
`cargo clippy --all-targets -- -D warnings` followed by `cargo test`, blocking the
commit if either fails — the same gates CI enforces. Re-run the command after
editing `xtask`.

## Current limitations

- Git state is collected through the Git CLI, not Git internals.
- Merge-base is attempted against the selected base candidate.
- Validation plans are rendered only; commands are not executed.
- Rust and Elixir detection use simple marker files.
- Context budgets are approximate character caps, not token counts.
- Impact analysis is not yet available.
- Upstream and merge-base may be unavailable in shallow, unborn, detached, or local-only repos.

## Output stability

Context packets use explicit ordering rules so agents can diff and consume them predictably:

- the changed surface unions working-tree changes (staged, unstaged, untracked) with files changed by commits on this branch since the merge-base, so a clean tree on a feature branch still reports its diff
- changed files are sorted by path and deduplicated in the changed surface
- read-first entries are grouped as changed files, matching-test hints, then configured docs
- warnings are sorted and deduplicated
- Markdown output uses a fixed heading hierarchy: top-level sections are `##`, sub-blocks (e.g. Staged/Unstaged under Working tree) are `###`, and a blank line precedes every top-level section
- high-value Markdown sections (`Scope`, `Read order`, `Read first`, `Validation plan`, `Warnings`, `Coupling`, `Risk`) render before the raw working-tree and changed-surface lists
- Markdown list sections are capped at 25 entries with an honest `... and N more (truncated)` line, so a large working tree cannot hide later sections
- empty values are rendered honestly: `none` for an empty-but-known list, `unavailable` for a value Git could not determine
- Markdown and pretty JSON output have exact regression tests

`toimija context --for-diff` adds a diff-focused Markdown section that separates committed files changed since the selected merge-base, staged, unstaged, untracked, and conflicted files.

## Read-first heuristics

Read-first hints are bounded and path-derived. Toimija does not scan the whole repo or parse source code.

Rust:

- changed Rust files are included directly
- `src/foo.rs` may point to `tests/foo.rs`, `tests/foo_test.rs`, `src/tests.rs`, or `src/foo_test.rs` if present
- `src/foo/bar.rs` may point to `tests/foo/bar.rs` and nearby sibling test files if present
- `src/foo/mod.rs` may point to `tests/foo.rs` or `tests/foo_test.rs` if present

Elixir:

- changed Elixir files are included directly
- `lib/foo/bar.ex` may point to `test/foo/bar_test.exs` if present
- `mix.exs` may point to `test/test_helper.exs` if present

## Future drawer

These are intentionally not implemented:

- MCP server
- Tree-sitter, LSP, SCIP, Kythe
- Sykli, Vaisto, or Kerto integration
- checkpoint and worktree management
- cross-repo / multi-repo coordination
- dynamic plugins
- graph database
- background daemons
