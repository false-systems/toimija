# Deterministic Guardrails — BEAM Instantiation (v1)

**Status:** requirements paper, implementation-grade. Applies to every False
Systems BEAM repository (kaiku, soiva, vaisto, …; Elixir-first, rules mark
Erlang deltas where they differ). **Companion:** `guardrails-rust.md` (shares
§0 verbatim — the core standard is language-neutral). **Provenance:** revised
Deterministic Guardrails Standard, 2026-07-02; the harness is **Kisko**
("Valjas" is retired).

Requirement language: **MUST** / **SHOULD** / **MAY**. Numbered for
conformance checklists.

**The one-paragraph BEAM thesis:** the compiler is weaker here (gradual
types — a declared gap, the one Vaisto exists to close), so the dynamic half
carries more load; the platform repays that with the strongest stateful-
property and concurrency-exploration tooling anywhere (PropEr state machines,
Concuerror) and a single seed-disciplined runner (ExUnit). The BEAM-specific
determinism hazards (map ordering, async interleaving, atom exhaustion,
timeouts-as-hidden-inputs) are legislated here so no repo rediscovers them
as flakes with false provenance.

---

## 0. Core standard

Identical to `guardrails-rust.md` §0 (G-1..G-7): admissibility = function of
declared inputs ∧ articulable predicate ∧ decorrelated failure modes; grid
not ladder; verdicts tree-anchored; the edit is bounded, not just the code;
declaration + drift guard + not-covered inventory; no blind retries.

## 1. Layer 0 — hermetic pinning (B-0.x)

Determinism is *more* version-sensitive on BEAM than on Rust: dialyzer
verdicts, `mix format` output, `inspect` rendering, and compiler warnings all
change across OTP/Elixir releases.

- **B-0.1** OTP and Elixir versions MUST be pinned (`.tool-versions` via
  asdf/mise, or Nix); CI and local resolve from the same file. No floating
  versions, ever.
- **B-0.2** `mix.lock` MUST be committed (it carries Hex registry checksums —
  content-addressing built in). Gate runs `mix deps.get --check-locked` and
  `mix deps.unlock --check-unused`.
- **B-0.3** The MixAudit vulnerability database MUST be a pinned input
  (vendored / commit-pinned snapshot; explicit reviewable bumps). Unpinned
  advisory fetch disqualifies the check (G-1).
- **B-0.4** Compile with `+deterministic` (`ERL_COMPILER_OPTIONS`) so `.beam`
  files omit absolute paths and compile-time env — required wherever build
  artifacts are digested or compared.
- **B-0.5** The dialyzer PLT is a **derived pinned artifact**: built and
  cached keyed by exactly {OTP, Elixir, mix.lock digest}; a PLT from any
  other triple is a different input, not a warm cache.
- **B-0.6** `MIX_ENV`, `ERL_FLAGS`, and node-name/port assignments used by
  the gate MUST be declared in checked-in config, not ambient shell.
- **B-0.7** Verdict anchoring as in the Rust paper (consistency token / tree
  OID where toimija is present; exact commit OID otherwise) — G-4.

## 2. Static depth stack (B-S.x) — BEAM's schedule differs, by design

Order note (normative): Credo parses **source** and runs pre-compile; the
compiler is the correctness gate; dialyzer needs compiled beams + PLT and is
the most expensive static check. This is the mirror image of Rust's
fmt→check→clippy — proof that rung order is per-toolchain scheduling (G-3).

- **B-S.1 Formatting.** `mix format --check-formatted`; `.formatter.exs`
  committed. Formatter output changes between Elixir versions — B-0.1 is the
  reason this check is deterministic at all.
- **B-S.2 Style lints.** `mix credo --strict` with `.credo.exs` committed;
  Credo's readability/consistency/refactor categories gate here.
- **B-S.3 Compiler as correctness gate.**
  `mix compile --warnings-as-errors --force`. On BEAM this slot is
  load-bearing: undefined-function, unreachable-clause, and (Elixir ≥1.17)
  set-theoretic type-inference warnings are the platform's cheapest real
  correctness check. Credo's `warning` category gates here too (the
  style/correctness split, same as clippy's classes).
- **B-S.4 Types — the declared gap.** `mix dialyzer` (dialyxir) with the
  B-0.5 PLT. Success typing proves *some* errors, never their absence; the
  repo declaration MUST say "gradual", not "type-checked". Public APIs of
  library repos MUST carry `@spec`s (dialyzer earns its cost only where
  specs exist). This is the gap Vaisto exists to close; until it does, BEAM
  repos compensate with §4 — and that trade is *declared*, per G-6.
