# Toimija — Implement proposal 0008: suspension-aware liveness

You are Codex, working in the `toimija` Rust crate. Implement **proposal 0008
completely**, as two small PRs plus one dependent test PR: resume detection
in the watch loop, the native watcher backend with proven fallback, and the
lid-death record test. The design is
`docs/proposals/0008-suspension-aware-liveness.md`; this prompt locks the
implementation-level decisions on top of its D8-1…D8-5. Where this prompt
locks a decision, do not relitigate it. Where it points at a source file,
read the actual code before writing an identifier — never guess.

Work like a lighthouse keeper: the light must come back the instant the power
does, and the logbook must say the outage happened. Your entire feature is
the wake-up; nothing you write runs while the machine sleeps, and anything
you're tempted to run *at* death (signal handlers, atexit) is forbidden by
design — reread proposal §3 Part C until that feels right, then don't.

This prompt being dispatched constitutes acceptance of proposal 0008: your
first PR flips its status line to `**Status:** ACCEPTED 2026-07-19 (Yair
dispatched implementation).`

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` must be green at **every commit**, not just the last one. The
pre-commit hook mirrors these gates; do not bypass it. Run
`toimija doctor --strict` before claiming any PR complete.

---

## 0. Load context first (do not skip)

If `$TOIMIJA_LIVE` is set you are wrapped by `toimija run`: read that file
first and re-read it after Git-state changes. Otherwise run
`toimija enter --for-diff --budget 4000`. Then read, in order:

- `AGENTS.md` — boundaries and multi-agent coordination. You will open and
  claim a tracking issue per PR (`gh issue create` then
  `--add-assignee @me`); none exist yet for 0008.
- `docs/proposals/0008-suspension-aware-liveness.md` — the design. §2's two
  observations are the reasoning; §4's D8-1…D8-5 are law.
- `docs/proposals/0002-heartbeat-staleness.md` — the semantics you must NOT
  change: idle-but-awake still decays to stale; the watcher is the heartbeat;
  an event worth a refresh is worth a heartbeat. Your resume is such an
  event.
- `src/watch.rs` — your primary file. Read all of it. Note precisely:
  `BackendWatcher` is a type alias for `notify::PollWatcher`; `start()` arms,
  writes a `toimija-watch-probe-{pid}` file, calls **`watcher.poll()`** — a
  `PollWatcher`-only method that does not exist on the native backend (this
  single line is why the backend switch is an enum, not an alias swap) —
  then proves readiness via a 5 s `recv_timeout` loop before removing the
  probe; `Session::run` is the loop you extend (TICK = 200 ms re-check,
  `drain_burst` with DEBOUNCE = 300 ms, the four match arms and their error
  posture); `watcher_config()` sets the 500 ms poll interval; `watch()`
  prints `toimija watch: armed`.
- `src/cli.rs` `run_run` — the watcher thread (`std::thread::spawn` around
  `watch_session.run`) whose `on_refresh` closure bumps the refresh counter
  and stamps the heartbeat via `session::touch`. Your resume path must flow
  through this exact closure — you add **no** new heartbeat call sites. Also
  the `toimija run: watcher armed` line.
- `src/session.rs` — `touch()`, `now_secs()` (the wall-clock convention),
  `SessionMarker`.
- `src/sessions.rs` — `prunable()` and its `now_secs: u64` parameter: the
  house pattern for testable time. Your clock seam follows it in spirit.
- `tests/cli_watch.rs`, `tests/cli_run.rs` — find every assertion that pins
  the `armed` output lines (#144 shipped tests around "prove readiness
  before reporting armed"). You will update these strings deliberately in
  PR 2; understand them first so you extend the proof, never weaken it.
- `Cargo.toml` — notify is 8.2.0; `cargo tree -p notify` shows `fsevent-sys`
  already present. The native backend costs zero new dependencies. Adding
  any dependency to this work is wrong.
- For PR 3 only: `src/footprint.rs` and `docs/schemas/history-record.v1.schema.json`
  **on main** — these land from proposal 0007's PR 1, which may still be in
  flight. Verify with `git fetch && git log --oneline origin/main` (or
  `toimija landed`) before starting PR 3; if 0007 PR 1 has not merged, PR 3
  is blocked — say so on its issue and stop, do not reimplement its types.

## 1. What you are building

No new commands. Three behavior changes:

```
# Part A (PR 1): a session revives itself within one tick of wake
toimija.watch.resumed   slept_secs=<n>        # structured event on detection
# ...followed by exactly one refresh + heartbeat through the existing path

