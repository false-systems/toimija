# Proposal 0010 — Teamwork: the rest of the team as a fact in your packet

**Status:** DRAFT for review 2026-07-26 (Yair, design session).

**Supersedes:** `0004-cross-machine-coordination.md` (parked). 0004 moved
coordination state into a shared ref (`refs/toimija/coord`) — a second
database with last-writer-wins races, retention logic, and declared state
that can silently go stale. This proposal replaces it with zero new shared
state: everything is derived from what git already syncs.

**Not in this proposal, permanently:** presence ("Dima is online"), locks,
claims, task dispatch, toimija-to-toimija messaging, any shared coordination
ref. See §5.

---

## 1. What teamwork means for toimija

Two environments — each a human plus their agents, each with its own clone —
work the same repo. They share exactly one medium: **the remote**. Everything
env A can know about env B arrives via fetch; everything it can say leaves
via push. Git's remote model is already a distributed-systems solution
(content-addressed state, explicit sync points, computable convergence); any
side channel toimija adds on top reintroduces the hard problems git solved —
races, staleness, split-brain — without git's guarantees.

So: **teamwork, for toimija, is two independently-evolving worksets over one
shared history, and toimija's job is to make the *other* workset legible
inside *this* environment's packet.** Legible, not managed. Each env runs its
own toimija; each compiles team facts deterministically from its locally
fetched origin and states the age of that observation. Collaboration without
coordination machinery.

One-liner: *toimija makes the rest of the team a fact in your packet — never
a party to your session.*

The admission test for any team signal: **can it be stale without confessing
it?** "Tip committed 12m ago; fetched 2m ago" carries its own staleness — the
timestamps are the confession. "Online: yes" decays silently into a lie.
Toimija holds only the first kind. Evidence over declaration, always.

## 2. The scenario this must serve

Yair (laptop, agents) and Dima (another machine, agents), same origin.

1. **Entering.** Yair starts a session touching `src/context.rs`. His entry
   packet already says: `src/context.rs — your workset ∩ origin/fix/hints
   (Dima <dima@example.com>, inferred; tip committed 34m ago; fetched 2m
   ago)`. The collision is visible at the cheapest point to avoid it: before
   work starts.
2. **Working.** Both keep editing. After a fetch, Yair's refresh upgrades the
   hint: `merge dry-run CONFLICTS [git merge-tree; fetched 2m ago]`. The
   merge conflict is known days before any PR would surface it. Dima learns
   the mirror fact on their next fetch — advisory, eventually consistent.
3. **Leaving.** Dima stops mid-task: `toimija handoff --print` renders a
   trailer for the human or agent to include in the final commit, then they
   push. Yair switches to the branch; `toimija enter` compiles arrival context
   from what the branch itself carries — changed surface, exit HEAD, the
   handoff's next-step text, and any declared env label.

Humans still coordinate *decisions* (who yields, who reviews) through GitHub
and conversation, per AGENTS.md. The agents on both sides never talk to each
other at all: teamwork for them is passive — better context, not a protocol.

## 3. What ships — four small layers, review between each

### L0 — Environment identity (the flag)

- Minted once per clone into the **common Git directory** (`.git/toimija-id`
  in an ordinary clone; resolved with `git rev-parse --git-common-dir`, never
  the worktree, never committed): `{id: <short random>, label:
  <hostname-derived, editable>}`. Linked worktrees share that common
  directory, so all worktrees of a clone share one identity — matching what
  "environment" means.
- **Inferred tier (free):** committer name/email on pushed branches identifies
  the author account, not a particular clone. Rendered as committer
  attribution. Confidence: medium.
- **Declared tier (opt-in):** commits made under a toimija session get a
  `Toimija-Env: <id>/<label>` trailer, stamped by a future
  `prepare-commit-msg` hook if Q2 opts into it. Toimija itself never commits.
  The label rides inside the pushed object, so it cannot be fresher or staler
  than the work it labels. Rendered as env attribution. Confidence: high.
- No registry anywhere. A committed `.toimija/envs.yaml` is the trap
  version: a second database and a claims table at once.

### L1 — Entering: team overlap in the packet

A new relevance analyzer — same pipeline, rendering, and advisory framing as
the existing Coupling and Risk hints. During `enter`/`refresh`/`context`:

- For each `refs/remotes/origin/*` branch ahead of main: diff names vs the
  common merge-base, intersect with this session's workset.
- Emit a `## Team overlap` packet section: file, branch, attribution (committer
  at the inferred tier, env at the declared tier), tip-commit age, and a
  freshness stamp. `fetched 14m ago` is shown only when that branch and tip
  are present in the current `FETCH_HEAD`; otherwise the line says `fetch age
  unknown`. The staleness of the knowledge is part of the knowledge.
- Branches that do not intersect the workset are not shown — a stale branch
  that touches nothing of yours is noise by definition.

### L2 — Working: the proof tier

For branches that intersect, run `git merge-tree --write-tree` (in-memory
dry-run merge, no worktree touch, pure local plumbing) and upgrade the hint
from "you might collide" to "your branches **conflict as of now**". Ranked
above name-intersection hints.

### L3 — Leaving: handoff travels in the branch

The existing `handoff-record.v1` gains a travelling form:
`toimija handoff --print` renders the record as a structured trailer for the
human or agent to include in the next commit or a PR body. Toimija never
creates or amends a commit. Arrival is the existing `enter` on that branch,
extended to surface a handoff trailer found at the tip. No side log, no shared
ref — continuity crosses the machine boundary inside the branch, so it cannot
drift from the code it describes.

## 4. What this explicitly does not do

- **No presence.** "Is Dima online?" is unanswerable without heartbeats, and
  heartbeats lie by default (crashed env, suspended laptop — 0002/0008
  fought this on *one* machine; cross-machine with fetch lag it degrades to
  actively misleading). The truthful ceiling is tip-commit recency plus a
  branch-specific local fetch observation, and that is what ships. "Will Dima
  answer now?" belongs to Slack; "is this claimed?" belongs to GitHub issues
  and draft PRs per AGENTS.md.
- **No unpushed visibility.** Work not pushed is invisible until pushed —
  git's native semantics, worn openly via the freshness stamp. The team norm
  this leans on (small branches, pushed early) is already AGENTS.md law.
- **No messaging, no locks, no claims, no dispatch.** The human remains the
  orchestrator.
- **No network calls by toimija.** Reads are local (`refs/remotes/*`,
  FETCH_HEAD, merge-tree). The fetch/push that moves facts between machines
  is run by humans, agents, or CI on their existing cadence.

## 5. Boundary impact

None. Every layer is synchronous reads/writes during an explicitly-invoked
command, no daemon, no network — inside the existing `repo.yaml` clause.
`doctor --strict` stays green with no edits.

Deferred behind an explicit human boundary call (its own proposal, if ever):
a `gh` plugin (plugin area, Teko pattern) observing draft PRs and assignees
to surface *claims* in the packet — `gh` talks to the network, so it is a
genuine boundary decision, not a mechanical addition.

## 6. Open questions for the human

- **Q1 — identity granularity.** Per-clone (default above) vs per-human
  (follow git config; envs stay anonymous) vs per-install (one id across all
  repos — closer to the "one toimija" north star, but leaks cross-repo
  correlation to every team you share a remote with).
- **Q2 — trailer opt-in surface.** Is `adopt --hooks` the right switch for
  the `Toimija-Env` trailer, or does it deserve its own flag so freshness
  hooks and identity stamping can be adopted independently?
- **Q3 — merge-tree cost gate.** Run the proof tier on every refresh, or
  only at `enter` and on demand? (Needs measurement; merge-tree is fast but
  refresh runs in the watch loop.)
- **Q4 — does L3 subsume 0004's travelling handoff entirely,** or is there a
  residual case (handoff for a branch that will never get a PR) worth
  keeping in mind?

## 7. Sequencing

1. L0 identity (mint + inferred tier only; trailer hook lands with Q2
   answered).
2. L1 overlap analyzer + packet section — the visible payoff, zero new
   state.
3. L2 merge-tree proof tier.
4. L3 travelling handoff.

Each layer is its own small PR. Stop and review between layers.

---

**One-line summary:** each clone gets a flag (`.git/toimija-id`), and the
packet gains team facts derived purely from what git already syncs — overlap
on entering, proven conflicts while working, handoff in the branch when
leaving — with freshness confessed on every line and presence deliberately
refused.
