# I. Final recommendation

## 1. Single most important missing capability

An **evidence-bearing preparation verdict** that binds the exact artifact an
agent will read to complete-enough repository/worktree/task/instruction/
validation coverage and refuses unsafe launch. Today Toimija has many of the
inputs, but nothing composes them into “ready, degraded for these reasons, or
blocked.”

## 2. Most dangerous current source of false confidence

The verify/launch story. `run` detects stale or unsafe state and launches anyway,
while later `toimija verify` checks the last-writer-wins repository packet rather
than the wrapped agent's authoritative session packet. A green check can prove
the wrong artifact; a red check can be reduced to a log warning.

## 3. What should be built next

In order:

1. Verify the selected session artifact and fail unsafe launch closed.
2. Make Git collection lossless/degraded-aware, including submodules and
   worktrees, and rename branch “intent” to surface.
3. Introduce structured validation requirements and context omissions.
4. Resolve task and scoped instructions into one PreparationReport.
5. Add minimal validation result evidence and an append-only AgentRun ledger.

## 4. What should not be built yet

Do not add languages, MCP, Tree-sitter/LSP/SCIP, a generic index, plugin system,
database, task dispatch, hosted control plane, daemon, implicit network sync, or
cross-machine ref protocol. Sauma can remain a small envelope; more provenance
does not repair incomplete preparation evidence.

## 5. Can it become dependable without becoming a workflow engine?

Yes. Dependability requires better observations, explicit coverage and
uncertainty, safe gating, and durable evidence—not scheduling. A synchronous CLI
can compile a PreparationReport, optionally run/import named validation checks,
append local JSONL evidence, and emit a handoff. Humans/GitHub/Git remain the
orchestrators.

## 6. Smallest credible v1 boundary

V1 prepares **one local Git worktree in a repository containing one or more Rust
and/or Elixir project roots** for one bounded agent run. It:

- losslessly reports supported Git state and explicitly degrades unsupported
  state;
- binds task plus applicable repository instructions to the changed surface;
- selects context deterministically with explicit omissions;
- produces structured validation requirements and captures/imports minimal
  results tied to an exact state token;
- refuses unsafe/unverifiable launch by default;
- records a local append-only run and evidence-derived handoff;
- exposes versioned human/JSON artifacts and verifies the exact artifact read.

V1 is local-first, synchronous, no-network, no-daemon, no assignment, and no
claim of cross-machine awareness. That boundary is small enough for the current
architecture and strong enough to deserve “dependable agent-preparation tool.”

