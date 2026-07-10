# G. Prioritized implementation plan

Each item is independently mergeable unless a dependency is named.

## P0 — correctness and honesty

### P0.1 Verify the artifact actually read — small

- **Problem/consequence:** `verify` proves repo-level JSON, not the authoritative
  session packet; concurrent agents can trust the wrong artifact.
- **Change/modules/dependencies:** resolve ambient or `--packet` path in
  `cli`, `session`, `refresh`; no dependency.
- **Acceptance/tests:** JSON returns verified path/digest/token; two session
  packets and a different repo mirror prove no cross-verification.

### P0.2 Make unsafe launch fail closed — small

- **Problem/consequence:** stale/unsafe entry still launches an agent.
- **Change:** retry a stale race once; refuse unsafe/unverifiable/unsupported;
  explicit override with reason. `cli`, `consistency`.
- **Acceptance/tests:** merge/rebase/conflict and raced packet do not execute the
  child; override is visible in run evidence.

### P0.3 Lossless Git paths and status — medium

- **Problem/consequence:** unusual paths are split/lossy.
- **Change:** byte output runner, porcelain/name-status `-z`, old/new rename
  paths, index stages. `command`, `git`, DTO path encoding.
- **Dependencies:** API path rule.
- **Acceptance/tests:** spaces, tabs, newlines, leading dash, Unicode and Unix
  non-UTF-8 fixtures round-trip.

### P0.4 Close freshness gaps — medium

- **Problem/consequence:** submodule/sparse/index state can be omitted.
- **Change:** capture submodule OIDs/dirt, sparse/index flags, git/common dirs;
  bump consistency algorithm. `git`, `consistency`.
- **Dependencies:** P0.3.
- **Acceptance/tests:** every relevant transition changes token; unsupported
  states degrade instead of verifying.

### P0.5 Preserve collection failures — medium

- **Problem/consequence:** errors become empty evidence.
- **Change:** `StageResult<T>` and CapabilityCoverage; propagate into packet and
  JSON. `git`, `relevance`, `sessions`, `context`.
- **Acceptance/tests:** forced command/parser/read failure yields degraded stage
  and no “none/0” negative claim.

### P0.6 Separate branch surface from intent — small

- **Problem/consequence:** guessed base diff is called intent.
- **Change:** rename output/type; add base provenance, ahead/behind and remote
  observation honesty. `git`, `context`, `render`, README.
- **Acceptance/tests:** stacked/local/detached cases never claim inferred intent.

### P0.7 Structured validation requirements — medium

- **Problem/consequence:** command strings imply unexplained completeness.
- **Change:** requirement ID, trigger/evidence, coverage, cwd, environment,
  safety, cost, authority; explicit config extend/replace. `validate`, `config`,
  adapters, render.
- **Acceptance/tests:** one configured privileged command does not erase inferred
  tests; every command traces to a requirement and states omissions.

### P0.8 Context selection with omissions — medium

- **Problem/consequence:** tail truncation can hide mandatory context.
- **Change:** ContextItem/Selection, deterministic byte budget, mandatory items,
  omission manifest. `context`, `relevance`, `render`.
- **Dependencies:** P0.5; instruction items can join later.
- **Acceptance/tests:** same state/budget gives same IDs; relevant policy/test is
  retained or explicitly omitted; JSON and Markdown agree.

### P0.9 Harden persistence and Git execution — small

- **Problem/consequence:** torn artifacts and snapshot env redirection/filter
  trust are unsafe.
- **Change:** atomic replace; route snapshot through hardened runner; sanitize
  control characters; document trust boundary. `refresh`, `session`, `snapshot`,
  `command`, render.
- **Acceptance/tests:** injected Git location vars cannot redirect snapshots;
  interrupted writes leave old complete artifact; escape fixture is harmless.

## P1 — complete preparation loop

### P1.1 Multi-root repository inventory — large

- **Problem/consequence:** mixed/monorepos collapse to one language.
- **Change:** Repository/Worktree/ProjectRoot inventory and multi-adapter merge.
  `repo`, `context`, adapters.
- **Dependencies:** P0.5, P0.7.
- **Acceptance/tests:** mixed Rust/Elixir, Rust workspace, Elixir umbrella, nested
  roots; coverage and validation include all roots.

### P1.2 Scoped instruction resolver — medium

