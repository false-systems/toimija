# H. Proposed acceptance-test catalogue

Every JSON assertion below also checks schema/version, deterministic ordering,
and explicit capability coverage. Fixtures should be created through Git
plumbing/test helpers, not mocks alone.

## Git and readiness

1. **Partial staging:** Given a file with staged and unstaged changes, Toimija
   preserves both surfaces independently and never describes unstaged bytes as
   staged or branch intent. JSON references the same path with two layer IDs.
2. **Conflict stages:** Given a merge conflict with stages 1/2/3, preparation is
   `blocked`, JSON lists every stage OID/mode/path, and `run` does not execute a
   sentinel child without an explicit recovery override.
3. **Rebase:** Given a rebase stopped on a conflict, JSON reports operation kind,
   onto/original/stopped evidence when available; missing detail is degraded,
   not `none`.
4. **Detached CI:** Given detached HEAD plus an explicit base, JSON preserves
   detached state and base provenance without calling the diff “intent.”
5. **Divergence:** Given local branch two ahead/one behind, JSON reports exact
   counts and observed local ref OIDs, plus `remote_freshness: unknown`.
6. **Unborn/unrelated:** Given unborn or unrelated histories, no merge-base is
   a supported unknown state and no branch-wide negative claim is emitted.
7. **Renames/copies:** Given staged/unstaged renames and copies, old/new paths,
   scores, and layers survive JSON.
8. **Adversarial paths:** Spaces, tabs, newline, leading dash, Unicode, and Unix
   non-UTF-8 filenames round-trip with documented encoding and do not create
   fake Markdown lines or terminal escapes.
9. **Intent-to-add/index flags:** Intent-to-add, assume-unchanged,
   skip-worktree, and sparse checkout are reported or capability-degraded.
10. **Submodule:** Changing submodule HEAD, dirt, or index gitlink changes the
    consistency token and structured state.
11. **Worktree:** Two linked worktrees report distinct worktree/gitdir/index and
    shared repository/common-dir identities.
12. **Collection failure:** Force each Git stage to fail. JSON names the failed
    stage; no empty list is presented as proof of no changes.

## Artifact freshness and persistence

13. **Selected artifact:** With two session packets and a different repo-level
    mirror, ambient `verify --json` verifies only its own `TOIMIJA_LIVE_JSON` and
    returns path, artifact id, digest, and token.
14. **Race:** Mutate state between entry compile and verification. `run` retries
    once and either launches with a verified packet or refuses; it never merely
    logs and launches stale.
15. **Atomic pair:** Kill a writer at each write boundary. Readers observe either
    the previous complete pair or the next complete pair, never mixed/truncated
    artifacts.
16. **Linked-worktree refresh:** Commit/reset/rebase when Git metadata is outside
    the worktree root. The live packet refreshes or visibly enters degraded
    watcher status within a bounded interval.
17. **Snapshot isolation:** Leaked `GIT_DIR`, `GIT_WORK_TREE`, and
    `GIT_INDEX_FILE` cannot redirect snapshot objects/refs. Hostile filters are
    refused or require explicit unsafe authorization.

## Repository, task, and policy

18. **Mixed repo:** Root contains Rust workspace and nested Elixir umbrella.
    JSON lists every project root and both adapter coverages; validation includes
    both ecosystems without one overwriting the other.
19. **Rust workspace impact:** A shared crate change identifies dependent
    packages from native metadata and marks focused tests as partial.
20. **Elixir shared config:** Root config/mix lock change either maps to affected
    umbrella apps or explicitly falls back to umbrella-wide coverage.
21. **Nested policy:** Parent and directory-local `AGENTS.md` apply to a changed
    file with source/range/hash and deterministic precedence.
22. **Policy conflict:** Contradictory applicable instructions produce a
    blocking uncertainty, not concatenated prose.
23. **Task conflict:** User task says `src/a`; branch evidence suggests `src/b`.
    JSON preserves both, prefers explicit user scope, and flags out-of-scope dirt.
24. **Ownership/CI unknown:** Unsupported CODEOWNERS or CI syntax becomes
    degraded coverage; it never becomes “no owners/checks.”

## Context selection

25. **Budget determinism:** Same state, task, and byte budget produce identical
    selected item IDs/order/omissions across runs and machines (modulo documented
    path identity fields).
26. **Mandatory policy:** Applicable security/testing instruction is selected
    before optional history. If the budget cannot hold it, readiness is blocked
    or the omission is explicit; it is never silently cut.
27. **Relevant test omitted:** An adversarial large diff cannot hide the only
    companion test. JSON explains its score, selection or omission.
28. **Binary/large/generated:** Binary and oversized files are represented by
    metadata/digest and exclusion reason; generated/vendor classification cites
    evidence and remains overridable.
29. **Markdown/JSON agreement:** Budgeted Markdown item IDs and omission counts
    exactly match the corresponding selected JSON view.

## Validation and evidence

30. **Config composition:** Adding one custom privileged command does not erase
    inferred format/lint/test requirements; explicit replacement records what it
    replaced.
31. **Requirement trace:** Every planned command has requirement id, trigger,
    coverage, gaps, cwd, environment names, safety, cost, and authority.
32. **No shell injection:** Repository config containing `;`, `$()`, newline, or
    leading dash is never passed through a shell; unsafe command specs are
    rejected before execution.
33. **Result states:** Executor/import represents planned, attempted, passed,
    failed, skipped, blocked, unknown, cancelled, timed out, and stale distinctly.
34. **State binding:** A passing result against token A becomes stale after a
    relevant state change to token B; unrelated generated Toimija output does
    not invalidate it.
35. **Output/redaction:** Large output is bounded with digest/byte counts;
    configured secrets and environment values do not appear in JSON/logs.
36. **Mutating validation:** A command that changes tracked state records before
    and after tokens and cannot be reported as clean pass without that fact.

## Run, handoff, and coordination

37. **Pre-existing dirt:** Start with dirty file A; agent changes B. Handoff
    labels A pre-existing and B attributable/observed-during-run.
38. **Interrupted run:** Terminate after preparation and one failed command.
    Successor reads intact append-only events and sees exact incomplete status
    without an explicit final handoff.
39. **Truncated ledger:** A partial final JSONL line is diagnosed and ignored as
    incomplete while all prior records remain available.
40. **Cross-worktree visibility:** Two live linked worktrees see one another,
    their intents/scopes/tokens, without adding worktree files or assignments.
41. **Liveness honesty:** Dead PID, recycled PID, inaccessible PID, old heartbeat,
    and foreign host each return evidence and uncertainty appropriate to the
    observation; active is never inferred from PID alone.
42. **Malformed sibling:** One malformed marker creates a degraded warning while
    valid siblings remain listed; it is not silently absent.
43. **Handoff validation:** Handoff includes all required/recommended checks and
    their result states; focused checks are marked non-substitutive.
44. **Cross-machine future fixture:** Concurrent CAS writers to a local test ref
    preserve both immutable run records; a stale fetched ref reports its observed
    OID/time. No Toimija network operation occurs.

## Scale and portability

45. **Large untracked tree:** Synthetic large repo respects file/byte/time caps,
    returns promptly, and reports uninspected counts/coverage.
46. **Large binary change:** Hashing obeys resource policy without reading
    unbounded content into memory; freshness behavior is explicit.
47. **Missing tools:** Missing Git/Cargo/Mix produce environmental failure or
    unsupported validation, not empty success.
48. **Platforms:** Linux and macOS contract fixtures agree; Windows paths and
    unsupported process/watch semantics have explicit compatibility results.

