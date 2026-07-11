# C. Findings register

`Blocks wider use` means the finding prevents claiming dependable preparation,
not that every current local use must stop.

## Correctness and honesty

### TA-001 — The authoritative session packet cannot be verified by the CLI

- **Area / severity / confidence:** freshness; **critical**; **confirmed**
- **Evidence:** wrapped agents are told their per-session packet is authoritative
  (`src/session.rs:155-178`), but `verify` always loads
  `.toimija/current.json` (`src/cli.rs:1111-1175`), which is explicitly a
  last-writer-wins mirror (`src/refresh.rs:102-114`).
- **Why / scenario:** agent A reads A's packet while agent B last wrote the repo
  packet; A's `toimija verify` proves B's artifact, not A's.
- **Correction:** resolve the packet from `TOIMIJA_LIVE_JSON` inside a valid
  ambient session, accept an explicit `--packet`, and return the verified path
  and sauma digest.
- **Blocks wider use:** yes.

### TA-002 — `run` launches after detecting stale or unsafe entry state

- **Area / severity / confidence:** preparation gate; **critical**; **confirmed**
- **Evidence:** stale, unsupported, and unsafe verdicts only call `warn!`
  (`src/cli.rs:673-703`); launch follows unconditionally
  (`src/cli.rs:705-759`).
- **Why / scenario:** an agent starts broad edits during a merge/rebase or after
  a refresh race despite the README's verify-before-read posture.
- **Correction:** make unsafe/unverifiable entry a refusal; retry one stale race;
  require an explicit named override for advisory-only launch.
- **Blocks wider use:** yes.

### TA-003 — Git parsing is not path-lossless

- **Area / severity / confidence:** Git model; **high**; **confirmed**
- **Evidence:** command output is converted with `from_utf8_lossy`
  (`src/command.rs:61-62`); porcelain is line/space parsed rather than `-z`
  (`src/git.rs:270-340`); conflict paths use the last whitespace token
  (`src/git.rs:288-293`).
- **Why / scenario:** a conflict named `docs/release plan.md`, newline-bearing
  path, quoted path, or non-UTF-8 Unix path can be changed, split, omitted, or
  attributed to another path.
- **Correction:** retain stdout bytes and parse `git status --porcelain=v2 -z`
  plus `git diff --name-status -z`; define byte-safe JSON path encoding.
- **Blocks wider use:** yes.

### TA-004 — Submodule state can change without changing the consistency token

- **Area / severity / confidence:** freshness; **critical**; **strongly indicated**
- **Evidence:** porcelain's submodule field is ignored (`src/git.rs:303-340`);
  a directory fingerprint records only path/type/mode
  (`src/consistency.rs:238-254`).
- **Why / scenario:** a submodule HEAD or dirt changes while the directory mode
  stays constant; worktree status remains `.M`, but the computed metadata and
  token can remain identical.
- **Correction:** model gitlink HEAD, index OID, worktree OID, and dirt flags;
  pin them in the consistency algorithm and acceptance tests.
- **Blocks wider use:** yes for repos with submodules.

### TA-005 — Evidence collection failures can masquerade as empty evidence

- **Area / severity / confidence:** error honesty; **high**; **confirmed**
- **Evidence:** `run_git_ok` returns `Option` for spawn and exit failure
  (`src/git.rs:261-267`); failed branch diff/log become empty vectors
  (`src/git.rs:146-159`); unreadable session markers are skipped
  (`src/sessions.rs:147-174`).
- **Why / scenario:** an unsupported Git invocation yields “0 files changed” or
  no sibling session rather than “unknown/degraded.”
- **Correction:** stage results must carry `available|degraded|failed`, command,
  and safe diagnostic; never derive negative claims from failed collection.
- **Blocks wider use:** yes.

### TA-006 — Branch intent is a diff label, not evidenced intent

- **Area / severity / confidence:** intent; **high**; **confirmed**
- **Evidence:** base selection is origin HEAD/common-name heuristic
  (`src/git.rs:219-241`); “Branch intent” renders file count only
  (`src/render.rs:418-445`).
- **Why / scenario:** a stacked branch based on another feature or a repository
  integrating through `develop` is compared with `origin/main` and presented as
  one coherent intent.
- **Correction:** rename current evidence to branch surface; represent intent
  separately with sources, candidate base, confidence, conflicts, and user
  confirmation.
- **Blocks wider use:** yes.

### TA-007 — Divergence and remote freshness are absent

- **Area / severity / confidence:** branch state; **high**; **confirmed**
- **Evidence:** `GitState` has upstream but no ahead/behind or observation time
  (`src/git.rs:12-31`); porcelain `branch.ab` is ignored
  (`src/git.rs:270-302`).