- **Problem/consequence:** applicable policy is unknown.
- **Change:** candidate discovery, ancestry/precedence/scope, source hashes and
  conflicts. New `instructions` module, `config`, context.
- **Acceptance/tests:** nested/parent/conflicting/obsolete/unreadable policies map
  to changed files with reasons.

### P1.3 Task contract — medium

- **Problem/consequence:** preparation lacks an objective.
- **Change:** CLI/session task input plus evidence-backed inferred candidates and
  unresolved conflict. `session`, new domain, context.
- **Dependencies:** P0.6.
- **Acceptance/tests:** explicit task outranks heuristics; conflicting branch
  evidence remains visible; unknown task downgrades readiness.

### P1.4 Ownership, CI, generated, and boundary discovery — large

- **Problem/consequence:** required review/check surfaces are omitted.
- **Change:** evidence readers for CODEOWNERS, local CI workflow references,
  generated/vendor rules, configured architecture boundaries. New discovery
  modules; no execution.
- **Dependencies:** P1.1/P1.2.
- **Acceptance/tests:** changed path maps to owners/check sources; unsupported CI
  syntax becomes unknown, never silently complete.

### P1.5 Minimal validation evidence — medium

- **Problem/consequence:** planned and passed cannot be distinguished.
- **Change:** opt-in argv executor and external result import; statuses, timeout,
  bounded output digest, tool versions, state tokens. `validate`, new evidence.
- **Dependencies:** P0.7, run ledger P2.1 may initially be a simple result file.
- **Acceptance/tests:** pass/fail/signal/timeout/skipped/blocked/stale, dirtying
  command, secret redaction, no shell.

### P1.6 Scale budgets and observability — medium

- **Problem/consequence:** large dirty repos can stall or exhaust resources.
- **Change:** per-stage timings/file+byte caps, degraded coverage, immutable-OID
  caches only. `git`, `consistency`, `relevance`, logging.
- **Acceptance/tests:** synthetic large fixture completes within configured caps
  and reports exactly what was not inspected.

## P2 — handoff and run evidence

### P2.1 Append-only AgentRun ledger — medium

- **Problem/consequence:** interruption loses attempt history.
- **Change:** local JSONL events with run/parent/task/entry state/selection/plans/
  results/status; atomic append/record validation. New `run_ledger`, session.
- **Dependencies:** P0 API conventions; P1.3/P1.5 add richer events later.
- **Acceptance/tests:** crash-truncated tail is detected; prior events remain;
  secrets/source content absent by default.

### P2.2 Evidence-derived handoff — medium

- **Problem/consequence:** handoff attributes current dirt and omits results.
- **Change:** entry baseline, pre-existing vs run delta, assumptions, results,
  omissions, incomplete status, next step. `handoff`, ledger.
- **Dependencies:** P2.1.
- **Acceptance/tests:** pre-existing dirty file remains labeled; successor can
  verify every result or see it stale.

### P2.3 Cross-worktree local awareness — medium

- **Problem/consequence:** recommended separate worktrees are mutually invisible.
- **Change:** coordination markers/ledger index under common git dir; host and
  process-start identity; evidence-based liveness. `session`, `sessions`.
- **Dependencies:** P0.4, P2.1.
- **Acceptance/tests:** two linked worktrees see each other without worktree dirt;
  PID reuse and malformed marker degrade honestly.

## P3 — scale, ecosystem, and polish

### P3.1 Cross-machine Git-ref exchange — large

- **Problem/consequence:** local-only runs cannot share handoff.
- **Change:** only after P2 evidence: immutable/append-only records under an
  explicitly synchronized ref, with observed-at/ref freshness and CAS merge.
- **Dependencies:** P2.1-P2.3; separate accepted proposal.
- **Acceptance/tests:** concurrent writers lose no records; no network/worktree
  mutation; stale fetch is visible.

### P3.2 Packaging/support matrix — medium

- **Problem/consequence:** dependable use is limited to source install/Linux ARM.
- **Change:** tested Linux/macOS targets, explicit Windows status, release
  checksums and compatibility statement. Workflows/docs.
- **Acceptance/tests:** clean install/version/context smoke test per supported OS.

### P3.3 Additional language adapter — medium

- **Problem/consequence:** ecosystem reach is narrow.
- **Change:** add Go only after multi-root/requirement contracts stabilize.
- **Dependencies:** P1.1 and P0.7.
- **Acceptance/tests:** same adapter conformance suite as Rust/Elixir.