# Part B (PR 2): armed names its backend, laptops stop paying the poll tax
toimija watch: armed (native)                  # FSEvents/inotify, probe-proven
toimija watch: armed (poll fallback)           # probe timed out on native
toimija run: watcher armed (native)            # same, under run
toimija.watch.armed     backend=native|poll    # structured event

# Part C (PR 3): the journal tells lid-deaths honestly (test only)
```

## 2. Contracts

### 2.1 Resume detection (PR 1)

In `src/watch.rs` (private to the module; nothing else needs it):

```rust
/// Both clocks, sampled together. Wall advances during suspension; the
/// monotonic clock does not (macOS and Linux) — their disagreement is the
/// suspension signal (proposal 0008 §2).
struct ClockPair { wall: SystemTime, mono: Instant }

/// Threshold for calling a tick gap a suspension: far above scheduler
/// jitter and App Nap stretch, far below the 90-minute staleness window.
const SUSPENSION_THRESHOLD: Duration = Duration::from_secs(30);

/// Some(slept) when wall time advanced more than monotonic time by at least
/// `threshold` between samples. Pure — no clock reads inside. A wall clock
/// set *backwards* (negative wall delta) never fires.
fn detect_suspension(prev: &ClockPair, now: &ClockPair, threshold: Duration)
    -> Option<Duration>
