# Proposal 0005 — Workset contract: from advice to obligation

**Status:** DRAFT 2026-07-10 — awaiting Yair.

**Date:** 2026-07-10

**Builds on:** [proposal 0001](0001-sessions-awareness-handoff.md) (session
markers, `intent`). No boundary edits — Part A is render-time framing of data
the packet already carries; Part B is a new synchronous, explicitly-invoked
command. Still no daemon, no network, no dispatch, and no code parsing.

---

## 1. The problem, observed live

An agent (Codex) working a large Toimija-prepared audit burned its entire
provider usage window in one session — much of it re-reading and
re-discovering the same repository state the packet had already compiled.
The packet was in front of it the whole time.

The diagnosis: toimija reduces context **presentation** but does not govern
context **acquisition**. Every load-bearing section of the packet — Scope,
Read order, Read first, Configured exclusions, Coupling, Validation plan —
is phrased as *description*. An agent reading descriptions can rationally
treat them as a preamble and still do its own repository archaeology "to be
safe". Nothing in the packet says what the agent owes back when it goes
wider, so widening is free, invisible, and unbounded.

Two distinct failure shapes hide in that incident:

- **Task-scoped work** (fix a bug, land a feature): the packet already names
  a sufficient working set, but nothing frames it as the working set.
- **Breadth-scoped work** (audits, migrations): reading widely is
  *legitimate* — the failure is doing it in one enormous session with no
  persistence between passes, so every pass re-proves the last one.

One fix does not cover both. Part A addresses the first; Part B the second.

## 2. What already exists — the gap is framing, not data

The "missing contract" is mostly shipped:

| Contract element        | Already in the packet                     |
| ----------------------- | ----------------------------------------- |
| Primary/supporting files| Scope (`in play` / `unchanged`), Read order |
| Read-first docs         | Read first                                |
| Do-not-scan list        | Configured exclusions (from `repo.yaml`)  |
| Known facts             | Coupling, Risk, Branch surface            |
| Validation commands     | Validation plan (focused → strict)        |
| Expansion declaration   | `toimija intent "<why>" --scope <path>`   |

What is missing is a single normative paragraph that binds them together —
and a breadth mode for the audit shape.

## 3. Part A — the workset contract (render-only)

The packet gains a short policy block, rendered between the Session and Git
sections, that reframes the existing sections as a contract:

```markdown
## Workset contract
Work from this packet's Scope and Read order; they are your working set,
not a preamble. Configured exclusions are do-not-scan. You MAY read outside
the workset when a concrete unresolved dependency requires it — but you
MUST declare it first: `toimija intent "<why>" --scope <path>`. Expansion
is never forbidden; invisible expansion is.
```

- **Permission and visibility are split.** A hard workset would break
  legitimate work and get ignored; a declared expansion keeps the agent
  honest and makes over-reading *observable* — it lands on the session
  marker, shows up in `toimija sessions`, and siblings see the widened
  scope.
- **No new state, no new commands.** `intent --scope` already appends scope
  paths to the marker synchronously. The contract merely aims existing
  machinery.
- **Advisory by construction.** Toimija cannot observe what an agent reads
  (and must not grow the machinery to try — see R2). The contract works the
  way AGENTS.md works: it is the instruction layer agents actually weight.

## 4. Part B — staged breadth for audits (sketch, follow-up sized)

For breadth-scoped work, the unit of control is the *pass*, not the file.
Sketch, to be sized in its own issue once Part A lands:

- `toimija audit-plan [--surfaces N]` partitions the repo into bounded
  surfaces from Git state alone (top-level areas, `ls-tree` file counts,
  churn) and prints one workset per surface.
- Findings persist as **committed files** (the existing
  `docs/audit/<date>/` shape). Each surface's packet points at prior
  committed findings so later passes read conclusions instead of re-deriving
  them. Git is the cross-run cache; there is no other one.

## 5. Explicitly rejected

Recorded so they are not re-proposed after the next rate-limit incident:

- **R1 — symbol-level worksets** ("Known symbols: `ContextBuilder`…").
  Requires parsing code; that is Tree-sitter/LSP territory, out of scope by
  boundary. Toimija's relevance is Git-mined: it can say *which files*,
  never *which symbols*.
- **R2 — read telemetry** (track what the agent actually opened). `run`
  wraps the agent as a parent process and cannot see file reads without
  fsevents/ptrace-shaped surveillance — daemon-shaped machinery guarding
  state Git does not store.
- **R3 — an understanding cache outside Git** (durable repo knowledge
  recomputed across runs). Fails the design test *does Git already store
  this?* Committed findings files are the cache; a second store would need
  its own invalidation, GC, and trust story.

## 6. Decisions

- **D1 — the packet carries a normative Workset contract block.**
  Render-only; wraps existing Scope/Read order/exclusions data in
  obligation language. Applies to session packets and `enter`/`context`
  output alike (one code path).
- **D2 — expansion is free but never invisible.** Reading outside the
  workset is always permitted and always declared, via the existing
  `toimija intent --scope`. No enforcement, by design.
- **D3 — Configured exclusions are rendered as an instruction**
  ("do not scan"), not an inventory.
- **D4 — staged audit breadth is in scope, separately sized.** Surfaces
  partitioned from Git state; findings persisted as committed files;
  command name (`audit-plan` vs `enter --surface N/M`) decided at
  implementation.
- **D5 — R1–R3 are rejected, not deferred.**

## 7. Open questions for Yair

- Contract wording strength: is MUST-declare the right calibration, or
  should first-render packets soften to SHOULD until we have dogfooding
  evidence?
- Does the contract block belong in `--for-diff` output only, or in every
  packet including bare `toimija context`?