- **Why / scenario:** local commits not pushed, upstream divergence, and a stale
  remote-tracking ref all read similarly.
- **Correction:** capture ahead/behind and ref OIDs locally; explicitly state
  that remote freshness is unknown until the human fetches.
- **Blocks wider use:** yes for coordination/landing claims.

### TA-008 — Advanced index/worktree states are invisible

- **Area / severity / confidence:** Git model; **high**; **confirmed**
- **Evidence:** no sparse checkout, skip-worktree, assume-unchanged,
  intent-to-add, linked-worktree/common-dir, or submodule fields exist in
  `GitState` (`src/git.rs:12-31`).
- **Why / scenario:** a clean sparse worktree or hidden index flag is presented
  as a complete ordinary checkout.
- **Correction:** add capability coverage and targeted plumbing inspection;
  degrade when an important state is detected but unsupported.
- **Blocks wider use:** yes for affected repos.

## Repository, task, and context

### TA-009 — One root-level enum cannot represent a mixed or multi-root repo

- **Area / severity / confidence:** repository model; **high**; **confirmed**
- **Evidence:** `RepoKind` is singular and detection checks only root markers;
  Rust wins (`src/repo.rs:11-16`, `src/repo.rs:78-87`). The context merely warns
  if root `mix.exs` coexists (`src/context.rs:128-141`).
- **Why / scenario:** a monorepo with Rust and Elixir projects gets only Rust
  validation and Rust read-first behavior.
- **Correction:** inventory multiple `ProjectRoot`s and run every applicable
  adapter; keep a generic repository layer above them.
- **Blocks wider use:** yes.

### TA-010 — Repository instructions are listed, not resolved

- **Area / severity / confidence:** policy; **high**; **confirmed**
- **Evidence:** read-first adds configured/root important docs by existence
  (`src/context.rs:389-437`); doctor concatenates root docs and checks lexical
  presence (`src/repo.rs:228-315`, `src/repo.rs:346-357`).
- **Why / scenario:** nested `AGENTS.md`, conflicting parent/child policy, a
  security policy, or file-scoped test rule never becomes an applicable
  instruction for a changed file.
- **Correction:** discover instruction candidates, resolve ancestry and explicit
  precedence per target path, retain source/range/hash, and report conflict.
- **Blocks wider use:** yes.

### TA-011 — Task scope is optional advisory text, not a task contract

- **Area / severity / confidence:** task; **high**; **confirmed**
- **Evidence:** packet session identity contains only id/optional intent/scope
  (`src/context.rs:63-70`); marker fields are free text (`src/session.rs:53-65`).
- **Why / scenario:** prompt, issue, branch, commit messages, failing tests, and
  pre-existing dirt can disagree with no representation of the conflict.
- **Correction:** add Task with supplied objective/constraints, evidence refs,
  inferred scope, exclusions, unanswered questions, and confidence.
- **Blocks wider use:** yes.

### TA-012 — Context budgeting optimizes byte fit, not correctness

- **Area / severity / confidence:** context selection; **high**; **confirmed**
- **Evidence:** Markdown is fully rendered then cut at a character budget
  (`src/render.rs:216-363`, `src/render.rs:606-624`); JSON is always full
  (`src/refresh.rs:154-156`).
- **Why / scenario:** ownership or a relevant test can be omitted with only a
  generic tail marker; consumers cannot know which items were considered and
  excluded.
- **Correction:** plan context items before rendering with mandatory/priority,
  estimated cost, provenance, and an explicit omissions manifest.
- **Blocks wider use:** yes.

### TA-013 — “Mechanical ripple” can demote authored source

- **Area / severity / confidence:** relevance; **high**; **confirmed**
- **Evidence:** a manifest+matching lockfile makes source in the manifest subtree
  eligible for ripple classification (`src/relevance.rs:782-850`); README tells
  agents to read intent and skip ripple (`README.md:503-524`).
- **Why / scenario:** a dependency bump and required handwritten compatibility
  changes occur together; the most important code is ranked last.
- **Correction:** label this weak heuristic, never recommend skipping it, and
  require corroborating generated-file evidence before strong demotion.
- **Blocks wider use:** yes.

### TA-014 — Ownership, CI requirements, and architectural boundaries are absent

- **Area / severity / confidence:** repository understanding; **high**;
  **confirmed**
- **Evidence:** no ownership/CI requirement/domain-boundary types or discovery
  exist; `ContextPacket` contains only current render-oriented fields
  (`src/context.rs:27-60`).
- **Why / scenario:** local tests pass while CODEOWNERS approval, a CI-only job,
  generated check, migration gate, or architecture rule is unknown.
