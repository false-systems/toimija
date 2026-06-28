# Proposal 0001 — Per-session packets, awareness, and handoff

**Status:** ACCEPTED 2026-06-28 (decisions in §9). §7 boundary edits applied.
Layer 1 ships first as its own PR; Layers 2–3 do not start until Layer 1 lands.

**Date:** 2026-06-28

**Supersedes scope of:** the "do not add fleet / handoff" line in `CLAUDE.md`
and the "it does not coordinate agents, and should not be made to" line in
`AGENTS.md`.

---

## 1. Why now

v0.2 shipped the **env handshake** (Layer 0): `toimija run` exports
`TOIMIJA_RUN=1`, `TOIMIJA_LIVE`, and `TOIMIJA_LIVE_JSON` into the wrapped agent.
That changed what `run` *is*:

```
toimija run -- codex
```

no longer merely wraps Codex. It **introduces Codex to Toimija** through the
process environment — a stable, Unix-native contract:

```bash
cat "$TOIMIJA_LIVE"
```

The question "how does the agent know where its context is?" is now answered by
the environment, not by docs, not by memory, not by "please remember." Toimija
stopped being only a context generator and became a **process boundary**: the
wrapper is the point where live context is injected into the agent.

That reframe is what makes the next layers worth defining — and worth bounding.

## 2. The line

```
Layer 0 (shipped) — makes the agent Toimija-aware.   (env handshake)
Layer 1           — gives each agent its own packet.  (per-session state)
Layer 2           — makes Toimija agent-aware.        (awareness)
Layer 3           — lets a session leave a record.    (handoff / save)
```

Layers 1–3 are **local, synchronous, advisory, and human-orchestrated**. None of
them adds a daemon, a network call, a dispatcher, or decision-making. The human
remains the orchestrator; Toimija reports and records, it does not assign.

## 3. The problem Layer 1 fixes: shared-packet collision

Today every wrapped agent in a repo writes and overwrites **one** global
`.toimija/current.md`. With one agent that is fine. With several it is ambiguous:

```
current for whom?      for which task?
for which branch?      for which agent process?
```

Observed live: three agents (`toimija run -- claude` + two `toimija run -- codex`)
sharing this repo, all racing on the same file. The "live" packet reflects the
union of everyone's churn, not any one session.

**Fix:** per-session packets. Each `run` gets its own directory and its
handshake points the agent at *its own* packet.

## 4. Layer 1 — per-session packets

`toimija run`:

1. mints a **session id** (see open question Q1),
2. writes the live packet under a per-session directory,
3. exports the session into the child's environment,
4. keeps that session packet fresh via the existing watcher,
5. on exit, marks the session closed (and may leave a handoff record — Layer 3).

### Layout

```
.toimija/
  current.md            # optional repo-level "latest" summary (last writer wins)
  current.json
  sessions/
    <session-id>/
      current.md        # this session's live packet
      current.json
      session.json      # session marker: id, pid, agent argv, branch, started-at, status
```

### Environment (extends Layer 0)

```bash
TOIMIJA_RUN=1
TOIMIJA_SESSION=<id>
TOIMIJA_LIVE=.toimija/sessions/<id>/current.md         # absolute at runtime
TOIMIJA_LIVE_JSON=.toimija/sessions/<id>/current.json   # absolute at runtime
TOIMIJA_REPO_LIVE=.toimija/current.md                   # optional repo-level summary
```

The agent **primarily reads its own session packet** (`TOIMIJA_LIVE`).
`TOIMIJA_REPO_LIVE` is a convenience for "what does the repo look like overall,"
explicitly documented as last-writer-wins and not authoritative for any session.

### Fate of `.toimija/current.*`

Keep it, demoted to an **optional repo-level latest summary** (so existing docs,
the `enter`/`refresh`/`watch`/`context` commands, and muscle memory keep working),
and clearly label it as such. The per-session packet is the authoritative one for
a wrapped agent. `enter`/`refresh`/`watch` (no wrapped process, no session)
continue to write only `.toimija/current.*` — they are sessionless by nature.

### `.gitignore`

`.toimija/sessions/` is generated, ephemeral, and must be git-ignored (like
`current.*` already is).

## 5. Layer 2 — awareness

A new command (e.g. `toimija sessions`, exact name = Q4) lists **active sessions
in this repo**, read **synchronously**, on demand:

- reads `session.json` markers under `.toimija/sessions/`,
- "active" = the recorded pid is still alive **and** the marker is not closed;
  stale markers (dead pid, or older than a TTL) are reported as stale, not active,
- output is advisory text/JSON: who is working, on what branch, since when.

Awareness is also surfaceable inside `enter`/`context` so a newly-entering agent
sees "codex-XXX is already working this repo" as part of its packet.

**Explicitly NOT in Layer 2:**

- no dispatcher, no task assignment, no orchestration,
- no killing/signalling other sessions,
- no network, no daemon — `sessions` is a one-shot read of local files +
  process-liveness checks, in the same spirit as today's "run only while
  explicitly invoked."

Claiming/assignment stays where `AGENTS.md` already puts it: GitHub issues, draft
PRs, labels. Toimija reports presence; it does not arbitrate it.

