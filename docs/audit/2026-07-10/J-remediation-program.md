# J. Remediation program

This is the execution plan for closing all 28 audit findings. It refines the
priority plan into issue-sized, dependency-ordered changes. The target is a
credible local-first v1, not a workflow platform.

## Program outcome

At the v1 gate, Toimija must be able to say one of three things about the exact
packet an agent will read:

- `ready`: required preparation stages completed with no blocking uncertainty;
- `ready_with_warnings`: safe to begin, with explicit bounded omissions;
- `blocked`: state is unsafe or required evidence is unavailable.

That verdict must bind repository/worktree state, task, applicable instructions,
context selection, validation requirements/results, and the artifact digest. A
warning printed somewhere is not a verdict.

## Non-negotiable rules

1. No negative claim from failed collection. Failure means degraded or blocked.
2. No renderer-invented semantics. Renderers display domain decisions.
3. No shell execution. Commands remain argv + cwd + declared environment names.
4. No silent compatibility break. Additive v1 changes are tested; breaking
   changes get a new schema or consistency algorithm.
5. No new platform: one crate, local files/Git, synchronous commands, no network,
   daemon, dispatcher, database, plugin system, or generic code index.
6. One behavioral promise per PR. Mechanical refactors ship only with the
   behavior that needs them.
7. Every PR adds the smallest failing acceptance fixture for its promise.

## Dependency graph

```text
M0 containment
  R01 exact-packet verify ─> R02 fail-safe run
  R03 validation composition
  R04 claim correction

M1 trustworthy substrate
  R05 hardened Git execution ─> R06 atomic artifacts
  R07 byte-safe command output ─> R08 lossless status/diff paths
                                  └> R09 complete Git/worktree state
  R10 explicit collection coverage ───────────────┘
  R09 ─> R11 branch-state honesty ─> R12 live worktree refresh

M2 preparation contract
  R10 ─> R13 PreparationReport/readiness
  R08 ─> R14 context selection
  R13 ─> R15 task contract
  R13 ─> R16 instruction resolver
  R10 + R13 ─> R17 multi-root projects
  R16 + R17 ─> R18 ownership/CI/generated evidence

M3 validation evidence
  R03 + R13 + R17 ─> R19 validation requirements
  R19 ─> R20 result import ─> R21 optional executor

M4 recovery and coordination
  R13 + R14 + R20 ─> R22 run ledger ─> R23 evidence handoff
  R09 + R22 ─> R24 cross-worktree awareness

M5 contract, scale, release
  R13..R24 ─> R25 API schemas/migrations
  R09 + R14 + R17 ─> R26 scale limits
  R25 + R26 ─> R27 v1 release gate

Deferred after v1 proof
  R24 + explicit proposal ─> R28 cross-machine Git-ref exchange
```

R01–R04 are hotfixes and should land before broader refactoring. R05–R12 make
the underlying claims trustworthy. R13 is the seam after which preparation—not
individual commands—becomes the product model.

## Milestone gates

| Milestone | Exit condition | Product claim allowed |
|---|---|---|
| M0 — containment | R01–R04 merged | “Git context helper with guarded launch”; no “real gates” claim |
| M1 — trustworthy substrate | R05–R12 merged | Supported Git states are lossless; unsupported states are explicit |
| M2 — preparation contract | R13–R18 merged | “Prepares local Rust/Elixir repositories” with stated coverage |
| M3 — validation evidence | R19–R21 merged | Planned/attempted/passed/failed/stale are distinguishable |
| M4 — recovery | R22–R24 merged | Interrupted and cross-worktree local runs can be continued honestly |
| M5 — v1 | R25–R27 merged | “Dependable agent-preparation tool” for the documented v1 boundary |

No milestone advances while a critical finding assigned to it remains open.

## Work packages

### R01 — Verify the exact packet selected by the agent