- **Correction:** add evidence sources for ownership, CI workflows, configured
  boundaries, and coverage gaps; do not claim completeness.
- **Blocks wider use:** yes.

## Validation and evidence

### TA-015 — Validation is an unstructured list of commands

- **Area / severity / confidence:** validation; **critical**; **confirmed**
- **Evidence:** `ValidationPlan` is tiered `Vec<String>` plus notes
  (`src/validate.rs:10-24`); inference is fixed whole-repo Cargo/Mix commands
  (`src/validate.rs:32-86`).
- **Why / scenario:** an agent cannot tell why a command exists, what it covers,
  where to run it, prerequisites, mutability, cost, authority, or omissions.
- **Correction:** introduce ValidationRequirement and structured CommandSpec;
  render shell text only at the edge.
- **Blocks wider use:** yes.

### TA-016 — A configured validation section suppresses all inferred defaults

- **Area / severity / confidence:** validation; **high**; **confirmed**
- **Evidence:** any non-empty configured tier immediately returns only config
  commands (`src/validate.rs:32-44`; `src/config.rs:118-123`).
- **Why / scenario:** adding one privileged deployment check silently removes
  inferred formatting, lint, and tests.
- **Correction:** give config explicit `extend`/`replace` semantics per tier;
  default to additive and report overridden requirements.
- **Blocks wider use:** yes.

### TA-017 — No validation result or stale-evidence model exists

- **Area / severity / confidence:** evidence; **critical**; **confirmed**
- **Evidence:** `validate` deliberately prints only a static plan
  (`src/cli.rs:1437-1453`); handoff stores only suggested commands and a token
  (`src/handoff.rs:145-205`).
- **Why / scenario:** successor sees a command suggestion and prose “tests pass”
  but cannot distinguish planned, attempted, passed, failed, skipped, blocked,
  unknown, or invalidated by later edits.
- **Correction:** smallest layer is an opt-in argv executor and/or result import
  that records status, cwd, start/end, exit/signal, tool versions, bounded output
  digest, and consistency token.
- **Blocks wider use:** yes.

### TA-018 — Handoff records current state, not agent-attributable work

- **Area / severity / confidence:** handoff; **high**; **confirmed**
- **Evidence:** the marker records entry HEAD but not entry dirty state
  (`src/session.rs:34-78`); handoff rebuilds the current packet and snapshots it
  (`src/handoff.rs:58-73`, `src/handoff.rs:135-207`).
- **Why / scenario:** pre-existing dirty files are described as the departing
  agent's state with no indication they existed before; validation results and
  explicit incomplete status are absent.
- **Correction:** capture entry baseline/token, compute exit delta against it,
  preserve “pre-existing,” and link ValidationResults and omissions.
- **Blocks wider use:** yes.

### TA-019 — Handoff suggestion can omit mandatory validation tiers

- **Area / severity / confidence:** handoff; **medium**; **confirmed**
- **Evidence:** handoff chooses focused commands if present, otherwise fast,
  omitting strict/full/privileged (`src/handoff.rs:145-149`).
- **Why / scenario:** successor runs only a focused crate test because the
  handoff appears to contain “suggested validation.”
- **Correction:** store requirement IDs and every tier/status, with focused
  explicitly marked non-substitutive.
- **Blocks wider use:** yes.

### TA-020 — There is no first-class run ledger

- **Area / severity / confidence:** recoverability; **high**; **confirmed**
- **Evidence:** session marker is latest mutable state and handoff is optional;
  neither records selected/omitted context, commands attempted, results, or
  lifecycle events (`src/session.rs:34-78`, `src/handoff.rs:25-55`).
- **Why / scenario:** interruption before explicit handoff loses the reasoning
  needed to continue safely.
- **Correction:** append a small local JSONL event stream per run; derive the
  current session view and handoff from it.
- **Blocks wider use:** yes for interruption/replay claims.

## Concurrency, operations, security, and scale

### TA-021 — Session awareness is neither worktree-wide nor reliably live

- **Area / severity / confidence:** multi-agent; **high**; **confirmed**
- **Evidence:** sessions are read only below the current worktree's
  `.toimija/sessions` (`src/sessions.rs:96-174`), not the common Git directory.
  During this audit, live `toimija sessions --json` labeled the still-open Codex
  session stale, showing the pid/heartbeat heuristic can disagree with reality.
- **Why / scenario:** agents in separate worktrees—the recommended isolation—do
  not see one another; a live agent may be presented as abandoned.
- **Correction:** keep local coordination metadata under the common git dir,
  add host/process-start identity, and expose liveness evidence/uncertainty.
- **Blocks wider use:** yes for multi-agent claims.

### TA-022 — Linked-worktree Git changes can bypass live refresh