- **B-S.5 Code/security analysis.** Sobelow for Phoenix-facing repos only
  (declare applicability; it is meaningless elsewhere). Two hard lints
  everywhere: no `String.to_atom/1` on external input
  (`to_existing_atom/1` only — atom-table exhaustion is a VM-kill), no
  dynamic `apply/3` on unvalidated module/function atoms.
- **B-S.6 Dependency policy.** `mix hex.audit` (retired packages) MUST gate.
  License policy: **named gap** — no cargo-deny equivalent exists; the
  nearest conforming mechanism is a small committed mix task over `mix.lock`
  + Hex metadata with an allowlist. Until it exists in a repo, license
  checking goes in that repo's not-covered inventory. SBOM via the CycloneDX
  mix task in the scheduled lane.
- **B-S.7 Vulnerability lookup.** `mix deps.audit` against the B-0.3 pinned
  snapshot only.

## 3. Dynamic scope stack (B-D.x) — one runner, one seed discipline

- **B-D.1** ExUnit is the single runner for unit, integration, and
  in-process e2e (ConnTest / LiveViewTest). Browser e2e (Wallaby) is
  declared separately and kept fewest.
- **B-D.2 The seed is the replay handle.** ExUnit randomizes test *order*
  and prints the seed; `mix test --seed N` replays exactly. Policy:
  - the **gate** runs a pinned seed (declared in the gate alias);
  - a **soak lane** (scheduled, non-gating) runs fresh seeds to hunt
    order-dependence;
  - any soak failure is replayed by its printed seed and *fixed* — order
    dependence is a defect, never retried away (G-7).
- **B-D.3 `async: true` is a contract, not a speed flag.** Async is
  permitted ONLY with fully sandboxed state: Ecto SQL Sandbox per-test
  ownership; Mox in private mode with `verify_on_exit!`; no named
  singletons, no shared ETS, no `Application.put_env` in async modules. An
  async test touching a named process is a flake being born.
- **B-D.4 Canonical serialization.** Map iteration order is unspecified (and
  differs between small-map and HAMT representations). Anything snapshotted,
  compared, or digested MUST sort keys first. This also binds sauma-envelope
  emitters on BEAM: canonicalize before digesting. Digests use
  `:crypto.hash(:sha256, …)` — never `:erlang.phash2` (not guaranteed stable
  across OTP releases).
- **B-D.5 Identity and randomness.** Pids, `make_ref`,
  `:erlang.unique_integer` never appear in comparison output; `:rand` is
  explicitly seeded anywhere gate-relevant.
- **B-D.6 Time.** `assert_receive` timeouts are declared inputs — set
  explicitly and generously (the 100 ms default is a hidden input under CI
  load); no `Process.sleep` synchronization — monitors, messages, and
  `assert_receive` only.
- **B-D.7 Contract/schema cells.** Behaviours + Mox are the BEAM contract
  mechanism: every cross-boundary dependency has a behaviour, and every mock
  is therefore a compile-checked contract. Language-neutral schemas (packet,
  handoff-record.v1, sauma envelope) get the same two tests as the Rust
  side: emitter↔schema agreement, and decode-old-fixture (additive-only
  evolution). Fixture files are vendored from the producing repo.
- **B-D.8 Test isolation.** Integration tests own their state (fresh temp
  dirs/DBs per test); `@tag :skip` / `@moduletag :skip` require an issue
  link at the site.

## 4. Generation modes (B-G.x) — where BEAM out-punches its type system

- **B-G.1 Property, stateless.** StreamData; generation seed derives from
  the ExUnit seed, so B-D.2's pinning covers it. StreamData has **no
  automatic counterexample persistence** (a real gap vs proptest): the
  promotion rule is therefore mandatory process — every shrunken
  counterexample is committed as an explicit example test *before* the fix
  merges.
- **B-G.2 Property, stateful — the BEAM superpower.** PropCheck (PropEr)
  state-machine properties for every invariant-shaped core: model the
  GenServer as a state machine and drive command sequences against the
  domain invariants (append never mutates prior records; a delegation chain
  resolves to exactly one root). PropCheck's counterexample file MUST be
  committed — it is the regression corpus.
- **B-G.3 Concurrency exploration.** **Concuerror** systematically explores
  process interleavings of a test — bounded, reproducible exploration; the
  closest thing to model-checking a supervision or enforcement path the
  platform offers. Scheduled lane for enforcement-critical GenServers and
  supervisors; findings become pinned regression tests.
- **B-G.4 Fuzz — honest placement.** BEAM's byte-fuzzing story is weak.
  Structured adversarial input = property tests with hostile generators
  (B-G.1/2). Binary parsers on hot paths SHOULD live in Rust NIFs (rustler)
  and be fuzzed on the Rust side per `guardrails-rust.md` R-G.2 — the
  boundary advice is architectural, not aspirational.