- **Findings:** TA-001.
- **Promise:** verification proves the artifact the caller will read.
- **Change:** select packet in this order: explicit `--packet`, valid ambient
  `TOIMIJA_LIVE_JSON`, repository `current.json`. Canonicalize and require it to
  be either the repository packet or below the validated ambient session
  directory. Return `path`, `artifact_id`/sauma digest, algorithm, and token in
  JSON. Reuse `session::ambient_session`; do not add a resolver framework.
- **Files:** `src/cli.rs`, `src/session.rs`, `tests/cli_verify.rs`, README.
- **Compatibility:** repository-level behavior remains the fallback. Additive
  verifier JSON fields only.
- **Acceptance:** two valid session packets plus a different repo mirror; each
  ambient process verifies only its own file. Traversal and unrelated absolute
  paths fail. Tampering with the selected file fails even if repo-level JSON is
  valid.
- **Size/dependency:** small; none.

### R02 — Refuse unsafe or unverifiable `run` entry

- **Findings:** TA-002.
- **Promise:** a child never starts from a known unsafe/unverified entry packet.
- **Change:** extract the existing entry verification into a result-returning
  helper. Retry exactly once on stale race by rebuilding. Block conflicts,
  in-flight operations, unsupported algorithms, collection failure, and a
  second stale result. Add `--allow-unsafe-git-state <reason>` only if recovery
  work is a demonstrated need; otherwise omit the override in the first PR.
- **Files:** `src/cli.rs`, `tests/cli_run.rs`, README.
- **Acceptance:** sentinel child is not created in merge, rebase, conflict,
  unsupported, or repeated-race fixtures; ordinary run still launches.
- **Size/dependency:** small; R01 preferred but not technically required.

### R03 — Make configured validation additive by default

- **Findings:** TA-016 and immediate part of TA-028.
- **Promise:** adding one configured check cannot silently remove standard checks.
- **Change:** merge configured tiers with inferred defaults, stable-deduped. Add
  explicit `replace_inferred: true` at the validation section only if replacement
  is needed. Old config stays additive. Render source as `inferred`, `repo_config`,
  or `inferred_and_repo_config`.
- **Files:** `src/config.rs`, `src/validate.rs`, render/tests, README,
  `.toimija/repo.yaml` only if its shape changes.
- **Acceptance:** one configured privileged command preserves Rust/Elixir fast,
  strict, and full defaults; explicit replacement is visible in JSON/Markdown.
- **Size/dependency:** small; none.

### R04 — Correct claims before capabilities arrive

- **Findings:** TA-006, TA-013, TA-028.
- **Promise:** documentation and rendering never claim intent, skippability, or
  complete validation that current evidence cannot prove.
- **Change:** rename `Branch intent` to `Branch surface`; change mechanical
  ripple wording to “possible dependency-related change—inspect after manifest”;
  replace “real gates”/“prepared” claims with current bounded wording. Keep JSON
  compatibility by retaining existing field names until R11/R25.
- **Files:** `src/render.rs`, README, snapshots/tests.
- **Acceptance:** no user-facing current-mode text calls a merge-base diff intent,
  tells agents to skip source, or calls inferred commands complete gates.
- **Size/dependency:** small; none.

### R05 — Route every Git invocation through the hardened runner

- **Findings:** TA-024 and part of TA-025.
- **Promise:** cwd is authoritative for all Git reads and writes.
- **Change:** extend `CommandRunner` minimally to support extra environment
  assignments needed by snapshot while always removing repo-locating Git vars.
  Replace raw `Command::new("git")` in snapshot. Do not build a generic process
  framework.
- **Files:** `src/command.rs`, `src/snapshot.rs`, `tests/git_isolation.rs`,
  snapshot tests.
- **Acceptance:** leaked location variables cannot redirect snapshot objects,
  refs, or scratch index; existing snapshot behavior remains.
- **Size/dependency:** medium; none.

### R06 — Atomically replace generated artifacts

- **Findings:** TA-023.
- **Promise:** readers observe an old complete artifact or a new complete
  artifact, never a torn file.