## 6. Layer 3 — handoff / save

When a session ends (or on an explicit `toimija handoff` — Q4), the session may
write a **close/handoff record**: an append-only, local file capturing what the
session did, so a successor agent can resume without re-deriving everything.

Record contents (superset of the existing exit summary):

- session id, agent argv, branch, entry/exit HEAD,
- changed files (staged / unstaged / untracked at close),
- branch scope vs merge-base,
- validation: which plan tier was *suggested* (Toimija still never runs it),
- risks/coupling carried from the packet,
- a free-text "next step" the agent may write.

Constraints:

- **append-only, local files** (e.g. `.toimija/sessions/<id>/handoff.json` or a
  repo-level `.toimija/history/`),
- **no remote memory**, no network, no hidden background service,
- writing a handoff is a side effect of `run` exiting or an explicit command —
  never a daemon, never automatic across repos.

## 7. Proposed boundary edits (apply only after review)

These three must change **together** — `toimija doctor --strict` fails if
`.toimija/repo.yaml` and `AGENTS.md` drift apart.

### 7a. `.toimija/repo.yaml` — `boundaries:`

Add one line (keeps the no-daemon/no-network guardrail that defines Toimija):

```yaml
  - Agent coordination — per-session live packets, sibling-session awareness,
    and session handoff records — is in scope, but only as synchronous reads and
    writes during an explicitly-invoked command. Still no background daemons, no
    network, no task dispatch or assignment.
```

### 7b. `AGENTS.md`

- Mirror the same line in the Boundaries list (verbatim — the drift guard is
  lexical).
- Revise the "Working with other agents" closing line. From:

  > Toimija reports Git work state; it does not coordinate agents, and should not
  > be made to.

  to:

  > Toimija reports Git work state, surfaces live sibling sessions, and can record
  > a session handoff. It still does not *coordinate* agents — no dispatch, no
  > assignment, no orchestration. Claiming work stays in GitHub issues, draft PRs,
  > and labels; the human remains the orchestrator.

### 7c. `CLAUDE.md`

Remove `fleet` and `handoff` from the "do not add … unless a human explicitly
moves it into scope" list (this proposal is that explicit move), and keep
`MCP, Tree-sitter, LSP, SCIP, Sykli, Vaisto, Kerto, Terraform, workspace,
checkpoint` still excluded.

## 8. Hard boundaries (unchanged spirit)

- No daemon by default. Every layer runs only during an explicitly-invoked command.
- No network.
- No validation execution (Toimija still only *prints* the plan).
- No ownership of `AGENTS.md` / `CLAUDE.md`.
- No fleet decision-making, dispatch, or assignment. The human orchestrates.
- One Rust crate (no workspace split for this).

## 9. Decisions (accepted 2026-06-28)

- **D1 — session id scheme.** `<unix-timestamp-secs>-<pid>` (e.g.
  `1782608449-66987`). Roughly sortable by start time; the pid disambiguates
  sessions started in the same second. Minted from a pure constructor so tests
  are deterministic. (Pid reuse over time is acceptable for now; a marker that
  outlives its process is reported as stale via D2.)
- **D2 — staleness threshold: 90 minutes.** A non-closed marker whose pid is no
  longer alive, *or* that is older than 90m, is reported as stale (not active) by
  `toimija sessions`. **Layer 2 only** — Layer 1 records `started_at`/`status` but
  implements no staleness logic yet (no dead code lands early).
- **D3 — `.toimija/current.*`: demote-and-keep.** Remains as the repo-level
  "latest" summary, explicitly last-writer-wins, not authoritative for any
  session. Sessionless commands (`enter`/`refresh`/`watch`/`context`) keep writing
  only it. `run` writes the session packet *and* mirrors to `current.*`.
- **D4 — command surface.**
  - Awareness (Layer 2) is a new command named **`toimija sessions`** — *not*
    `fleet`.
  - `run` **auto-closes** its session marker on exit (a lifecycle concern, part
    of Layer 1: set `status: closed`, `closed_at`).
  - Rich handoff (Layer 3) is an **explicit `toimija handoff`** command, *not*
    automatic rich generation on every `run` exit. (The existing one-line exit
    summary stays; it is not a handoff record.)
- **D5 — single-repo for now.** Sessions live under one repo's `.toimija/`. The
  cross-repo "one Toimija" north star is a later proposal; `session.json` must not
  be shaped in a way that blocks it (keep the marker self-describing: it already
  records id, pid, agent, branch, timestamps).

### Layer 1 scope (this PR)

Per-session packets + extended handshake + marker lifecycle (open on start,
auto-close on exit). No `sessions` command, no `handoff` command, no staleness
logic. Hard boundaries from §8 unchanged: no daemon, no network, no dispatch, no
task assignment, no validation execution.

## 10. Sequencing

1. Accept §7 boundary edits → apply → `doctor --strict` green.
2. Layer 1 (per-session packets + extended handshake). Smallest, fixes a real bug
   (collision), unblocks everything.
3. Layer 2 (`sessions` awareness, read-only).
4. Layer 3 (handoff records).

Each layer is its own small PR. Stop and review between layers.
