# Proposal 0008 — Suspension-aware liveness: the lid is an event

**Status:** ACCEPTED 2026-07-19 (Yair dispatched implementation).

**Date:** 2026-07-19

**Builds on:** [0002](0002-heartbeat-staleness.md) (the watcher is the
heartbeat; staleness measures inactivity), #144 (armed means proven),
[0007](0007-footprint-stewardship.md) (reaping and the history journal). No
boundary edits — no daemon, no network, no signal handlers, no new
dependencies; everything runs inside the already-sanctioned foreground watch
loop.

---

## 1. The problem, observed live

Close a MacBook lid while `toimija run` wraps a working agent. Three distinct
things go wrong, two of them observed in this repo:

1. **False STALE after wake.** The heartbeat is stamped only when the watcher
   refreshes, and the watcher refreshes only on filesystem/Git events
   (0002 D2-r2). A machine asleep for >90 minutes produces no events, so at
   wake `now − last_seen_at` exceeds the staleness window and every sibling
   sees `STALE` / "marked open but looks stale" — for a session whose agent
   is alive and mid-conversation. Nothing revives the label until a file
   happens to change. 0002 removed exactly this mislabel for long-*idle*
   sessions; suspension reintroduces it through a gap that decision did not
   foresee: wall-clock time passing while no observer runs.
2. **Death without teardown.** `run`'s teardown (stop watcher → close marker)
   runs only on a normal exit. Battery death in the bag, shutdown, force-quit
   → an unclosed marker (a corpse labeled stale for up to 7 days) and, when
   the kill lands mid-refresh, an orphaned `current.json.tmp-<pid>-<seq>`
   atomic-write dropping. Observed: the tmp files a session once cleaned up
   carried the pids of two same-day stale sessions — the forensic signature
   of this death.