- **Change:** one private stdlib helper writes a same-directory temporary file,
  `sync_all`s where supported/required by the durability contract, and renames.
  Use it for packet JSON/Markdown and session markers. Give packet pairs one
  shared `artifact_id`; JSON is authoritative if pair replacement is interrupted.
- **Files:** `src/refresh.rs`, `src/session.rs`, perhaps `src/lib.rs`; filesystem
  integration tests.
- **Acceptance:** injected failure before rename preserves old valid JSON; marker
  never parses partially; Markdown/JSON expose matching artifact id after success.
- **Size/dependency:** medium; R05 independent.

### R07 — Preserve command stdout as bytes

- **Findings:** prerequisite for TA-003/TA-027.
- **Promise:** Git evidence is not mutated by UTF-8 replacement.
- **Change:** `CommandOutput.stdout/stderr` become bytes. Add explicit lossy text
  helpers only for human diagnostics and commands whose contracts are UTF-8.
  Update fake runners mechanically in the same PR.
- **Files:** `src/command.rs` and callers/tests.
- **Compatibility:** internal only; JSON unchanged.
- **Acceptance:** invalid UTF-8 bytes survive runner round trip; diagnostic helper
  is explicitly lossy.
- **Size/dependency:** medium; none.

### R08 — Parse status and diffs losslessly with `-z`

- **Findings:** TA-003.
- **Promise:** every Git path is preserved without whitespace/quoting ambiguity.
- **Change:** use porcelain v2 `-z` and name-status `-z`; parse byte fields,
  including original/new rename paths and unmerged stages. Introduce a small
  `RepoPath` wrapper only if necessary for byte-safe ordering/serialization;
  otherwise use `OsString` internally. JSON encoding decision: `{ "utf8": ... }`
  when valid, `{ "base64": ... }` on Unix when invalid; do not add a base64
  dependency unless stdlib-free encoding is demonstrably worse—hex is acceptable.
- **Files:** `src/git.rs`, `src/context.rs`, render/DTO/tests.
- **Acceptance:** H-8 fixture set plus rename/copy/conflict stages round-trip and
  render safely.
- **Size/dependency:** medium; R07.

### R09 — Complete the supported local Git/worktree state

- **Findings:** TA-004, TA-007, TA-008.
- **Promise:** freshness changes for every supported state transition; unsupported
  advanced state is explicit.
- **Change:** add worktree gitdir/common-dir, porcelain submodule flags, gitlink
  index/worktree OIDs, sparse-checkout status, intent-to-add, skip-worktree and
  assume-unchanged observations, ahead/behind, upstream/ref OIDs. Bump consistency
  to v3. Avoid scanning ignored files; report their coverage policy instead.
- **Files:** `src/git.rs`, `src/consistency.rs`, context/render, Git fixtures.
- **Acceptance:** H-5, H-9, H-10, H-11; each material state transition changes
  v3 token or produces degraded/blocked coverage.
- **Size/dependency:** large; R08 and R10 data shape agreed.

### R10 — Represent collection coverage and failure

- **Findings:** TA-005 and foundation for TA-014/TA-026.
- **Promise:** empty evidence means a successful empty observation.
- **Change:** add small `Coverage { status, reason, source }` values for Git
  branch diff, history relevance, session scan, repository projects,
  instructions, ownership/CI, and validation discovery. Do not wrap every value
  in a generic abstraction; attach coverage at stage boundaries. Replace
  `run_git_ok` only where failure affects a product claim.
- **Files:** `src/git.rs`, `src/relevance.rs`, `src/sessions.rs`, `src/context.rs`,
  render/tests.
- **Acceptance:** injected spawn/exit/parse/read errors are distinct from empty
  results in both Markdown and JSON; malformed sibling is visible.
- **Size/dependency:** medium; none, but land before R09/R13.

### R11 — Model branch facts without calling them intent

- **Findings:** TA-006, TA-007.
- **Promise:** branch graph facts, base choice, and task intent cannot be confused.
- **Change:** `BranchState` groups ref/OID/upstream/ahead/behind/base candidate/
  merge-base/committed surface and provenance. Base source enum: explicit config,
  remote HEAD, common-name fallback, unavailable. Remote freshness is always
  `unknown_without_fetch` because Toimija does not fetch.
