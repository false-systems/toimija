# F. Architecture recommendation

## Smallest coherent architecture

Keep one synchronous Rust crate and the current command runner. Evolve the
pipeline from “collect structs and render” into:

```text
Git inspection ─┐
Repo discovery ─┼─> PreparationReport ─> Context planner ─> Markdown / API v1
Instructions ───┤          │
Task input ─────┘          ├─> Validation planner ─> optional executor/import
                           └─> AgentRun JSONL ─> Handoff projection
```

### Core domain

`PreparationReport` owns facts, inferred claims, coverage, uncertainty, and one
readiness verdict. Renderers must not invent semantic labels. Task/Intent and
BranchState remain separate. Every negative claim must have successful evidence;
otherwise it is unknown/degraded.

### Git inspection layer

Retain the CLI-based approach and shared runner. Replace textual parsing with
byte-safe `-z` plumbing. Split collection into named stages returning
`StageResult<T>`. Add ahead/behind, ref OIDs, common/worktree git dirs,
operation detail, sparse/index flags, submodule state, and observation metadata.
Do not fetch. State remote freshness as unknown.

### Repository discovery layer

Replace singular `RepoKind` with `Vec<ProjectRoot { root, kinds, evidence }>`.
Use bounded marker discovery first. For detected Rust, prefer `cargo metadata
--no-deps` when available/safe; for Elixir, derive umbrella apps from local
manifests. Fall back to current path heuristics with degraded coverage. Do not
scan source semantics or introduce Tree-sitter/LSP/SCIP yet.

### Instruction resolver

Discover root and ancestor/local `AGENTS.md`/`CLAUDE.md`, configured important
docs, CONTRIBUTING/SECURITY, and explicit config entries. Resolve applicable
instructions per changed/task path with source hash, scope, precedence, and
conflict. Treat content as untrusted text; listing a file is not resolution.

### Context planner

Convert evidence into `ContextItem`s, assign mandatory/relevance/cost, then
select deterministically under a declared unit (`utf8_bytes` initially; tokens
later only if a proven tokenizer is available). Emit selected and omitted item
IDs/reasons. JSON and Markdown describe the same selection; full raw evidence
may remain available under an explicit unbudgeted API.

### Language adapters

Adapters report project roots, changed units, companion tests, and candidate
validation requirements. Core owns Git state, task, instruction precedence,
command safety, evidence, and rendering. Repository config supplies facts that
discovery cannot prove. Keep the trait small; add Go only after the multi-root
contract works for both existing adapters.

### Validation planner and optional executor

Requirements precede commands. A `CommandSpec` is argv, cwd, environment names
(not secret values), safety/mutability, cost, and coverage. Config is additive
by default with explicit per-tier replacement. The smallest executor is opt-in,
no shell, timeout/cancellation, bounded stdout/stderr capture, exit/signal, and
state token before/after. Also allow importing external results against a token.
This is evidence capture, not a task runner or scheduler.

### Evidence, run ledger, and handoff

Use `.toimija/runs/<run-id>.jsonl`, append-only. Events: opened, task declared,
preparation compiled, context selected, command planned/started/finished,
handoff written, closed. Keep source/output payloads out by default; store hashes
and bounded excerpts only when requested. Derive session listing and handoff from
events plus current marker. Move local cross-worktree state under the common Git
dir; keep worktree packet files where they are.

### Persistence and JSON API

Atomically replace packets/markers. Give every artifact `schema`, `artifact_id`,
producer version, repository/worktree identity, state token, and coverage.
Create explicit v1 DTOs and JSON schemas for context/preparation, verify,
sessions, snapshot, validation plan/result, run events, and handoff. Additive
fields are allowed; enum additions require tolerant readers; breaking shape
changes require v2. Paths use UTF-8 when valid and a documented byte encoding
otherwise.

## Change disposition

### Remain as-is in principle

- Single crate, synchronous commands, no network/background daemon.
- Git CLI and argv-based process execution.
- Separate staged/unstaged/untracked/conflict surfaces.
- Content-based consistency and sauma envelope.
- Config-driven role/boundaries and advisory relevance.
- Foreground `run`/`watch`, explicit handoff, Git-object snapshots.

### Refactor

- `GitState` into evidence-rich RepositoryState/BranchState.
- Singular repo kind into multi-root discovery.
- Validation strings into requirements + CommandSpec.
- Renderer-first budgeting into ContextSelection.
- Session marker/handoff into views over an AgentRun ledger.
- Internal serde types into explicit API DTOs.

### Remove or rename

- Rename rendered “Branch intent” to “Branch surface” until actual intent exists.
- Remove “mechanical ripple—skip” strength; retain a weak, sourced hint.
- Remove claims that inferred defaults are the repository's “real gates.”
- Do not silently skip failed evidence or malformed coordination records.

### Must add

- Readiness verdict and degraded coverage.
- Per-session artifact verification and fail-safe launch.
- Lossless Git/submodule/worktree coverage.
- Task and scoped instruction resolution.
- Structured validation requirements/results.
- Atomic persistence and versioned machine contracts.
- Minimal append-only run evidence.

### Deliberately do not build

No workflow engine, task assignment, hosted control plane, database, plugin
system, MCP, Tree-sitter, generic code index, daemon, implicit network sync, or
language expansion before the preparation contract is dependable. Defer the
cross-machine coordination proposal until local cross-worktree evidence and
concurrency semantics are proven.