```

Wiring in `Session::run`: sample a `ClockPair` at loop top each iteration;
compare against the previous sample; on `Some(slept)` — log the
`toimija.watch.resumed` event (`slept_secs`), run **one** `plan.run` with the
same warn-and-continue error posture the event arm uses (a failed resume
refresh must not kill the loop), call `on_refresh()` once on success, then
update the previous sample and continue. Detection must also work when the
loop was blocked in `recv_timeout` during the sleep — which it will be; the
sample point at loop top covers both the Timeout and the event arms.

Testability: the public `Session::run` signature does not change. Introduce a
private seam — `run_with_clocks(..., clocks: impl FnMut() -> ClockPair)` (or
equivalent), with `run` passing the real sampler — so a unit test drives a
fabricated pair sequence through the loop logic. Pure `detect_suspension`
tests carry the edge cases; the seam test proves the wiring: exactly one
refresh and one `on_refresh` per detected suspension, zero on jitter.

### 2.2 Backend selection (PR 2)

```rust
/// The armed filesystem watcher: platform-native (FSEvents/inotify) when it
/// proves readiness, the polling scanner when it does not. The probe is the
/// arbiter (#144); the label is part of the armed claim.
enum Backend { Native(RecommendedWatcher), Poll(PollWatcher) }
```

`start()` becomes: arm native → probe (the existing probe-file + 5 s
`recv_timeout` loop, unchanged in mechanism) → on proof, `Backend::Native`;
on timeout, drop it, arm `PollWatcher` with today's `watcher_config()`,
**re-probe from scratch** (fresh probe file; the poll path additionally
calls `.poll()` exactly as today) → on proof, `Backend::Poll`; on a second
timeout, fail `start()` with the existing readiness error. `Session` holds
the enum; the label (`"native"` / `"poll fallback"`) comes from one method on
`Backend`, used by both armed lines and the `toimija.watch.armed` event —
one source of truth, no per-OS strings. Update every pinned `armed`
assertion in `tests/cli_watch.rs` / `tests/cli_run.rs` to expect the labeled
form; the #144 property being tested — no armed report before a proven
probe — must survive verbatim on both paths.

### 2.3 Lid-death record (PR 3)

One test (no production code unless it fails honestly): fabricate an open
marker with a dead pid, an old `last_seen_at`, and no `closed_at`; drive
0007's reap path; assert the history record carries `end: reaped_stale`, the
frozen `last_seen_at`, and `started_at` — the true story of a lid-death. If
the record derivation cannot express this, the fix belongs in
`derive_record`, not in the test's expectations.

## 3. Locked decisions

Proposal decisions **D8-1…D8-5 stand verbatim.** Additionally:

- **D8-6 — one seam, house-patterned.** Time enters the loop only through
  the injected sampler; `detect_suspension` never reads a clock. Mirror the
  `prunable(now_secs, …)` discipline.
- **D8-7 — resume refresh shares the event arm's posture.** Warn-and-continue
  on failure, identical message style to the existing
  `refresh failed (packet may be stale)` line. The loop never dies from a
  resume.
- **D8-8 — no burst interaction.** A resume refresh does not call
  `drain_burst`; if real filesystem events also arrive at wake, they are
  handled by the ordinary event arm with its ordinary debounce. Worst case is
  two refreshes at wake — idempotent by design; do not add cleverness to
  prevent it.
- **D8-9 — the enum, not a trait object.** `Backend` is a two-variant enum
  (illegal states unrepresentable: there is no "armed but unknown backend");
  no `Box<dyn Watcher>`, no generic parameter leaking into `Session`'s public
  type.
- **D8-10 — constants, not config.** `SUSPENSION_THRESHOLD` and the probe
  window are doc-commented constants (D8-5). No `stewardship:` entry, no env
  var, no flag.

## 4. Craft rules — the skills, baked in

- **Dependency direction.** Decisions pure, effects at the edge:
  `detect_suspension` is a pure function tested with fabricated pairs;
  clock sampling and refresh execution stay in the loop. If a test for the
  *decision* needs a real clock or a sleep, the decision is in the wrong
  layer.
- **Illegal states unrepresentable.** The backend is an enum (D8-9); a
  detected suspension is `Option<Duration>`, not a bool plus a field.
- **Errors are data, posture is explicit.** The resume path's
  warn-and-continue is a deliberate, commented posture (D8-7), matching the
  arms around it — never a bare `let _ =`.
- **Names are contracts.** `detect_suspension` is pure; `run_with_clocks`
  runs; the `Backend` label method is a getter with no side effects. No
  `check_` that refreshes, no `get_` that arms.
- **Tests are specifications.** House naming:
  `suspension_is_detected_when_wall_time_outruns_the_monotonic_clock`,
  `scheduler_jitter_under_the_threshold_is_not_a_suspension`,
  `a_wall_clock_set_backwards_never_fires`,
  `resume_triggers_exactly_one_refresh_and_heartbeat`,
  `native_probe_timeout_arms_the_poll_fallback_and_says_so`,
  `a_reaped_lid_death_records_its_frozen_heartbeat`. Do not test private
  internals beyond the declared seam; if an existing test fights you,
  re-read §3 — especially the #144 armed tests, which you extend, never
  weaken.
- **No sleeping in tests.** No `std::thread::sleep` beyond what the existing
  watch tests already do; suspension is always fabricated pairs. CI time is
  a budget.
- **No new dependencies**, runtime or dev. The native backend is already in
  the tree.
- **Delete code > write code.** PR 2 replaces the alias with the enum; it
  must not leave a second arming path beside the first. One `start()`, one
  probe mechanism, two outcomes.

## 5. Order of work — three PRs, bottom-up

Per PR: open/claim its issue, branch from a **fresh** `origin/main`
(`git fetch` first), small gated commits, rebase before opening,
`Closes #<n>`, describe decisions exercised and test evidence. After each
merge: `git pull && cargo install --path .`. Check PR state before every
push (`gh pr view <n> -q .state`) — this repo merges fast.

### PR 1 — `feat/0008-resume-detection` (the light comes back)