- **Files:** `src/git.rs`, `src/context.rs`, render/JSON/tests.
- **Acceptance:** stacked, develop-trunk, local-only, detached, shallow, and stale
  remote-tracking fixtures state only proven facts.
- **Size/dependency:** medium; R09.

### R12 — Refresh on linked-worktree Git changes

- **Findings:** TA-022.
- **Promise:** live packet freshness covers worktree files and its actual Git
  metadata locations.
- **Change:** watch repo root plus resolved worktree gitdir. Watch common dir only
  for refs that can affect the current worktree, or use a cheap periodic token
  verification if platform watcher semantics make selective watching unreliable.
  Persist watcher degradation in the session packet; stderr is supplemental.
- **Files:** `src/watch.rs`, `src/git.rs`, refresh/session integration tests.
- **Acceptance:** commit/reset/rebase in linked worktree refreshes within a
  bounded test interval; backend error changes coverage state.
- **Size/dependency:** medium; R09.

### R13 — Introduce `PreparationReport` and readiness

- **Findings:** systemic risk behind TA-002, TA-005, TA-009–TA-017, TA-027.
- **Promise:** one domain result composes facts, coverage, uncertainties, and
  readiness before rendering.
- **Change:** add `PreparationReport` containing existing packet evidence plus
  `readiness`, `coverage`, and `uncertainties`. Initially adapt existing fields;
  do not rewrite every module. `ContextPacket` becomes the v1 projection. Rules:
  unsafe Git or required-stage failure = blocked; optional degraded stage =
  ready-with-warnings; otherwise ready.
- **Files:** new `src/preparation.rs`, `src/context.rs`, render/CLI/tests.
- **Acceptance:** table-driven readiness tests cover every blocking/degraded
  input; renderers contain no independent readiness rules.
- **Size/dependency:** medium; R10 and M0.

### R14 — Select context items before rendering

- **Findings:** TA-012, TA-013.
- **Promise:** every omission is explicit and reproducible.
- **Change:** create `ContextItem { id, kind, source, mandatory, priority,
  estimated_bytes }` and `ContextSelection`. Use UTF-8 byte budget for v1.
  Reuse current ordering as initial priorities, but mandatory safety/task/policy
  items cannot be displaced. Markdown and budgeted JSON project the same
  selection. Keep an explicit unbudgeted evidence command/path if needed.
- **Files:** context/relevance/render/tests.
- **Acceptance:** H-25–H-29; no tail truncation is the primary selection method.
- **Size/dependency:** medium; R10/R13.

### R15 — Make task scope first-class

- **Findings:** TA-011 and intent half of TA-006.
- **Promise:** explicit user task is separate from inferred scope and branch facts.
- **Change:** `Task { objective, constraints, requested_scope, exclusions,
  sources, open_questions }`. Seed from `run --intent/--scope`; preserve unknown.
  Add branch/commit-derived candidates only as inference with confidence; do not
  integrate GitHub/network metadata. Flag changed files outside explicit scope.
- **Files:** session/context/preparation/render/tests.
- **Acceptance:** explicit task wins; contradictory evidence remains; unknown
  task downgrades readiness according to command mode, not necessarily blocks
  read-only `context`.
- **Size/dependency:** medium; R11/R13.

### R16 — Resolve applicable repository instructions

- **Findings:** TA-010 and part of TA-025.
- **Promise:** each changed/task path has a traceable instruction set or explicit
  coverage gap.
- **Change:** discover root and nested `AGENTS.md`, `CLAUDE.md`, configured docs,
  `CONTRIBUTING*`, `SECURITY*`; resolve parent-to-child scope. Record source path,
  content hash, scope root, precedence, and conflicts. Do not parse arbitrary
  natural-language semantics beyond document applicability; conflicting text is
  surfaced for the agent/human.
