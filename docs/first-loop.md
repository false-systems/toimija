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

Toimija needs no setup. Its own files under `.toimija/` (session markers, the
handover) are not for Git, so keep them out; `.toimija/repo.yaml`, written by
hand if you want one, says what the repository is for and is committed:

```console
$ printf '/.toimija/*\n!/.toimija/repo.yaml\n' >> .gitignore
$ mkdir -p .toimija && printf 'repo:\n  name: demo\n  role: a crate that greets\n' > .toimija/repo.yaml
```

```console
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

## Start the agent

```console
$ git checkout -b greet-politely
$ toimija run --intent "greet politely" claude
toimija run: delivery: launch prompt with the packet inlined for `claude`
toimija run: launching `claude` (session 1794349981-90012)
```

The agent starts with the packet in its prompt: the branch, what changed, what
to read first, and how to validate. `codex` works the same way. `--intent`
says why this session is here, so other sessions see it. While the agent
works, toimija starts no git process; the agent runs `toimija enter` when it
wants the state as it is now.

From another terminal, while it works:

```console
$ toimija sessions
LIVE
  claude  1794349981-90012
    branch: greet-politely
    worktree: /work/demo
    intent: greet politely
    scope: unknown
    started: 2m ago
    seen: 1m ago
```

The agent makes its change and commits. The hook runs the checks on that
commit too. When the agent exits, toimija says what the session did:

```console
toimija run: session summary
  branch:    greet-politely
  HEAD:      e5a73d4 → 3588810 (moved)
  worktree:  0 staged, 0 unstaged, 0 untracked
  intent:    "greet politely" (declared at launch)
  expansions: none declared
```

The session ends with the agent: `toimija sessions` no longer lists it.

## Prove it

```console
$ sykli run
passed: clippy
passed: test
receipt: /work/demo/.sykli/receipts/rcpt_515d4123….json
```

The receipt is the proof: these checks passed on exactly this tree.

## Review, then push

`sykli review` starts a fresh agent that reads the change with reading tools
only. It takes about a minute and costs tokens.

```console
$ sykli review
review: fine (0 blocking, 0 to look at) by codex, author agent unknown
```

The answer is `fine`, `look` or `blocking`. A blocking review stops the
push. Fix what it found and commit, or a person overrules it at a terminal
with `sykli review --overrule "<why>"`. An agent cannot overrule. A reviewer
that is missing or gives no verdict is no review, never a pass.

Landing is a plain push to main, so it is a person's decision when to do it.
The hook you installed lets it through only when the commit is proven,
reviewed against the main it lands on, and on top of it, and it shares both
records with the remote before the branch moves:

```console
$ git push origin HEAD:main
sykli hook pre-push: d01050abbea9 to refs/heads/main: .sykli/receipts/rcpt_0682dcdc….json is a passing run of its tree
sykli hook pre-push: d01050abbea9 is reviewed look against origin's main (4b8de89fe461)
sykli hook pre-push: shared refs/notes/sykli/review and refs/notes/sykli/receipt with origin before the branch moves
sykli hook pre-push: kept /work/demo/.git/sykli/receipts/rcpt_0682dcdc….json as evidence for d01050abbea9
To ../origin.git
   4b8de89..d01050a  HEAD -> main
```

Without a receipt or a review the push is refused, and the hook says which
command to run. `sykli status` shows every branch: proven or not, and how its
review went.

## See what is cooking

`toimija sessions` lists who is working in this repository, on which branch,
and why; run it in each repository you care about. With no agent at work:

```console
$ toimija sessions
No open sessions under .toimija/sessions/.
```

Leave a note for whoever works here next, a person or an agent:

```console
$ toimija handover --next "greeting is polite now; the README still says hello"
Handover written for the next agent in /work/demo: for the next agent
```

Every packet worked out in this working copy shows it under "Last handover".

## Deploy

A deployment is a second contract, `deploy.json`, whose tasks do the
deploying. `"reuse": false` makes a task run every time: a pass taken from the
cache would be a deployment that never happened. This one only builds a
release:

```json
{
  "schema": "sykli-contract.v1",
  "tasks": [
    {
      "name": "ship",
      "run": "cargo build --release",
      "inputs": ["Cargo.lock", "Cargo.toml", "src/lib.rs"],
      "reuse": false
    }
  ]
}
```

Pin it with `sykli lock deploy.json` and commit it with the setup. Then run it
on the commit you landed:

```console
$ sykli run deploy.json
run: 090102d4…
running: ship
passed: ship
receipt: /work/demo/.sykli/receipts/rcpt_8ddc0447….json
```

The receipt says the deploy contract passed for that commit, here, at that
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

To keep the thread from main back to what was asked, run `vessa hook install`
once per clone. Every commit an agent makes then carries an `Agent-Session`
line naming its session and how many messages a person typed in it, never the
text. `vessa messages --commit <commit>` shows those messages, on the machine
where the session ran.

## Most days

```bash
toimija run claude        # start an agent as a session
sykli run                 # prove the commit
sykli review              # a fresh agent reads the change
git push origin HEAD:main # put it on main, when you decide to
sykli status              # every branch: proven, reviewed
toimija sessions          # who is working where, and why
```
