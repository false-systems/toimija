<img src="docs/assets/toimija.svg" alt="toimija: Facts for agents. No decisions." width="100%">

# toimija

Toimija is a command-line tool with one job: it tells a coding agent where the
work stands in the Git repository it works in.

It is for a person who works with coding agents such as Claude Code, Codex, pi
or opencode, at the moment they start one in a repository and want it to know
where it is. It needs `git`, an agent CLI, and Rust for a source install.

> We do not ask the model to understand the repo first. We ask Git what state the repo is in.

A coding agent usually enters a repository by listing, searching and guessing.
That is slow, and every agent repeats it. Toimija asks Git instead and works
out a short packet: the branch, the changed files, what to read first, how to
validate, and the note the last agent left. It starts the agent with that
packet in its prompt, shows which other sessions are working in the same
repository and why, and carries a note from one agent to the next. The packet
is worked out each time it is asked for and never saved, so it cannot go
stale. In a folder that holds several repositories, it gives the agent the
facts of the folder instead: each repository, its branch, and who is working
where.

Git is not a save button here. Git is the substrate.
Git gives us the graph of what happened. Toimija turns that graph into operational context for agents.

## See it

<img src="docs/assets/see-it.svg" alt="One terminal runs an agent with toimija run. In another, toimija sessions lists the session as live with its branch and intent, and toimija handover leaves a note for the next agent." width="100%">

<details><summary>The same, as text</summary>

```console
# one terminal:  toimija run --intent "tidy the README" claude

$ toimija sessions
LIVE
  claude  1794350046-58479
    branch: tidy-readme
    intent: tidy the README

$ toimija handover --next "the intro is done; the examples still to check"
Handover written for the next agent in /work/demo: for the next agent
```

</details>

The packet an agent reads (shortened here):

```console
$ toimija enter
# Toimija Context
- compiled-from: 2 changed file(s), 3 recent commit(s) — reading this replaces that discovery

## Repo
- name: demo
- role: service that signs users in
- kind: Rust

## Last handover
- written: 2026-11-10 14:30 UTC on fix/login-redirect at 0cb97f9
- for the next agent: the redirect is fixed; tests still to write

## Git
- branch: fix/login-redirect
- merge-base: main @ 199d5873d4180dfb0c076ac555e1686e74920b7a
- dirty: true
- staged: 0
- unstaged: 1
- untracked: 0

## Read first
- README.md (important doc)
- .toimija/repo.yaml (changed)
- src/auth.rs (changed)
- tests/auth.rs (matching test heuristic)

## Validation plan
- source: inferred
### Fast
- cargo test --workspace
```

## Install

Toimija builds from source with cargo. The repository pins its Rust toolchain
(1.98) in `rust-toolchain.toml`, and `rustup` installs that version on the
first build. It also needs `git` on the path.

```bash
git clone https://github.com/false-systems/toimija.git
cd toimija
cargo install --path .
```

Toimija is not on crates.io yet. Once it is published, `cargo install toimija`
installs it with Rust 1.98 or newer; until then, use the commands above.

Toimija runs on macOS and Linux. On Windows, use WSL, where it is the Linux
build; native Windows is not supported.

## Use

Toimija needs no setup: it works in any Git repository. Five commands, in the
order you meet them:

```bash
toimija run claude         # start an agent as a session, with the packet in its prompt
toimija enter              # the packet now, for an agent started some other way
toimija sessions           # who is working in this repository, and why
toimija intent "fix the login redirect"          # a session says why it is here
toimija handover --next "tests still to write"   # a note for the next agent
```

`toimija run` takes any agent command: `toimija run codex`, or
`toimija run --intent "fix the login redirect" claude --model opus`.

Two optional files, both written by hand. `.toimija/repo.yaml` says what the
repository is for and what it must not become; it is committed. In a folder
of repositories, `.toimija/workspace.yaml` makes the folder a workspace. The
rest of `.toimija/` is toimija's own: keep it out of Git with `/.toimija/*`
and `!/.toimija/repo.yaml` in `.gitignore`.

## How it fits

Toimija is one of four small tools for working with coding agents. Each one does
one job and works alone.

<img src="docs/assets/loop.svg" alt="A piece of work moves through five steps: start, check, review, land, deploy. toimija starts it. sykli checks and reviews it, lets it land on main only with proof and a review, and runs the deploy. ohjaa holds what agents know, and vessa counts what agents used and points each commit back to its session." width="100%">

- **toimija** starts the session and tells the agent where the work stands.
- **sykli** decides whether a change may reach main: it runs the declared checks, keeps the proof and has a second agent review the change, and its hook refuses a push to main without both. Deploys run through it too.
- **ohjaa** holds what agents are told, and writes it into each agent's own instruction file. A person decides it.
- **vessa** counts what agents used, and points each commit back to the agent sessions behind it.

[Your first loop](docs/first-loop.md) walks one small change through all four.

## What it does not do

- It does not decide. It assigns no work, judges no result and closes nothing.
- It runs no checks. The packet names the validation commands and toimija
  never runs them.
- It does not coordinate agents. Overlap between sessions is a warning, never
  a lock.
- It keeps no copy of the packet. Ask again, with `toimija enter`, to see the
  state as it is now.
- It makes no network calls and leaves no process running after a command
  ends. While an agent runs under `toimija run`, it starts no git process.
- It does not parse source code or keep an index. What to read first comes
  from file paths.

## More

- [Your first loop](docs/first-loop.md): one small change, from a new repository to a deployment.
- [Reference](docs/reference.md): every command, the packet, sessions, workspaces and configuration.
- [Agent guide](AGENTS.md): for agents that work on toimija itself.
- [Licence](LICENSE): MIT.

<p align="center"><sub>Part of <a href="https://false-systems.com">False Systems</a> · computing as craft</sub></p>
