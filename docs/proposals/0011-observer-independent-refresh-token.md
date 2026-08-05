# Proposal 0011 — Freshness is a repository fact: the refresh token must be observer-independent

**Status:** ACCEPTED 2026-08-05 (Yair directed spec + fix in session
1785888321-93248).

**Date:** 2026-08-05

**Revises:** the packet `refresh_token` introduced with runtime-provider
surfacing (`feat(context): surface runtime providers`). No boundary edits —
everything here narrows what an existing synchronous check reads; no daemon,
no network, no dispatch.

---

## 1. The problem, observed live

A vartio agent working an unchanged repository was flooded with
`stale_packet` errors from the verify-before-read gate. Reproduction on the
same machine, zero Git changes between the two calls:

```
$ toimija verify --json                           → "status": "verified"
$ env PATH="/usr/bin:/bin" toimija verify --json  → "status": "stale_packet"
```

The Git consistency token (`toimija-consistency-v2`) matches in both runs.
The failure is the *second* check: `verify` recomputes the packet's
`refresh_token` and that token hashes, alongside the Git state, the
**runtime-provider discovery result and plugin warnings**. Provider discovery
is a function of the observing process's environment, not of the repository:

- `teko`/`kisko` discoverability comes from `which::which(id)` — the PATH of
  whichever process happens to run verify (`src/plugins.rs`).
- The `plugins.toml` location comes from `XDG_CONFIG_HOME`/`HOME`
  (`src/plugins.rs::user_config_path`).

A harness-spawned agent runs with a trimmed or different PATH than the shell
that compiled the packet, so every verify it issues reports `stale_packet`.
Worse, the error does not converge: refreshing under the agent's environment
bakes "provider unavailable" into the packet, which then reads as stale from
every *other* environment. The staleness ping-pongs between observers instead
of settling — the "a lot of stale errors" signature.

The root cause: an **environment-derived observation was folded into a
repository-state version token**, so "has the state changed?" no longer has
one answer. A consistency token must be a pure function of the shared state
it versions; two honest observers of the same repository must compute the
same version. The v2 content-based Git token honors this (sorted, byte-level,
observer-independent); the refresh token broke it.

## 2. The fix: hash repository state only

`context::refresh_token` drops the provider summary and plugin warnings from
its input. It hashes the (generated-output-filtered) Git state alone:
HEAD, branch, index, worktree, untracked, conflicts — plus the Git-*ref*
surface the consistency token deliberately omits (base, upstream, merge-base,
branch files, recent commits). All of that is repository state: any two
processes reading the same repo at the same moment compute the same token,
regardless of PATH, HOME, or config environment.

Ref movement staying in the token is deliberate. A sibling's `git fetch`
that moves `origin/main` shifts the merge-base and genuinely outdates the
packet's Branch surface and Compiled workset; flagging that as stale is
honest, deterministic, and cured by exactly one refresh — unlike provider
drift, it converges.

Runtime providers remain in the packet, informational, exactly as
`feat(context): surface runtime providers` intended: the packet still tells
the agent what was discoverable *when and where the packet was compiled*.
What changes is only that their drift no longer masquerades as repository
staleness.

The `stale_packet` message drops its "or runtime-provider" clause — after
this change it would be a lie.

## 3. What "stale" means now

> The repository — its objects, refs, index, worktree, or in-progress
> operation — differs from what the packet was compiled from.

Environment differences between observers can no longer produce staleness.
A packet compiled in one shell verifies fresh from any process on the same
repository until the repository itself moves.

## 4. Decisions

- **D11-1 — the refresh token is a pure function of repository state.**
  `refresh_token(git)` loses its provider and warning parameters;
  `live_refresh_token` no longer loads config or runs discovery. Anything
  that reads the process environment (PATH, HOME, XDG, config files outside
  the repo) is banned from the token's input, now and later.
- **D11-2 — provider drift is not staleness.** Providers stay in the packet
  as compile-time facts. Surfacing live drift at verify time as an advisory
  (never a fatal verdict) is explicitly deferred — a **non-goal** here.
- **D11-3 — ref movement stays fatal.** Fetch-driven merge-base movement
  keeps invalidating packets: it outdates the packet's branch surface, it is
  repository state, and one refresh settles it.
- **D11-4 — the contract is pinned by tests.** The unit test that asserted
  provider status changes the token now asserts the opposite; a CLI-level
  regression test compiles a packet under one provider environment, verifies
  it from another, and demands `verified`. The old CLI test that demanded
  `stale_packet` on provider drift is deleted as contradicting D11-2.
- **D11-5 — the watcher still tracks provider drift, locally.** Environment
  comparisons are only unsound *across* observers; a `watch` loop is one
  continuous observer comparing its own successive observations. The watcher
  therefore polls a composite of the repository refresh token plus a
  provider-summary fingerprint it computes itself (from the packet it just
  wrote and from a live probe), so the packet's Runtime providers section
  keeps refreshing on provider changes — without any environment fact ever
  entering the stored token that other processes verify against.