- **Area / severity / confidence:** live operation; **high**; **strongly indicated**
- **Evidence:** watcher recursively watches only `repo_root`
  (`src/watch.rs:41-51`), while Git state for a linked worktree can live in an
  external common/worktree git directory. Refresh errors are ephemeral stderr
  warnings (`src/watch.rs:75-91`).
- **Why / scenario:** commit/reset/rebase changes HEAD/index without a relevant
  worktree event; the session packet remains stale until another file event.
- **Correction:** watch resolved git-dir/common-dir as well, or periodically
  verify the token during `run`; persist degraded watcher state in the packet.
- **Blocks wider use:** yes.

### TA-023 — Packet pairs and markers are not atomically replaced

- **Area / severity / confidence:** persistence; **medium**; **confirmed**
- **Evidence:** Markdown then JSON are written directly
  (`src/refresh.rs:161-170`); markers are also direct `fs::write`
  (`src/session.rs:180-184`).
- **Why / scenario:** crash or concurrent read observes mismatched packet pair or
  truncated JSON and treats it as current.
- **Correction:** temp file in same directory, flush as required, atomic rename;
  include a shared artifact id in both forms.
- **Blocks wider use:** yes for dependable harness use.

### TA-024 — Snapshot bypasses the hardened Git command path

- **Area / severity / confidence:** security/data integrity; **high**;
  **confirmed**
- **Evidence:** the shared runner scrubs repo-locating Git variables
  (`src/command.rs:17-42`, `src/command.rs:47-78`), but snapshot creates raw Git
  commands without that scrub (`src/snapshot.rs:170-205`).
- **Why / scenario:** inherited `GIT_DIR` redirects object/ref writes to another
  repository; `git add -A` may also invoke repository-configured clean filters
  in a hostile repo.
- **Correction:** route all Git through one hardened runner; document snapshot as
  an explicit trust boundary and add a safe inspection posture.
- **Blocks wider use:** yes for untrusted repos.

### TA-025 — Repository-controlled text can reach terminals unsanitized

- **Area / severity / confidence:** security; **medium**; **strongly indicated**
- **Evidence:** paths, commit summaries, config role, validation strings, and
  warnings are interpolated directly into Markdown/terminal output
  (`src/render.rs:216-363`, `src/render.rs:452-463`).
- **Why / scenario:** control characters or escape sequences spoof headings,
  diagnostics, or terminal state; malicious validation text can look like an
  instruction even though it is not executed.
- **Correction:** escape control characters for terminal/Markdown, mark trust
  provenance, and keep machine output byte-safe.
- **Blocks wider use:** yes for hostile repositories.

### TA-026 — Large dirty repositories can exhaust time and memory

- **Area / severity / confidence:** performance; **high**; **confirmed**
- **Evidence:** status requests all untracked files (`src/git.rs:96-107`), every
  changed/untracked file is content-hashed (`src/consistency.rs:165-188`), and
  `run` installs a recursive watcher (`src/watch.rs:41-51`).
- **Why / scenario:** a monorepo with hundreds of thousands of untracked files or
  large binaries repeatedly scans/hashes on refresh, delaying launch and making
  watch storms self-amplifying.
- **Correction:** measurable stage budgets, file/byte limits, explicit degraded
  coverage, and caching only for immutable Git objects keyed by OID.
- **Blocks wider use:** yes at monorepo scale.

### TA-027 — Most JSON output is an internal serialization, not a contract

- **Area / severity / confidence:** machine API; **high**; **confirmed**
- **Evidence:** context directly serializes domain structs (`src/context.rs:27-60`);
  only handoff and sauma have schema files (`docs/schemas/`); sessions, verify,
  snapshot, landed, and context lack complete published schemas.
- **Why / scenario:** adding an internal field, changing an enum shape, lossy
  path conversion, or returning a CLI error on stderr breaks an agent consumer
  without a versioned compatibility decision.
- **Correction:** contract DTOs and JSON schemas per command family, stable error
  envelope, conformance fixtures, and explicit additive/breaking rules.
- **Blocks wider use:** yes.

### TA-028 — Product documentation overstates preparation and validation

- **Area / severity / confidence:** product claims; **high**; **confirmed**
- **Evidence:** README says agents start oriented and validation prints the
  repo's “real gates” (`README.md:72-105`), while validation is inferred from one
  root marker and has no CI/policy discovery (`src/validate.rs:32-86`).
- **Why / scenario:** users treat a green inferred Cargo plan as proof the real
  repository contract was found.
- **Correction:** describe current output as Git-state compilation plus heuristic
  suggestions; reserve “prepared” and “real gates” for explicit coverage with
  known omissions.
- **Blocks wider use:** yes as a trust claim.

