# Proposal 0004 — Cross-machine session coordination (heartbeat liveness + travelling handoff)

**Status:** DRAFT for review 2026-07-03 (Yair, design session). Scoped
deliberately narrow: extend sibling awareness and handoff across machines
**without** exploding the working tree and **without** toimija ever touching
the network. Builds on `0001-sessions-awareness-handoff.md` and
`0002-heartbeat-staleness.md`.

**Not in this proposal:** the ref-namespace *per-session-file* placement from
the design sketch (rejected — see §5), a `toimija sync` network command
(deferred — see §6), task dispatch or assignment (out of scope, permanently —
mirrors AGENTS.md).

---

## 1. The problem

Two or more computers work the same logical repo — your laptop, a CI runner, a
teammate's machine, a cloud sandbox — each with its own clone and its own
checkout. Today `toimija sessions` only sees siblings that share one `.git`
(one worktree tree). Across machines it sees nothing, so two agents on two
computers editing the same branch are invisible to each other, and an agent
that goes offline on one machine cannot hand its work to an agent on another.

The naive fix — write a marker file per remote session and commit them — would
grow the working tree without bound and litter `git status`. **The design
constraint is: coordination across machines must add zero files to the working
tree, no matter how many sessions or computers exist.**

## 2. The reframe: toimija stays local; Git is already the wire

Toimija does not become distributed. It stays a local, synchronous, no-network
tool. Three facts make that sufficient:

- **The v2 consistency token is content-based and machine-independent** — no
  clock, no hostname, no pid. Machine A and machine B at the same HEAD with the
  same tree compute the *identical* token. "Same repo state" is therefore a
  *computable fact*, not a coordination protocol.
- **Git already syncs objects between machines** on the `fetch`/`push` the team
  already runs. Anything toimija stores as a Git object under a ref travels for
  free, on the cadence that already exists.
- **Heartbeat staleness (0002) is already host-independent.** A pid means
  nothing across machines; a stale `last_seen_at` means the same thing
  everywhere. The cross-machine liveness primitive already shipped.

So the whole proposal is: **move the two things worth sharing into one Git ref,
keep them out of the working tree, and read them locally.**

## 3. What ships (moves 2 and 3 only)

### D-1 — One ref, not many files. `refs/toimija/coord`

All cross-machine coordination lives under a **single ref** whose tree is a
handful of small objects, never checked out into the working directory. `ls`,
`git status`, and the working tree are byte-identical whether zero or fifty
remote sessions exist. Concretely:

- `refs/toimija/coord` points at a tree with exactly two entries:
  - `sessions.jsonl` — one line per *live-or-recently-closed* session (see D-3
    retention), each carrying `{id, host, pid, branch, head, token, intent,
    scope, status, last_seen_at, closed_at}`.
  - `handoff.jsonl` — the append-only handoff log (D-4).
- Writes are last-writer-wins on the ref, reconciled by **session-id key**: a
  writer reads the current ref, replaces only its own session's line (or
  appends a handoff record), writes the new tree, updates the ref. Because each
  session only ever rewrites *its own* keyed line, two machines editing
  different sessions do not lose each other's data even under a naive
  last-writer-wins (the loser re-applies on its next heartbeat). Same-session
  double-write is impossible — a session runs on one machine.
- Nothing here is in the worktree, so none of it interacts with the consistency
  fingerprint, `.toimija/` blessing, or `git status`. It is pure Git-object
  side-channel.

### D-2 — Liveness by heartbeat, pid as a local hint (move 2)

Extend the session marker with `host` (a stable machine id — hostname plus a
boot-or-clone salt) alongside the existing `pid`. Liveness resolves in two tiers:

- **Same host** (marker's `host` == this machine): keep the cheap local check —
  `pid` alive AND heartbeat fresh, exactly as 0002 does today.
- **Different host:** pid is unknowable, so liveness is heartbeat-only —
  `last_seen_at` within the staleness window ⇒ active, else stale. This is
  already the 0002 semantics; the only change is *not* consulting pid for a
  foreign host (a recycled pid on another machine must never read as alive).

No new files. One field on an existing struct, one branch in the liveness
function.

### D-3 — `toimija sessions` gains a cross-machine view

When `refs/toimija/coord` exists, `sessions` merges the local markers with the
ref's `sessions.jsonl`, dedups by id, and shows siblings across machines:

```
laptop-a1b2   feat/parser   sha256:9e24…  active   intent: rewrite the lexer
ci-run-77     feat/parser   sha256:0f3c…  active   intent: run the soak suite
cloud-3       feat/render   sha256:aa10…  stale(4h) intent: —
```

The token column is the payoff: two sessions on the same branch with the *same*
token are provably at the same state; different tokens flag divergence to
reconcile. This is the "who is doing what right now" AGENTS.md already assigns
to issues and draft PRs — toimija surfaces it faster, without claiming to
*coordinate* anyone. Retention matches `sessions --prune`: closed sessions past
the 7-day window are dropped from the ref on the next write, so `sessions.jsonl`
stays bounded — it never grows without limit.

### D-4 — Handoff travels (move 3)

`handoff-record.v1` records append to the ref's `handoff.jsonl` instead of (or
in addition to) the per-session `.toimija/sessions/<id>/handoff.jsonl`. An agent
going offline on the CI runner writes a handoff; the next `fetch` carries it;
`toimija handoff --show` on your laptop reads it. This is the actual
remote-handoff use case and it is ~90% built — the record shape, the writer, the
reader all exist; only the *placement* (ref-side, one shared log) changes.
Append-only + retention keeps it bounded; one file, not one-per-session.

## 4. What explicitly does not change

- Toimija makes **no network call.** It reads and writes local Git objects
  (`git update-ref`, `git cat-file`, `git hash-object` — all local plumbing).
  The `fetch`/`push` that moves the ref between machines is run by humans or CI
  on their existing cadence, exactly as they already push branches. This
  preserves the AGENTS.md / `repo.yaml` no-network boundary **completely** — no
  proposal is needed to relax it, because it is not relaxed.
- No daemon, no background process, no polling. Every read and write happens
  during an explicitly-invoked `sessions` / `handoff` / `intent` / `refresh`
  command, synchronously.
- No dispatch, no assignment, no orchestration. The ref is a *shared bulletin
  board that Git happens to sync*, not a scheduler. The human remains the
  orchestrator.
- The working tree and the consistency fingerprint are untouched. Coordination
  state is Git-object-only.

## 5. Rejected: per-session marker files under a ref namespace

The original sketch proposed `refs/toimija/sessions/<id>` — one ref (and one
object) per session. Rejected: it re-introduces the file-explosion the whole
constraint is against (one object per session per machine, unbounded), and ref
namespaces are awkward to fetch as a set. **One ref, two consolidated logs,
keyed by session id, pruned by retention** is the minimal-footprint form and the
one that ships.

## 6. Deferred: `toimija sync`

A convenience `toimija sync` that wraps `git fetch`/`git push` of
`refs/toimija/coord` would be one explicit synchronous command — arguably within
the "no daemon, no background" spirit — but it is the *first* time toimija would
touch the network, so it is a genuine boundary decision, not a mechanical one.
**Not in scope here.** If the manual "let Git carry it" flow proves too fiddly
in practice, `sync` gets its own proposal and a human scope call, with the
`repo.yaml` boundary + `doctor --strict` drift guard updated in lockstep.

## 7. Open questions for the human

- **`host` identity:** hostname is not stable (containers, renames). Prefer a
  per-clone salt written once to `.git/toimija-host` (in `.git`, never the
  worktree) and combined with hostname? Or accept hostname and live with
  occasional aliasing?
- **Ref-write concurrency on one machine:** two local sessions writing the ref
  at once need a local lock (a lockfile under `.git/`), same pattern as Git's
  own `index.lock`. Confirm that is acceptable.
- **Fetch cadence:** the cross-machine view is only as fresh as the last
  `fetch`. Is "as fresh as your last pull" the right honesty contract to
  document, or does that argue for §6's `sync` sooner than later?

## 8. Boundary update (if accepted)

This proposal adds no boundary changes — it operates entirely within the
existing "synchronous reads and writes during an explicitly-invoked command, no
network, no daemon" clause of `.toimija/repo.yaml` and AGENTS.md. The only doc
touch is to note in AGENTS.md's "Working with other agents" section that sibling
awareness and handoff now span machines via `refs/toimija/coord`, carried by the
team's existing Git sync. `doctor --strict` stays green.

---

**One-line summary:** siblings and handoff go cross-machine by living in a single
Git ref that Git already syncs, with heartbeat (not pid) as the universal
liveness signal — zero new working-tree files, zero network calls by toimija.