- **B-G.5 Mutation.** muzak where it runs; the tooling is sparsely
  maintained, so: scheduled lane, expect investment, and until it is
  reliable the compensating controls are the coverage **ratchet** (B-C.1)
  plus the committed counterexample corpus. Declared in the not-covered
  inventory where muzak is not yet running (G-6 — silence made legible).
- **B-G.6 Performance.** Wall-clock benchmarks are not gate-admissible.
  Benchee runs in a non-gating tracking lane; reductions-based budget checks
  MAY gate where a deterministic proxy is established.

## 5. Coverage (B-C.x)

- **B-C.1** Coverage (`mix test --cover` / excoveralls) is a ratchet only
  (no-decrease vs main); absolute thresholds forbidden. Mutation score
  supersedes it wherever B-G.5 runs.

## 6. Diff-scope guardrails (B-E.x)

- **B-E.1 Protected paths** (human sign-off; CODEOWNERS + branch
  protection): `.github/`, `.tool-versions`, `.formatter.exs`, `.credo.exs`,
  `mix.exs` (the gate aliases live here), `mix.lock`, `config/*` gate-relevant
  entries, the repo declaration, and this standard.
- **B-E.2 Test-weakening detection** (deterministic diff checks): deleted
  `test`/`property` blocks, added `:skip` tags, raised `assert_receive`
  timeouts in gate-critical suites, deleted PropCheck counterexample
  entries → hard fail, human override only.
- **B-E.3 / B-E.4** Scope conformance and diff budget: as
  `guardrails-rust.md` R-E.3/R-E.4.

## 7. The gate, the record, the declaration (B-R.x)

- **B-R.1 One command.** The full gate is one committed mix alias run
  identically by human, CI, and harness:

  ```elixir
  "gate": [
    "deps.get --check-locked",
    "deps.unlock --check-unused",
    "format --check-formatted",
    "credo --strict",
    "compile --warnings-as-errors --force",
    "dialyzer",
    "hex.audit",
    "deps.audit",          # pinned snapshot (B-0.3)
    "test --seed 0"        # pinned; soak lane runs fresh seeds
  ]
  ```

- **B-R.2 Verdict record.** As R-R.2: attested artifact per check
  `{tool, version, config_digest, tree_oid_or_token, verdict, report_ref}`;
  sauma envelope once the retrofit lands, JSONL until then. Binary verdict =
  declared projection of a preserved report.
- **B-R.3 Declaration + drift guard.** Enforced cells + not-covered
  inventory declared in the repo manifest; a lexical drift guard (doctor
  pattern) fails CI when declaration and `mix.exs` alias disagree.
- **B-R.4 No blind retries; quarantine with owner + deadline; quarantined
  tests listed in the not-covered inventory.** ExUnit/CI retry plugins are
  forbidden at the gate.
- **B-R.5 Bypass inventory** as R-R.5.

## 8. Agent rollout protocol (use as the implementation prompt)

You are an agent bringing one BEAM repository into conformance. One wheel
per commit; `mix gate` green at every commit once it exists; claim the
rollout issue; branch from fresh origin/main.

1. **Audit** — conformance table: every B-number → met / unmet / n-a with
   one-line evidence. Commit as `docs/guardrails-conformance.md`.
2. **Layer 0** — B-0.1..B-0.7 (pins, lock checks, advisory snapshot,
   `+deterministic`, PLT caching, env declaration, verdict anchoring).
3. **Gate alias** — B-R.1 committed and wired into CI + pre-commit; B-R.2
   record (JSONL form).
4. **Static stack** — B-S.1..B-S.7 in schedule order, one commit per tool,
   config committed with it. Where a named gap applies (license policy),
   write the not-covered entry instead of faking a check.
5. **Dynamic discipline** — B-D.2 seed policy (gate pinned + soak-lane CI
   job), B-D.3 async audit (fix or de-async every violator), B-D.4..B-D.8.
6. **Generation modes** — B-G.1 promotion rule documented in CONTRIBUTING;
   B-G.2 PropCheck state-machine suite for each declared core invariant
   (counterexample file committed); B-G.3 Concuerror scheduled job for
   enforcement-critical processes.
7. **Diff-scope** — B-E.1..B-E.4.
8. **Declaration + drift guard** — B-R.3 with the not-covered inventory
   (dialyzer="gradual", license gap, muzak status — honesty is the
   deliverable).
9. **PR** — `guardrails: <repo> conformance v1`, body = before/after
   conformance table. Never weaken an existing test to pass (B-E.2 is
   watching for exactly that). Forbidden: retries, unpinning, editing this
   standard.

Acceptance: every MUST met or explicitly in the not-covered inventory with
a reason and an issue link.