- **Files:** new `src/instructions.rs`, config/preparation/context/tests.
- **Acceptance:** H-21/H-22 plus unreadable/symlink/oversized/control-character
  fixtures. Contents remain untrusted and escaped at render time.
- **Size/dependency:** medium; R13, R08 path handling.

### R17 — Inventory multiple project roots

- **Findings:** TA-009.
- **Promise:** Rust and Elixir roots coexist; no language silently wins.
- **Change:** `ProjectRoot { path, kinds, evidence, coverage }`; bounded discovery
  from tracked manifests. Rust uses `cargo metadata --no-deps` only after command
  safety review, otherwise current manifest walk; Elixir inventories umbrella
  apps from manifests. Merge adapter outputs rather than selecting one box.
- **Files:** repo/adapters/context/validate/tests.
- **Acceptance:** H-18–H-20, nested and mixed fixtures, deterministic ordering.
- **Size/dependency:** large; R10/R13.

### R18 — Discover ownership, CI, generated, and boundary evidence

- **Findings:** TA-014.
- **Promise:** supported local governance evidence is included; unsupported
  syntax is not treated as absence.
- **Change:** read CODEOWNERS-like files with a deliberately documented subset;
  inventory local CI workflow command references without claiming semantic
  execution coverage; classify generated/vendor paths from config plus common
  native markers; map configured boundaries/instructions to project/path scope.
  Separate readers, one PR each if any exceeds medium size.
- **Files:** new small discovery modules, preparation/config/tests.
- **Acceptance:** H-24 and fixtures for unsupported syntax/deleted sources.
- **Size/dependency:** large umbrella, preferably 3 small PRs; R16/R17.

### R19 — Replace validation strings with requirements and command specs

- **Findings:** TA-015, TA-019.
- **Promise:** every check states why, where, how safely, and what it does not
  prove.
- **Change:** `ValidationRequirement` and `CommandSpec { argv, cwd, env_names,
  safety, mutates_repo, cost }`; levels remain focused/fast/strict/full/
  privileged. Adapters/config emit requirements. Renderer joins argv for display
  only. Handoff stores requirement IDs and all levels.
- **Files:** validate/config/adapters/render/handoff/schemas/tests.
- **Acceptance:** H-30/H-31; focused never substitutes for required full checks.
- **Size/dependency:** large; R03/R13/R17.

### R20 — Import validation results

- **Findings:** TA-017.
- **Promise:** external execution can produce trustworthy evidence without making
  Toimija a runner.
- **Change:** `toimija validate record` accepts requirement ID, status, exit or
  skip/block reason, timestamps, tool version, bounded output file/digest, and
  binds it to the current token. Validate schema and refuse secret environment
  capture. Result status includes planned/attempted/passed/failed/skipped/
  blocked/unknown/cancelled/timed_out/stale.
- **Files:** validate/evidence/CLI/schema/tests.
- **Acceptance:** H-33–H-35 and stale-token transition.
- **Size/dependency:** medium; R19.

### R21 — Add the smallest optional validation executor

- **Findings:** remaining TA-017 plus execution trust boundary.
- **Promise:** opted-in safe command specs can be run and recorded without shell
  or orchestration.
- **Change:** `toimija validate --run <requirement-id>`; argv-only, cwd confined
  to repo, declared environment allowlist, timeout/cancellation, bounded
  stdout/stderr, before/after tokens. Privileged/unsafe/mutating specs require
  explicit confirmation or remain record-only. No retries or DAG scheduler.
- **Files:** validate/command/evidence/CLI/tests/security docs.
- **Acceptance:** H-32–H-36.
- **Size/dependency:** medium; R19/R20.

### R22 — Append a minimal AgentRun ledger

- **Findings:** TA-020, foundation for TA-018/TA-021.
- **Promise:** interruption leaves enough durable evidence to resume.
- **Change:** `.toimija/runs/<id>.jsonl` events for opened, task declared,
  preparation compiled, selection made, validation recorded, handoff, closed.
  Reuse session ID initially; optional parent run. Append one line with a local
  lock; detect and report truncated final line. Do not store prompt/source/output
  content by default.
