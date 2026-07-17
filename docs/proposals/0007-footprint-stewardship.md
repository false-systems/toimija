# Proposal 0007 — Footprint stewardship: every artifact owned, memory outlives cleanup

**Status:** ACCEPTED 2026-07-18 (Yair dispatched implementation).

**Date:** 2026-07-18

**Builds on:** [0001](0001-sessions-awareness-handoff.md) (session dirs, handoff
records), [0002](0002-heartbeat-staleness.md) (heartbeat-aged retention),
[0006](0006-adoption-wrapped-means-used.md) ("wrapped means used" — an artifact
is done when its lifecycle is owned, not when it is written). No boundary
edits — everything here is a synchronous read or write during an
explicitly-invoked command; no daemon, no network, no dispatch.

---

## 1. The problem, observed live

Toimija writes state into every repo it manages, and that state is the
least-managed thing about the tool. Observed in this repo on 2026-07-18:

- **`.toimija/sessions/` accumulates corpses.** 13 session directories on
  disk; 2 live. `toimija sessions --prune` exists (#50/#66, 7-day retention)
  but is opt-in, and the evidence says opt-in cleanup does not happen.
- **`.toimija/runs/` is immortal.** Harness transcripts (kisko) append
  forever; no retention mechanism exists at all.
- **Atomic-write droppings survive.** A `session.json.tmp-<pid>-<n>` file
  from a crashed write sits in a dead session dir; nothing ever looks for it.
- **Pruning destroys memory.** The handoff record lives *inside* the session
  directory (`.toimija/sessions/<id>/handoff.jsonl`), so the same prune pass
  that removes a corpse also deletes the only narrative record of what that
  session did. Reclamation and remembrance are currently the same file
  operation, which forces a bad choice: hoard or forget.

None of this corrupts anything — every surface is gitignored. But a tool
whose pitch is *trust the packet, it replaces discovery* (README trust model,
#151) undermines itself when its own directory contains files it cannot
explain. Same move as `watch` proving readiness before reporting armed
(#144) and packets declaring what they were compiled from (#150): claims must
be checkable, and "toimija's state is accounted for" is a claim.

## 2. The principle

> Every file toimija writes must be **explainable** (doctor can say what it
> is), **attributable** (which session/command produced it), and
> **reclaimable** (a retention rule says when it goes away) — and reclamation
> must never be the only record that something happened.

Corollary, the fix for §1's bad choice: **memory and artifacts have different
lifetimes.** The artifact (session dir, transcript) is working state with a
retention window; the memory (what happened, distilled) is durable and
compact. Cleanup converts the first into the second instead of erasing both.

## 3. Design

Four pieces, smallest useful order. Dependency direction: the registry and
retention decisions are pure domain code (like the existing `prunable()`);
filesystem sweeping and the journal writer are adapters over it; commands
wire them up.

### 3.1 Artifact registry (inventory)

One module (`src/footprint.rs`) enumerates every artifact class toimija
writes. Not config — code, because the taxonomy changes only when toimija's
own behavior changes:

| Class | Path | Owner | Lifecycle | Retention |
|---|---|---|---|---|
| Config | `.toimija/repo.yaml` | human | permanent | never reclaimed |
| Live packet | `.toimija/current.{md,json}` | `enter`/`refresh` | regenerated | overwritten in place |
| Session dir | `.toimija/sessions/<id>/` | `run` | heartbeat-scoped | closed/stale + retention (0002 aging) |
| Run transcript | `.toimija/runs/*` | harness under `run` | append-once | retention window |
| Snapshot lease | `.toimija/snapshots/*.json` + git refs | `snapshot` | lease-scoped | git gc owns the objects (unchanged) |
| Temp file | `*.tmp-*` anywhere under `.toimija/` | atomic writes | transient | orphaned when parent write concluded |
| History journal | `.toimija/history.jsonl` | sweep/`handoff` | durable | compaction only (§3.2) |

The registry is what makes "unexplainable file" a computable judgment: any
path under `.toimija/` matching no class is flagged by doctor (§3.4).

### 3.2 The history journal (memory)

`.toimija/history.jsonl` — append-only, gitignored, one JSON line per
concluded session. Written **by the sweep, before deletion**: when a session
directory becomes reclaimable, its marker and handoff records are distilled
into one record, the record is appended and fsynced, and only then is the
directory removed. Append fails → deletion does not happen; hoarding beats
forgetting.

Record shape (versioned like the handoff schema):

```json
{"v": 1, "id": "1783706737-17582", "agent": "claude", "branch": "main",
 "intent": "...", "scope": ["src"], "started_at": 1783706737,
 "last_seen_at": 1783713000, "end": "closed", "handoffs": ["<note>", "..."],
 "recorded_at": 1784326111}
```

- **Read by `toimija history`** — newest first, `--json` for machines. This
  is the answer to "who touched this repo last week and what were they
  doing", which today dies with the session dir.
- **Bounded.** `history_max_records` (default 500): on append past the cap,
  oldest records drop. A flat JSONL file with compaction, not a database —
  the design test "does git already store this?" says commits already store
  *code* outcomes; the journal stores only the session narrative git never
  sees (intent, agent, handoff notes, how it ended).
- **Handoff stays where it is** while the session lives — siblings read it
  from the session dir exactly as 0001 shipped it. The journal is where it
  *retires to*, not a second live location.

### 3.3 The sweep (reclamation)

One pass, `sweep()`, unifying three reclamations under the existing
conservative rules (live never touched, unreadable never deleted):

1. session dirs — the existing `prune` logic, now journaling first (§3.2);
2. run transcripts past `runs_retention_days`;
3. orphaned `*.tmp-*` files older than one hour.

Two entry points:

- **Opportunistic:** at the end of `enter` and `run` session setup —
  best-effort, logged, never blocks or fails packet compilation. These are
  the moments toimija is already writing to `.toimija/`; no timer, no
  daemon, the sweep exists exactly as long as the command does (0002's
  heartbeat argument, applied to cleanup).
- **Explicit:** `toimija clean` — the same pass, with `--dry-run` to show
  what would go and a summary of what went. Exists so the opportunistic path
  can stay silent and boring.

### 3.4 Configuration and doctor

`repo.yaml` grows one optional block, defaults in code:

```yaml
stewardship:
  auto_sweep: true            # opportunistic sweep on enter/run
  sessions_retention_days: 7  # existing DEFAULT_RETENTION_SECS, surfaced
  runs_retention_days: 14
  history_max_records: 500
```

`toimija doctor` grows a footprint section — the inventory made visible:
per-class counts and sizes, what is past retention, any path matching no
registry class ("unexplainable"), and whether `.gitignore` covers every
generated class. `doctor --strict` fails on unexplainable files and missing
ignore entries; `toimija adopt` writes the ignore entries, same
flag-then-fix pattern as the existing boundary drift guard.

## 4. Decisions

- **D1 — memory outlives artifacts.** Reclamation always journals before it
  deletes; a failed journal append aborts the deletion. Hoarding is the
  failure mode, never amnesia.
- **D2 — the sweep rides existing commands.** Opportunistic cleanup happens
  only inside `enter`/`run`, best-effort; `clean` is the explicit,
  inspectable form. No timers, no daemons.
- **D3 — the registry is code, config is knobs.** What artifact classes
  exist is `src/footprint.rs`; how long they live is `stewardship:` in
  `repo.yaml` with code defaults. Doctor validates the knobs.
- **D4 — unexplainable is a defect.** A file under `.toimija/` matching no
  registry class is a `doctor --strict` failure, on the same footing as
  boundary drift.
- **D5 — git keeps what git already has.** Snapshots stay refs under
  `git gc`; the journal holds only session narrative that never reaches a
  commit. No second database.

## 5. Non-goals

- No background reclamation, timers, or daemons (boundary, unchanged).
- No cross-repo or cross-machine history (0004 territory, parked).
- No analytics, metrics export, or history search beyond `history`/`--json`.
- No journaling of run transcript *content* — transcripts are reclaimed, not
  distilled; the session record already names what ran.

## 6. Resolved questions

- **Q1 — teardown journals.** `run` appends a `closed` record when it closes
  the marker. A later sweep deduplicates by session id, so history is complete
  without waiting for retention and every session has at most one record.
- **Q2 — run retention is 14 days.** The stewardship config may override it;
  transcripts are retained, never interpreted.
