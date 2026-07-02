# Deterministic Guardrails — Rust Instantiation (v1)

**Status:** requirements paper, implementation-grade. Applies to every False
Systems Rust repository (toimija, kisko, ahti, jälki, tutka, …).
**Companion:** `guardrails-beam.md` (BEAM family). **Provenance:** the revised
Deterministic Guardrails Standard (post-review 2026-07-02); the harness is
**Kisko** (the name "Valjas" is retired).

Requirement language: **MUST** / **SHOULD** / **MAY** (RFC-2119 sense). Every
requirement is numbered for per-repo conformance checklists.

---

## 0. Core standard (normative summary)

- **G-1.** A guardrail is a check whose verdict is a fixed function of
  *declared* inputs, projected to pass/fail by a declared rule, machine-
  runnable with no model in the loop.
- **G-2 (admissibility test).** A check is admissible as a boundary iff:
  (a) its verdict is a function of declared, pinned inputs; **and**
  (b) the property it checks is *articulable* — statable independently of the
  mechanism that checks it, so a human can re-derive what "pass" means; **and**
  (c) its failure modes are decorrelated from the proposer's. A
  temperature-0 model satisfies (a) at best and fails (b) and (c); it is
  never admissible. Sampling-based checks (property tests, fuzzing) are
  admissible only with pinned replayability and failure→regression promotion.
- **G-3 (grid, not ladder).** Checks decompose as: static depth stack ×
  dynamic scope stack (unit / integration / e2e) × input-generation mode
  (example / property / fuzz). Pipeline order is fail-fast **cost
  scheduling**, per-toolchain; it is not an epistemic strength order.
- **G-4.** Verdicts MUST be bound to the exact content-addressed tree they
  evaluated, and only that tree may merge (no gate/merge TOCTOU).
- **G-5.** The gate MUST bound the *edit*, not only the resulting code
  (diff-scope guardrails, §6): the boundary must not be modifiable by the
  actor it bounds.
- **G-6.** Each repo declares its enforced cells **and** a not-covered
  inventory; silence must be legible. A lexical drift-guard MUST tie the
  declaration to the enforcing config.
- **G-7.** No blind retries at the gate. A flake is a broken guardrail:
  quarantine lane, owner, deadline.

## 1. Layer 0 — hermetic pinning (R-0.x) — prerequisite for everything

- **R-0.1** `rust-toolchain.toml` MUST pin the exact toolchain (channel +
  version); CI and local MUST resolve from it. No floating `stable`.
- **R-0.2** `Cargo.lock` MUST be committed (binaries *and* libraries in this
  org — reproducibility outranks the library convention) and the gate MUST
  run with `--locked`.
- **R-0.3** The vulnerability database consulted by `cargo audit` /
  `cargo deny check advisories` MUST be a pinned input: a vendored or
  commit-pinned advisory-db checkout, refreshed only by explicit,
  reviewable bumps. An unpinned advisory fetch disqualifies the check under
  G-1.
- **R-0.4** Toolchain-relevant env (`RUSTFLAGS`, feature sets, target
  triples used by the gate) MUST be declared in checked-in config
  (`.cargo/config.toml`, xtask), not ambient shell.
- **R-0.5** Repos already carrying toimija MUST anchor verdicts with the
  consistency token / snapshot tree OID (G-4). Others MUST record the commit
  OID the gate evaluated and merge exactly it (fast-forward or merge-queue).

## 2. Static depth stack (R-S.x) — scheduled cheap→costly for THIS toolchain

Order note (normative): **type check precedes clippy** — clippy is a
compiler pass and cannot run on non-typechecking code. The rung order in the
core standard is scheduling, and this is Rust's schedule.

- **R-S.1 Formatting.** `cargo fmt --check`. Config `rustfmt.toml` committed
  (empty file = explicit default). Auto-fix allowed pre-gate; the gate only
  checks.
- **R-S.2 Type check.** `cargo check --locked --all-targets` (workspace-wide,
  all features the repo declares as supported).
- **R-S.3 Lints, split by claim.**
  `cargo clippy --locked --all-targets -- -D warnings` as the baseline;
  additionally the repo MUST NOT `allow` any lint in clippy's `correctness`
  class without a written justification comment at the allow site.
  Style-class lints MAY be auto-fixed pre-gate; correctness-class lints are
  hard failures. `clippy.toml` committed.