- **Files:** new `src/run_ledger.rs`, session/CLI/schema/tests.
- **Acceptance:** H-38/H-39; secret scan of fixture ledger remains clean.
- **Size/dependency:** medium; R13/R14/R20.

### R23 — Derive handoff from run evidence and entry baseline

- **Findings:** TA-018, TA-019.
- **Promise:** handoff distinguishes pre-existing state from observed run delta
  and validation evidence.
- **Change:** record entry token/HEAD/layered dirt at run open; compute exit
  comparison; include assumptions, incomplete status, omissions, all validation
  requirement/results, and next step. Keep current v1 handoff readable; emit v2
  for breaking shape.
- **Files:** handoff/session/run ledger/schemas/tests.
- **Acceptance:** H-37/H-43; closed/interrupted runs both produce honest view.
- **Size/dependency:** medium; R22/R19/R20.

### R24 — Make local session awareness cross-worktree and evidence-based

- **Findings:** TA-021.
- **Promise:** linked worktrees see one another and liveness never pretends to be
  stronger than its evidence.
- **Change:** store coordination markers/index below Git common dir, while live
  packets remain per worktree. Add host/boot-or-process-start identity and
  liveness evidence fields. Treat inaccessible PID and stale heartbeat
  distinctly. Never delete a session on uncertain liveness.
- **Files:** session/sessions/run ledger/tests/docs.
- **Acceptance:** H-40–H-42; no worktree dirt.
- **Size/dependency:** medium; R09/R22.

### R25 — Publish deliberate JSON contracts and migrations

- **Findings:** TA-027 and contract parts of TA-003.
- **Promise:** agent-facing JSON is versioned independently of internal structs.
- **Change:** explicit DTO modules and schemas for preparation/context, verify,
  sessions, validation plan/result, run event, snapshot, and handoff. Standard
  error envelope, timestamps, IDs, path encoding, coverage, uncertainty, and
  additive-enum policy. Existing `toimija-context.v1` gets additive compatibility
  where possible; otherwise ship v2 with a migration note. Keep sauma law.
- **Files:** DTO/schema/docs/tests; avoid duplicating business logic.
- **Acceptance:** schema/emitter agreement, golden fixtures, unknown additive
  fields/enums policy, Unicode/non-UTF-8/Windows path fixtures.
- **Size/dependency:** large; shapes from R13–R24 must stabilize first.

### R26 — Bound cost and expose scale degradation

- **Findings:** TA-026.
- **Promise:** large repositories finish within declared resource limits or say
  precisely which evidence was not collected.
- **Change:** stage timing/count/byte accounting; limits for untracked enumeration,
  content hashing, history, instruction files, and rendered JSON. Hash streams
  instead of loading whole files. Cache only immutable Git facts keyed by OID;
  do not cache mutable worktree truth in v1.
- **Files:** git/consistency/relevance/instructions/preparation/logging/tests.
- **Acceptance:** H-45/H-46 with deterministic degraded coverage.
- **Size/dependency:** medium; R09/R14/R17.

### R27 — v1 release and operational gate

- **Findings:** TA-025, TA-028 and packaging portion of the matrix.
- **Promise:** documented claims match tested supported environments.
- **Change:** threat model/trust boundaries, supported Git/repo states,
  Linux/macOS install smoke tests, explicit Windows status, release checksums,
  schema compatibility check, performance envelope, and end-to-end preparation
  fixtures for the 15 audited scenarios.
- **Files:** README/docs/workflows/release tests.
- **Acceptance:** every v1 claim links to an automated acceptance test; all
  critical/high findings are closed or explicitly outside the v1 boundary with
  honest blocking/degraded behavior.
- **Size/dependency:** medium; R25/R26.

### R28 — Cross-machine Git-ref exchange (deferred)

- **Findings:** future part of TA-021/scenario 11; not required for local v1.
- **Promise:** none until separately approved.
- **Gate:** R24 must be proven in real use; manual Git transport must have a
  demonstrated pain; proposal 0004 concurrency/retention/freshness questions
  must be resolved. Still no Toimija network call.
