# Performance and scale plan

## Baseline captured on 2026-07-15

Environment: local macOS worktree, stable toolchain, warm Cargo artifacts.

- cargo test --quiet: first run took 61.14s wall because the existing watcher
  test waited 30s after a missed event and failed; immediate isolated rerun
  passed in 1.52s.
- Release build: 14.52s wall, 4.2 MiB target/release/toimija.
- Release toimija --version: below /usr/bin/time's 10ms display resolution
  (median of 10 runs after 3 warmups, per the method below). A single earlier
  cold run showed 0.29s wall — first-execution overhead (macOS Gatekeeper
  scan), not steady-state startup; kept here as a reminder that one-run
  numbers lie.
- Release toimija --help: below the timer's displayed resolution.

The full-suite number is not a performance claim; it is evidence that the
current watcher test can dominate CI time when it flakes.

## Method

Use release binaries, a dedicated temporary filesystem, isolated Git config,
fixed locale/timezone, and three warmup plus ten measured process runs. Record
OS, CPU, RAM, Rust/Git versions, repository generator seed, file count, changed
count, untracked count, and output bytes. Report median and p95, not one run.

Measure cold/warm startup, discovery, Git collection, context JSON, refresh,
verify, validation-plan generation, watcher update latency, watcher idle CPU,
peak RSS, child process count, and external Git command count. Watch and run
now use a 500 ms full-tree polling scan; the Large and Pathological profiles
must measure whether scan time exceeds that interval and whether the 5-second
startup probe deadline can false-fail on a large initial scan. Keep correctness
and benchmark jobs separate.

The current polling backend is a reliability baseline. Revisit native events
plus a polling catch-up tick only after these measurements establish a safe
freshness margin.

## Profiles

| Profile | Shape |
|---|---|
| Tiny | 20 files, one project, one change |
| Small | 1,000 files, several packages, 20 changes |
| Medium | 25,000 files, mixed roots, 100 changes, 500 untracked |
| Large | 100,000 files, many roots, 1,000 changes |
| Pathological | 250,000 files, deep paths, many refs, giant manifests |

Each profile is tested clean, dirty, conflict, linked-worktree, and
mixed-language where applicable. Generated repositories stay out of Git.

## Initial release policy

Do not publish absolute latency budgets before the first stable profile run.
After measurement, propose an absolute ceiling and a relative regression gate:
fail on a meaningful >20% p95 regression over three baseline samples, warn on
smaller changes, and retain raw benchmark artifacts. Watch idle CPU and event
latency get separate thresholds.