3. **The poll watcher is laptop-hostile.** `watch.rs` uses
   `notify::PollWatcher` at 500 ms — a full recursive rescan of the repo
   twice a second, chosen when native backends missed the readiness probe in
   CI (#144). On a laptop this costs battery whenever the lid is open and is
   throttled unpredictably by App Nap as it closes.

Root cause of (1): the liveness model assumes **continuous execution**.
Suspension — the whole machine pausing — is invisible to a model that reads
only wall-clock timestamps.

## 2. Two observations that shape the fix

**Suspension freezes observers too.** Toimija is local-only (boundary): every
session *and every reader of `toimija sessions`* lives on the same machine.
When the lid closes they all stop together; the mislabel window opens only at
wake. So no sleeping-side work is needed at all — the entire fix is
wake-side: make *resumption* an observed event.

**The two clocks disagree about sleep.** Wall clock (`SystemTime`) advances
during suspension; the monotonic clock (`Instant`) does not on macOS or
Linux. A loop that samples both each tick can detect suspension as the
*disagreement* between their deltas — no OS sleep/wake notification API, no
IOKit, no systemd, no daemon, no new dependency. The machinery that already
wakes every 200 ms to check the stop flag is sufficient.

## 3. Design

### Part A — resume detection in the watch loop

A pure function decides; the loop samples:

```rust
/// Both clocks, sampled at the same instant.
struct ClockPair { wall: SystemTime, mono: Instant }

/// Some(slept) when wall time advanced materially more than monotonic time
/// between two samples — the signature of suspension. Pure; unit-tested with
/// fabricated pairs, no real clock, no sleep in tests.
fn detect_suspension(prev: &ClockPair, now: &ClockPair, threshold: Duration)
    -> Option<Duration>
```

- Threshold **30 seconds**: far above scheduler jitter and App Nap stretch,
  far below the 90-minute staleness window. A large NTP step also trips it;
  the consequence (one extra refresh) is harmless, so no attempt to tell
  them apart.
- `Session::run` samples a `ClockPair` every loop iteration. On detection it
  logs `toimija.watch.resumed` (`slept_secs`), runs **one** refresh through
  the existing `plan.run`, and calls the existing `on_refresh` callback — so
  under `run`, the heartbeat stamp arrives through the same closure every
  other refresh uses. No new write path exists.
- Latency: the session revives within one tick (≤200 ms) of wake — before
  any human or sibling plausibly runs `toimija sessions`.

This does not repeal 0002 D2-r2 ("no timer-based touching"). A resume is not
a timer: it is an event — the machine coming back — and 0002's own rule is
that an event worth a refresh is worth a heartbeat. An idle-but-awake session
still decays to stale exactly as 0002 intends; only suspension earns revival,
because it was the *machine* that went away, not the agent.

### Part B — native watcher, proven ready, poll fallback

Replace the hardcoded `PollWatcher` with the platform-native backend
(`notify::RecommendedWatcher`: FSEvents on macOS, inotify on Linux —
`fsevent-sys` already ships in our notify 8.2.0 tree; zero new
dependencies), keeping #144's readiness contract:

1. Arm the native watcher; run the existing probe-file readiness check.
2. Probe seen within the window → armed on the native backend. Kernel-push
   events, no rescans, no battery burn, no App Nap sensitivity.
3. Probe **not** seen → drop it, arm a `PollWatcher` (today's configuration),
   re-probe. CI and exotic filesystems — the environments that motivated
   polling — land here by measurement, not by punishing every laptop.
4. Either way the armed line names its evidence:
   `toimija watch: armed (fsevents)` / `(poll fallback)`, and a structured
   `toimija.watch.armed` event carries the backend. Armed stays a proven
   claim.

### Part C — ungraceful death is a first-class path (statement, no new work)

Nothing can run at SIGKILL or power loss, so teardown-hardening (signal
handlers, atexit hooks) is **explicitly rejected**: it cannot cover the worst
case, and a recovery path that must exist anyway should be the only path.
That path is already built or dispatched:

- pid-liveness classifies the corpse (`Stale`, never mislabeled live);
- 0007's sweep reaps it after retention **and journals it** —
  `end: reaped_stale` in `history.jsonl` is precisely the durable memory of
  "this session ended by the lid, not by choice";
- 0007's D7-5 collects the mid-write `*.tmp-*` droppings.

This proposal adds one refinement to 0007's record derivation: a reaped
session whose marker was never closed and whose last heartbeat long predates
the reap is what a lid-death looks like; the record already carries
`last_seen_at` and `end`, so no schema change — just a test pinning that a
fabricated lid-death corpse round-trips into an honest record.

## 4. Decisions

- **D8-1 — suspension is detected by clock disagreement.** Wall vs monotonic
  delta, 30 s threshold, pure `detect_suspension`, sampled by the existing
  200 ms tick. No OS notification APIs.
- **D8-2 — resume is an event, not a timer.** Exactly one refresh + heartbeat
  through the existing event path (`plan.run` + `on_refresh`); 0002's
  idle-decay semantics otherwise unchanged.
- **D8-3 — native backend first, poll by measured fallback.** The #144 probe
  decides per launch; the armed report names the backend; readiness stays
  proven on both paths.
- **D8-4 — no teardown heroics.** No signal handlers, no atexit. Corpse
  classification + 0007 reaping/journaling is the designed, tested path for
  ungraceful death.
- **D8-5 — thresholds are constants, not config.** 30 s detection and the
  existing 5 s probe window are code constants with doc comments; stewardship
  config (0007) stays about retention. Config can come later if reality
  disagrees; a knob is not a design.

## 5. Non-goals

- No OS sleep/wake notification integration (IOKit, `systemd-inhibit`).
- No cross-machine liveness (0004, parked).
- No change to staleness windows, retention, or 0002 display semantics.
- No attempt to refresh *during* sleep or catch events missed while
  suspended — locally nothing changes while the machine is off; the one
  wake-side refresh re-derives everything from Git state anyway.

## 6. Testing

- `detect_suspension`: fabricated `ClockPair`s — no sleep, no real clocks in
  tests. Cases: genuine suspension, scheduler jitter under threshold, NTP
  step over threshold (fires, documented as accepted), zero/negative wall
  delta (clock set back → never fires).
- Loop wiring: clock sampling injected (the same seam `prunable` uses for
  `now_secs`) so a unit test drives a fake pair sequence through
  `Session::run` and asserts exactly one refresh + one `on_refresh` per
  detected suspension.
- Backend fallback: force the native probe to fail (unwatchable-by-native
  fixture or injected prober) and assert the poll fallback arms and reports
  honestly.
- Lid-death record: fabricate an open marker with a dead pid and an old
  `last_seen_at`, reap via 0007's path, assert the history record's
  `end: reaped_stale` + timestamps tell the true story.