- **Acceptance when activated:** H-44 plus concurrent compare-and-swap writers,
  stale fetch evidence, privacy review, and zero worktree files.

## Finding coverage

| Finding | Primary work package(s) | Closed at |
|---|---|---|
| TA-001 wrong packet verification | R01 | M0 |
| TA-002 unsafe launch | R02, R13 | M0/M2 |
| TA-003 lossy paths | R07, R08, R25 | M1/M5 |
| TA-004 submodule freshness | R09 | M1 |
| TA-005 failure looks empty | R10, R13 | M1/M2 |
| TA-006 diff called intent | R04, R11, R15 | M0/M2 |
| TA-007 divergence/freshness | R09, R11 | M1 |
| TA-008 advanced Git states | R09 | M1 |
| TA-009 singular repo kind | R17 | M2 |
| TA-010 unresolved instructions | R16 | M2 |
| TA-011 no task contract | R15 | M2 |
| TA-012 tail truncation | R14 | M2 |
| TA-013 ripple demotion | R04, R14 | M0/M2 |
| TA-014 ownership/CI/boundaries | R18 | M2 |
| TA-015 command-string validation | R19 | M3 |
| TA-016 config erases defaults | R03, R19 | M0/M3 |
| TA-017 no result evidence | R20, R21 | M3 |
| TA-018 unattributable handoff | R22, R23 | M4 |
| TA-019 handoff omits tiers | R19, R23 | M3/M4 |
| TA-020 no run ledger | R22 | M4 |
| TA-021 session/worktree liveness | R24; R28 deferred cross-machine | M4 |
| TA-022 linked-worktree refresh | R12 | M1 |
| TA-023 torn persistence | R06 | M1 |
| TA-024 snapshot bypass | R05 | M1 |
| TA-025 unsanitized/untrusted inputs | R08, R16, R21, R27 | M1–M5 |
| TA-026 scale exhaustion | R26 | M5 |
| TA-027 accidental JSON API | R25 | M5 |
| TA-028 overstated claims | R04, R27 | M0/M5 |

Every finding has a primary closure path. R28 is the only deliberately deferred
capability, and v1 must say cross-machine coverage is absent rather than imply it.

## Recommended PR order

The merge queue should follow this order unless a failing production case
justifies moving an independent item earlier:

1. R01 exact-packet verify
2. R02 fail-safe run
3. R03 validation composition
4. R04 claim correction
5. R05 hardened Git execution
6. R06 atomic artifacts
7. R10 collection coverage
8. R07 byte output
9. R08 lossless paths
10. R09 complete Git/worktree state
11. R11 branch state
12. R12 linked-worktree refresh
13. R13 PreparationReport
14. R14 context selection
15. R15 task contract
16. R16 instructions
17. R17 multi-root projects
18. R18 governance evidence, split into ownership / CI / generated-boundaries
19. R19 validation requirements
20. R20 result import
21. R21 optional executor
22. R22 run ledger
23. R23 handoff v2
24. R24 cross-worktree sessions
25. R26 scale limits
26. R25 API contract freeze
27. R27 v1 gate

R25 comes late because freezing DTOs before the domain is stable would create
compatibility work with no user value. R01 adds only the minimum verifier fields
needed immediately and carries them forward.

## Per-PR gate

Every PR must include:

- one issue with finding IDs and one behavioral promise;
- before/after externally observable example;
- focused acceptance fixture plus relevant regression suite;
- JSON compatibility decision (`additive`, `new schema`, or `internal only`);
- trust-boundary and failure-mode note;
- README update when behavior/claim changes;
- `cargo fmt`, `cargo test`, `cargo clippy --all-targets -- -D warnings`;
- `toimija doctor --strict` when config, boundaries, major docs, or structure move;
- rebase on current `origin/main` immediately before PR/merge.

An issue is not complete because its happy-path type exists. It is complete when
the named false-confidence scenario cannot recur and the machine output proves
why.