- **R-S.4 Code analysis.** semgrep (if used) MUST pin its rule set by commit
  (no registry fetch at gate time).
- **R-S.5 Dependency policy.** `cargo deny check bans licenses sources` —
  deterministic from `Cargo.lock` + `deny.toml`. License allowlist and
  duplicate-version policy live in `deny.toml`.
- **R-S.6 Vulnerability lookup.** `cargo deny check advisories` (or
  `cargo audit`) against the R-0.3 pinned snapshot only.
- **R-S.7 SBOM.** CycloneDX generation (`cargo cyclonedx`) SHOULD run in the
  scheduled lane and attach to release artifacts.
- **R-S.8 Unsafe surface.** Repos containing `unsafe` MUST run `cargo miri`
  on the unsafe-bearing crates in the scheduled lane; `#![forbid(unsafe_code)]`
  SHOULD be declared by repos with none.

## 3. Dynamic scope stack (R-D.x)

- **R-D.1** `cargo test --locked --workspace` is the gate's dynamic entry.
  (nextest MAY be used; if so its config is committed and its retry feature
  MUST be disabled at the gate — G-7.)
- **R-D.2 Determinism hazards — each is a hard rule, not advice:**
  - `HashMap`/`HashSet` iteration order is randomized per process. Anything
    snapshotted, serialized for comparison, or digested MUST sort first or
    use `BTreeMap`/`BTreeSet`. (Canonical-serialization rule; also binds
    sauma digests.)
  - No wall-clock in assertions: inject clocks; `SystemTime::now()` in a
    gate-path test is a defect.
  - No unfenced real network / real filesystem outside `tempfile`; temp
    paths never appear in snapshots.
  - Thread-count sensitivity: tests MUST pass under `RUST_TEST_THREADS=1`
    and default parallelism both; ordering dependence between tests is a
    defect (found via the soak lane, fixed, never retried).
  - `#[ignore]` requires an issue link at the site.
- **R-D.3 Contract/schema cells.** Every versioned artifact schema (e.g.
  `handoff-record.v1`, sauma envelope) MUST have (a) an emitter↔schema
  agreement test (prior art: `handoff_schema_matches_emitted_record_fields`)
  and (b) a decode-old-fixture test enforcing additive-only evolution.
  Cross-repo consumers vendor the producer's fixture files.
- **R-D.4 Integration/e2e cells.** Declared per repo; e2e fewest. Integration
  tests MUST own their state (fresh temp repo/DB per test — prior art:
  toimija's `tests/common` with `GIT_*` scrubbing after the worktree-bricking
  incident).

## 4. Generation modes (R-G.x)

- **R-G.1 Property (proptest).** `proptest-regressions/` MUST be committed —
  it is the deterministic regression corpus and gate-time replay is
  automatic. `Config::cases` pinned in code. Fresh-seed exploration beyond
  the pinned cases runs in the **soak lane** (scheduled, non-gating); every
  soak failure is shrunk and lands as a committed regression before the fix
  merges (G-2 promotion rule).
- **R-G.2 Fuzz (cargo-fuzz).** Non-gating soak lane. Corpus committed (or
  artifact-stored with pinned restore); every crash becomes a permanent
  `#[test]` reproducer in the gate.
- **R-G.3 Concurrency exploration.** Crates with lock-free or
  channel-coordination logic SHOULD run `loom` tests (bounded deterministic
  interleaving exploration) in the scheduled lane.
- **R-G.4 Mutation (cargo-mutants).** Scheduled meta-gate: run on a cadence
  and on PRs touching gate-critical code; score is a **ratchet**
  (no-decrease), never an absolute target. Catches the canonical AI failure:
  vacuous tests.
- **R-G.5 Formal.** Enforcement-critical invariants (eBPF/LSM paths, the
  consistency/verify core) SHOULD carry Kani proofs for their core
  properties; scope is enumerated in the declaration, not implied.
- **R-G.6 Performance.** Wall-clock benchmarks are NOT gate-admissible.
  Instruction/allocation-count gates (iai-callgrind) MAY gate hot paths;
  criterion runs in a non-gating tracking lane.

