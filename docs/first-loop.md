# Your first loop

This page walks one small change through the whole loop: start an agent, prove
the work, review it, land it, deploy it, and see what it used. It is for a
person who has just installed the four tools (toimija, sykli, ohjaa, vessa)
and has an agent CLI such as `claude` or `codex`.

Every command below was run in a new throwaway repository with a local bare
repository as its remote. The output is real and trimmed; long paths and
receipt names are shortened with `…`, and the repository sits at `/work/demo`.

## Once per machine

`ohjaa apply` writes what agents are told into each agent's own instruction
file, adds one line to `~/.zshrc`, and adds two ignore rules to Git's global
excludes. Look first, with `--dry-run`:

```console
$ ohjaa apply --dry-run
ohjaa apply: would write ~/.config/ohjaa/roots with /work, where ohjaa looks for repositories
ohjaa apply: would add .sykli/ .ohjaa/ to ~/.config/git/ignore (git's global excludes), in a block marked "ohjaa apply"
ohjaa apply: would change ~/.zshrc:
  + # What needs your attention, as a terminal opens (written by `ohjaa apply`).
  + [[ -x '~/.cargo/bin/ohjaa' ]] && eval "$('~/.cargo/bin/ohjaa' shell-init zsh)"
ohjaa apply: dry run; nothing was changed
```

Then apply. `ohjaa apply --undo` takes it all back.

```console
$ ohjaa apply
ohjaa apply: wrote ~/.config/ohjaa/roots with /work, where ohjaa looks for repositories
ohjaa apply: added .sykli/ .ohjaa/ to ~/.config/git/ignore (git's global excludes), in a block marked "ohjaa apply"
ohjaa apply: changed ~/.zshrc:
  …
ohjaa apply: committed in ~/.config/ohjaa: apply: 3 changes
```

On a machine with no knowledge yet, that is all it writes. Knowledge arrives
later, when a person accepts a proposal.

## Once per repository

The repository here is a small Rust crate with one function and one test, and
`origin` set.

```console
$ toimija init
Toimija init

Created:
- /work/demo/.toimija
- /work/demo/.toimija/repo.yaml
- /work/demo/.toimija/.gitignore

Next steps:
- Fill role, not, and boundaries in .toimija/repo.yaml so the packet can orient an agent.
- Tell agents in your AGENTS.md/CLAUDE.md to run `toimija enter` first, or launch them with `toimija run`.

$ sykli init
wrote sykli.json (3 tasks, 4 inputs) and ./sykli.lock

$ sykli hook install
installed: /work/demo/.git/hooks/pre-commit
installed: /work/demo/.git/hooks/pre-push
```

`sykli init` reads the repository and declares its checks in `sykli.json`:
here `cargo fmt --check`, `cargo clippy` and `cargo test`. The hooks run those
checks on every commit and refuse a push to the main branch without proof.

Commit the setup. The pre-commit hook runs the checks on what is staged:

```console
$ git add -A && git commit -m "Set up toimija and sykli"
sykli hook pre-commit: the staged tree passed
[main e5a73d4] Set up toimija and sykli
```

## Optionally, write the spec first

A spec is a short decision file the session starts from.

```console
$ git checkout -b greet-politely
$ toimija spec new "Greet politely"
docs/adr/0001-greet-politely.md
```

The file has a title, `Status: proposed`, and three empty headings: Context,
Decision, Consequences. Fill it in, or let the agent do it.

## Start the agent

```console
$ toimija run --spec docs/adr/0001-greet-politely.md claude
toimija run: watcher armed (native)
toimija run: delivery: launch prompt with the packet inlined for `claude`
toimija run: launching `claude` (session 1794349981-90012, watching for changes)
```

The agent starts with the packet in its prompt: the branch, what changed, the
spec first under Read first, and how to validate. `codex` works the same way.
Add `--intent "<why>"` so that other sessions see why this one is here.

