# Proposal 0002 — Heartbeat staleness: idle time, not lifetime

**Status:** ACCEPTED 2026-07-02 (Yair directed implementation of issue #67).

**Date:** 2026-07-02

**Revises:** decision **D2** of
[proposal 0001](0001-sessions-awareness-handoff.md) (the 90-minute staleness
TTL). No boundary edits — everything here is a synchronous write during the
already-sanctioned `run` watcher; no daemon, no network, no dispatch.

---

## 1. The problem, observed live

D2 marks an open session **stale** when its pid is gone *or* the marker is
older than 90 minutes. The second clause conflates two different situations:

- a session that was abandoned an hour ago (what D2 meant), and
- a session that has been *legitimately working* for two hours (what D2 hits).

Observed while dogfooding: an interactive Claude session wrapped by
`toimija run`, pid alive, mid-conversation, shipping PRs — listed `STALE` by
`toimija sessions` and flagged "marked open but looks stale". The stale label
also feeds the GC path; #66 removed the destructive consequence (a live pid is
never pruned), but the *mislabel* remained: siblings were told a working agent
looked abandoned.

The root cause: the TTL measures **lifetime** (`now − started_at`), and a
session's lifetime says nothing about whether anyone is still home.

## 2. The fix: measure inactivity

Add one field to the session marker:

```json
"last_seen_at": 1783012345
```

- **Written by machinery that already runs.** `run`'s watcher refreshes the
  session packet on every relevant Git/filesystem change; it now also stamps
  `last_seen_at = now` on the marker (best-effort — a failed touch never
  interrupts the watch loop). No new process, no timer, no daemon: the
  heartbeat exists exactly as long as `run` does.
- **Staleness keys off the last heartbeat, not birth.** An open marker is
  stale when its pid is gone, or when `now − last_seen_at` exceeds the 90m
  window. A day-long *active* session stays `LIVE`; a session whose heartbeat
  froze (crashed harness, killed terminal) goes stale within the window, even
  if a recycled pid happens to look alive — D2's pid-reuse guard is preserved,
  now aimed at the honest signal.
- **Old markers keep working.** `#[serde(default)]`: a marker without the
  field falls back to `started_at`, which is exactly D2's old behavior —
  strictly no regression for records written before this proposal.

## 3. What "stale" means now

> Its process is gone, or it has shown no activity for 90 minutes.

An *idle-but-alive* session (agent thinking, human away from the keyboard,
no file or Git changes for 90 minutes) reads stale. That is deliberate and
honest — "no observed activity" is precisely the information a sibling needs,
and the pid-liveness half of the check still distinguishes "quiet" from
"gone" in the warning text. When the session touches anything, the next
refresh revives it to `LIVE`.

GC (`prune`, from #50/#66) also ages a dead-pid stale session from its last
heartbeat rather than its birth: retention counts from when it was last seen
alive.

## 4. Decisions

- **D2-r1 — staleness window measures inactivity.** 90 minutes unchanged as a
  number; its meaning changes from "open longer than" to "no heartbeat for".
- **D2-r2 — the watcher is the heartbeat.** No timer-based touching: an event
  already worth a packet refresh is worth a heartbeat, and nothing else is.
  (A truly idle session intentionally decays to stale; see §3.)
- **D2-r3 — `last_seen_at` is optional and additive.** Missing → fall back to
  `started_at` (old behavior). `sessions` renders a `seen: <age> ago` line
  when the field is present.