## 5. Coverage (R-C.x)

- **R-C.1** Coverage (cargo-llvm-cov) is a **ratchet only** (no-decrease
  against main); absolute thresholds are forbidden (Goodhart). Mutation
  score (R-G.4) is the preferred strength signal.

## 6. Diff-scope guardrails (R-E.x) — the edit, not just the code

- **R-E.1 Protected paths** — changes require human sign-off, enforced in
  CI (CODEOWNERS + branch protection): `.github/`, `rust-toolchain.toml`,
  `deny.toml`, `clippy.toml`, `rustfmt.toml`, the gate xtask, the repo
  declaration (`.toimija/repo.yaml` where present), and this standard.
- **R-E.2 Test-weakening detection** (deterministic diff checks): removed
  `#[test]`/`proptest!` blocks, added `#[ignore]`, deleted
  `proptest-regressions` entries, loosened assertion counts in gate-critical
  suites → hard fail with human override only.
- **R-E.3 Scope conformance.** Where the change was launched with a declared
  scope (toimija session scope), the diff SHOULD be checked against it;
  out-of-scope edits are surfaced, not silently merged.
- **R-E.4 Diff budget.** A maximum-diff-size gate MAY be declared per repo;
  oversized AI diffs are split, not waved through.

## 7. The gate, the record, the declaration (R-R.x)

- **R-R.1 One command.** Each repo exposes the full gate as a single
  entrypoint (`cargo xtask gate` — prior art: the xtask pre-commit hook) that
  a human, CI, and the harness run identically. The pre-commit hook and CI
  call the same entrypoint.
- **R-R.2 Verdict record.** Each gate run emits an attested artifact:
  `{tool, tool_version, config_digest, tree_oid_or_token, verdict,
  report_ref}` per check — the sauma envelope (`sauma-envelope.v1`) once the
  retrofit lands; a JSONL file until then. The binary verdict is a declared
  projection of a preserved rich report.
- **R-R.3 Declaration + drift guard.** The repo declares enforced cells and
  the not-covered inventory in its manifest (`.toimija/repo.yaml`
  `validation:` where toimija is present); a doctor-style lexical drift
  guard MUST fail CI when declaration and enforcing config disagree.
- **R-R.4 No blind retries** anywhere in the gate (G-7). Flakes go to
  quarantine with owner + deadline; quarantined tests are listed in the
  not-covered inventory while quarantined.
- **R-R.5 Bypass inventory.** Direct pushes and admin merges to protected
  branches MUST be disabled or enumerated in the declaration.

## 8. Agent rollout protocol (use this section as the implementation prompt)

You are an agent bringing one Rust repository into conformance. Work like a
clockmaker: one wheel per commit, gates green at every commit, claim the
rollout issue first, branch from fresh origin/main.

1. **Audit** — produce the conformance table: every R-number → met / unmet /
   n-a (with one-line evidence). Commit as
   `docs/guardrails-conformance.md`. No code changes yet.
2. **Layer 0** — R-0.1..R-0.5 (toolchain pin, --locked, advisory snapshot,
   env declaration, verdict anchoring).
3. **Gate entrypoint** — R-R.1 xtask + wire pre-commit and CI to it;
   R-R.2 verdict record (JSONL form).
4. **Close static gaps** — R-S.x in schedule order; one commit per tool
   introduced, config committed with it.
5. **Close dynamic/generation gaps** — R-D.x, R-G.1 (regressions dir),
   soak-lane CI jobs for R-G.2/R-G.4 (non-gating).
6. **Diff-scope** — R-E.1..R-E.4 (CODEOWNERS, weakening detection, branch
   protection).
7. **Declaration + drift guard** — R-R.3 including the not-covered
   inventory; run the drift guard in CI.
8. **PR** — titled `guardrails: <repo> conformance v1`, body = the
   conformance table before/after. Do not weaken any existing test to pass
   (that act is itself R-E.2's target). Forbidden: adding retries,
   unpinning anything, editing this standard.

Acceptance: the conformance table shows every MUST met or explicitly listed
in the not-covered inventory with a reason and an issue link.