From another terminal, while it works:

```console
$ toimija sessions
LIVE
  claude  1794349981-90012
    branch: greet-politely
    worktree: /work/demo
    intent: unknown
    spec: docs/adr/0001-greet-politely.md
```

The agent makes its change and commits. The hook runs the checks on that
commit too. When the agent exits, toimija says what the session did:

```console
toimija run: session summary
  branch:    greet-politely
  HEAD:      e5a73d4 → 3588810 (moved)
  worktree:  0 staged, 0 unstaged, 0 untracked
  scope:     2 file(s) changed on this branch since merge-base
```

## Prove it

```console
$ sykli run
passed: clippy
passed: test
receipt: /work/demo/.sykli/receipts/rcpt_515d4123….json
```

The receipt is the proof: these checks passed on exactly this tree.

## Review, then land

`sykli review` starts a fresh agent that reads the change with reading tools
only. It takes about a minute and costs tokens.

```console
$ sykli review
review: fine (0 blocking, 0 to look at) by codex, author agent unknown
```

The answer is `fine`, `look` or `blocking`. A blocking review stops the
landing. Fix what it found and commit, or a person overrules it at a terminal
with `sykli review --overrule "<why>"`. An agent cannot overrule. A reviewer
that is missing or gives no verdict is no review, never a pass.

Landing pushes to the main branch, so it is a person's decision when to run
it:

```console
$ sykli land
land: 35888107dc5d is on origin's main: proven, review fine; the receipt and the review are shared as notes on its tree
```

`sykli land` refuses unless the commit is proven, reviewed and on top of the
current main.

## See where things stand

```console
$ toimija graph
* 3588810  greet-politely     proven    review: fine
* e5a73d4  origin/main, main  proven    review: none
```

That is the graph just before landing: one branch in progress, proven and
reviewed, above the main branch. After landing, the branch is part of main:

```console
$ toimija graph
* 3588810  origin/main, greet-politely  proven    review: fine
```

Leave a note for whoever works here next, a person or an agent:

```console
$ toimija handover --next "greeting is polite now; the README still says hello"
Handover written for the next agent in /work/demo: for the next agent
```

Every packet built in this working copy shows it under "Last handover".

## Deploy

A deployment is a second contract, `deploy.json`, whose tasks do the
deploying. This one only builds a release:

```json
{
  "schema": "sykli-contract.v1",
  "tasks": [
    {
      "name": "ship",
      "run": "cargo build --release",
      "inputs": ["Cargo.lock", "Cargo.toml", "src/lib.rs"]
    }
  ]
}
```

Pin it with `sykli lock deploy.json` and commit it with the setup. Then, for a
commit that is proven and landed:

```console
$ sykli deploy --remote origin
running: ship
passed: ship
receipt: /work/demo/.sykli/receipts/rcpt_b2df39db….json
deploy: 35888107dc5d deployed to default; recorded on the commit and shared with origin

$ sykli deployed
default: 35888107dc5d  Greet politely  2026-11-10 22:33:32 UTC
```

`sykli deploy` refuses a commit that is not proven or not in the main branch.
The record says the deploy contract passed for that commit, here, at that
time. It does not say what is running anywhere now.

## See what it used

`vessa usage --by-agent` sums the tokens each agent used, from the agents' own
session files. This output is from the sample sessions in vessa's `examples/`
folder, not from real work:

```console
$ vessa usage --by-agent
Deduplicated agent totals
claude
  anthropic/demo-model in=1206 out=1675 cache-read=3300 cache-write=180
codex
  openai/demo-codex in=800 out=200 cache-read=700 cache-write=0
```

Vessa counts tokens. It shows no prices.

## Most days

```bash
toimija run claude    # start an agent as a session
sykli run             # prove the commit
sykli review          # a fresh agent reads the change
sykli land            # put it on main, when you decide to
toimija graph         # where every branch stands
```