## Release-blocking test suites

### M0 suite

- verifier selects exact session artifact;
- unsafe/raced entry never launches sentinel child;
- configured validation cannot erase inferred requirements;
- current claims use evidence-appropriate wording.

### M1 suite

- adversarial byte paths and all index layers;
- submodule/sparse/worktree/operation token transitions;
- forced collection failures;
- linked-worktree metadata refresh;
- atomic write crash points and leaked Git environment.

### M2 suite

- readiness truth table;
- mixed Rust/Elixir and nested projects;
- nested/conflicting instructions;
- explicit/unknown/conflicting task;
- budget selection/omissions;
- ownership/CI unsupported syntax.

### M3 suite

- validation requirement trace and config composition;
- record/import status matrix and stale evidence;
- argv-only execution, timeout, cancellation, output bounds, redaction, and
  mutating-command before/after tokens.

### M4 suite

- interrupted/truncated ledger;
- pre-existing vs run-observed dirt;
- evidence-derived handoff;
- two linked worktrees and uncertain liveness.

### M5 suite

- all 15 scenario audits as end-to-end fixtures;
- schema/golden compatibility;
- large untracked/binary repository limits;
- Linux/macOS installation and smoke runs;
- documentation-claim test mapping.

## Migration strategy

### Context packet

Keep `toimija-context.v1` additive through M2 if possible: add readiness,
coverage, uncertainty, selection, task, instructions, and projects while
retaining old fields. Mark legacy aggregate fields deprecated in docs. If
byte-safe paths cannot coexist with current path strings without ambiguity,
introduce `toimija-context.v2` at R25 rather than silently changing v1.

### Consistency token

R09 emits `toimija-consistency-v3`. Verifier keeps recognizing v2 as supported
only for legacy packets and must not call v2 complete for submodule/advanced
states. New packets always use v3. No token is rewritten in place.

### Validation config

Existing lists continue to parse and become additive. Structured command specs
are optional new syntax. Renderer preserves familiar command output. Explicit
replacement is opt-in and visible.

### Handoff and sessions

Readers retain v1 handoff support. R23 emits v2 because attribution/results are
semantic, not merely additive decoration. Session marker additions remain
additive until R24 changes placement; migration reads both locations for one
release and writes only the common-dir location.

## Risks and controls

| Risk | Control |
|---|---|
| Domain rewrite stalls critical fixes | Land R01–R04 without waiting for `PreparationReport` |
| Byte-safe paths explode API complexity | Keep byte handling internal until R25; one documented encoding |
| Git completeness becomes endless | Define supported v1 states; unsupported states degrade/block honestly |
| Native metadata executes hostile repo behavior | Safety review and opt-in; retain marker/path fallback |
| Validation executor becomes a task runner | One requirement ID per invocation; no scheduler, retries, DAG, or shell |
| Ledger stores secrets/source | Metadata/digests by default; bounded explicit payloads; redaction tests |
| New coverage warnings make tool noisy | Structured coverage with concise rendering; machines retain full detail |
| API freezes too early | Freeze schemas after domain milestones, except urgent additive verifier fields |
| Cross-machine design distracts core | R28 has a hard post-v1 activation gate |

## Issue template

```markdown
Title: <behavioral promise>

Audit findings: TA-...
Milestone / package: M... / R...

Current false-confidence scenario:
<one concrete externally observable failure>

Required behavior:
<one promise>

In scope:
- ...

Out of scope:
- ...

Compatibility:
- JSON/schema: additive | new version | internal only
- config/data migration: ...

Acceptance:
- Given ... when ... then ...
- Failure/degraded case ...

Required tests:
- fixture ...
- regression ...

Verification:
- cargo fmt
- cargo test
- cargo clippy --all-targets -- -D warnings
```

## First action

Open and implement R01 only. It is the smallest critical root-cause fix, has no
architectural dependency, and immediately makes the verify-before-read contract
true for concurrent wrapped agents. Do not begin `PreparationReport`, the
executor, or the ledger until M0 is green.

