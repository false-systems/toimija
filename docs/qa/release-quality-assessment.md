# Toimija public-release quality assessment

Assessment date: 2026-07-15  
Branch: qa/public-release  
Baseline: origin/main at 4cccc24

## Verdict

**Test foundations exist but release is blocked.**

The repository has a substantial Rust unit and CLI suite, hermetic Git helpers,
atomic packet writes, a versioned consistency token, and useful regression
coverage. That is a foundation, not public-release qualification.

The baseline suite exposed an existing reliability problem:
tests/cli_watch.rs::watch_refreshes_on_a_file_change_then_stops failed once
after a 30-second event deadline and passed on immediate rerun. A flaky
freshness test is release-blocking because stale context is the product's
central safety failure.

## What is actually guaranteed today

- Git state is collected through Git subprocesses and rendered as Markdown or JSON.
- Staged, unstaged, untracked, and conflict paths have separate fields.
- Current packets carry a toimija-consistency-v2 token.
- current --verify refuses stale, corrupt, missing, or unsafe packets.
- Packet files use atomic replacement.
- Validation plans are rendered and are not executed by Toimija.
- The default suite is offline and uses temporary Git repositories.

## What is implied but not release-proven

- Watch mode eventually observes every relevant filesystem/Git event.
- Concurrent refreshes never produce a mixed Markdown/JSON state.
- All interrupted Git operations are detected, represented, and safety-gated.
- Worktree, sparse, shallow, submodule, and partial-clone semantics are complete.
- JSON output is a durable compatibility API.
- Hostile paths, symlinks, Git configuration, and terminal control bytes are safe.
- Release binaries work from a clean machine.
- Upgrade behaviour for prior generated state is defined.
- Performance and memory remain bounded on large repositories.

## Release blockers

1. Make the watch test deterministic and fix or explicitly bound the observed
   missed-event behaviour.
2. Complete the P0 state-preservation and stale-state matrix in
   docs/qa/test-matrix.md and tests/qa_p0.rs.
3. Define the JSON schema/version and stdout/stderr/exit-code contract.
4. Add property/fuzz coverage for Git output and path codecs.
5. Add release-mode package smoke tests and an upgrade policy.
6. Measure representative repositories before publishing performance claims.

## Highest-risk surfaces

refresh, current --verify, context --json, watch, and run can cause an agent to
trust stale or incomplete state. The highest-risk repository states are
partially staged files, interrupted operations, linked worktrees, hostile paths,
and concurrent writers.

## Recommended release boundary

First release only the local, single-worktree workflow on Linux/macOS with a
normal non-bare Git checkout: context, context --json, refresh,
current --verify --json, validate --plan, and doctor. Keep watch, run, adopt,
audit-plan, submodules, sparse/partial clones, bare repositories, Windows, and
upgrade compatibility explicitly experimental until their rows are green.