1. `ClockPair`, `SUSPENSION_THRESHOLD`, pure `detect_suspension` + the four
   edge-case tests (§2.1, §6 of the proposal).
2. Loop wiring through the private clock seam + the exactly-one-refresh
   seam test. The `on_refresh` closure in `run_run` is untouched — read it,
   confirm the heartbeat flows, change nothing there.
3. Flip 0008's status line. Docs: one sentence in README's watch/run story
   ("survives laptop sleep: a resumed session re-proves itself within a
   tick") and, if `docs/reference.md` describes watch behavior, one line
   there. `tests/docs.rs` pins README structures — check before editing.

### PR 2 — `feat/0008-native-watcher` (the poll tax ends)

1. `Backend` enum + `start()` rework with the two-stage probe (§2.2);
   structured `toimija.watch.armed` event.
2. Labeled armed lines in `watch()` and `run_run`; update the pinned
   assertions in `tests/cli_watch.rs` / `tests/cli_run.rs` deliberately —
   each updated test keeps proving "no armed before proof".
3. Fallback test (§4 naming) — force the native probe to fail via the seam
   or an unwatchable fixture; assert poll arms and the label says so.
4. README/reference: the armed line now names its evidence; one sentence.

### PR 3 — `test/0008-lid-death-record` (the logbook is honest)

Blocked on 0007 PR 1 reaching `origin/main` — verify first (§0, last
bullet). Then the single test of §2.3, in 0007's test home for reaping
(follow where `reap`'s tests live), plus any `derive_record` fix it flushes
out. Smallest PR of the series; keep it that way.

## 6. Forbidden moves

- No signal handlers, no `atexit`, no `Drop`-based teardown heroics, no
  detecting "the lid is closing" — you only ever detect that it *closed*,
  afterwards (proposal Part C; D8-4).
- No OS sleep/wake notification APIs: no IOKit, no `systemd-inhibit`, no
  polling `pmset`. The two-clock disagreement is the entire detector.
- No new dependencies of any kind. No config knobs, env vars, or flags for
  thresholds (D8-10).
- No timer-based heartbeats — 0002 D2-r2 stands. The resume heartbeat rides
  the existing `on_refresh` closure; you add no call to `session::touch`
  anywhere.
- No change to staleness windows, retention arithmetic, `Liveness`
  semantics, or the display rules of 0002/`sessions`.
- No weakening the #144 readiness contract: every armed report, native or
  fallback, follows a seen probe. Never report armed on hope.
- No `std::thread::sleep`-driven suspension tests. No flaky-tolerance
  (`retry`, generous timeouts) added to make CI pass — if the fallback is
  flaky in CI, the probe logic is wrong, fix that.
- Do not touch `DEBOUNCE`, `TICK`, `IGNORED_DIRS`, or `is_ignored` — the
  event pipeline is not your surface. Do not weaken any existing test.

## 7. Acceptance — the keeper's log

- [ ] A fabricated suspension (pairs disagreeing by > 30 s) through the seam
      yields exactly one `toimija.watch.resumed`, one refresh, one
      `on_refresh`; jitter under threshold yields none; a backwards wall
      clock yields none, ever. (D8-1, D8-2, D8-6)
- [ ] A failed resume refresh warns in the established style and the loop
      keeps watching. (D8-7)
- [ ] On this Mac, `toimija watch` arms `(native)` with no periodic
      full-tree scan; forcing probe failure arms `(poll fallback)`; both
      lines appear only after a seen probe, and the structured armed event
      names the backend. (D8-3, D8-9)
- [ ] Every #144 armed assertion still proves "no armed before proof" in its
      updated labeled form.
- [ ] The lid-death test round-trips a dead-pid, never-closed marker into a
      history record with `end: reaped_stale` and the frozen heartbeat.
      (lands only after 0007 PR 1)
- [ ] `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` ·
      `cargo test` · `toimija doctor --strict` — green at every commit of
      all three PRs; no new dependencies in `Cargo.lock` beyond lockfile
      churn from nothing at all.

When every box ticks, the light survives the night. Sign the logbook with
the PRs.
