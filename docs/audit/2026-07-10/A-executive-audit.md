# A. Executive audit

## Verdict

**Useful internal tool.**

Toimija is beyond a prototype: it has a coherent Git-first thesis, a small and
well-tested implementation, deterministic output, freshness fingerprints,
session packets, and useful local handoff primitives. It is not yet a
dependable agent-preparation tool. It can produce and launch an agent with a
plausible packet while critical preparation evidence is missing, inferred from
weak signals, stale, or known unsafe.

The architecture can support the intended product incrementally. The single
crate and synchronous command model are strengths. The missing center is a
first-class, evidence-bearing preparation result: task, scoped instructions,
repository/worktree state, validation requirements, coverage, uncertainty,
and freshness must be modeled before rendering. No rewrite or workflow engine
is warranted.

## Five strongest capabilities

1. **Git state is genuinely central.** HEAD, index, worktree, merge-base,
   committed branch surface, and distinct staged/unstaged/untracked/conflict
   lists are collected rather than flattened (`src/git.rs:12-31`,
   `src/context.rs:271-292`).
2. **Freshness is content-based.** The consistency token includes HEAD, index,
   tracked worktree content, untracked content, conflicts, and in-flight Git
   operations (`src/consistency.rs:15-25`, `src/consistency.rs:64-94`).
3. **Uncertainty is sometimes explicit.** Missing merge-base and inferred or
   unknown validation are rendered as such (`src/git.rs:129-144`,
   `src/validate.rs:26-29`, `src/validate.rs:79-86`).
4. **The implementation is compact and testable.** External commands route
   through a runner, output ordering is tested, and the baseline suite is broad
   for the current command surface (`src/command.rs:8-15`,
   `tests/cli_context.rs`, `tests/cli_verify.rs`, `tests/git_state.rs`).
5. **Sessions and handoffs stay local and bounded.** Per-session packets avoid
   one obvious concurrent-writer collision, handoffs are explicit append-only
   JSONL, and no hidden service is introduced (`src/session.rs:86-134`,
   `src/handoff.rs:58-93`).

## Ten most important missing capabilities

1. A fail-safe preparation gate that refuses launch or explicitly requires an
   override when Git state or packet freshness is unsafe.
2. Verification of the authoritative per-session packet, rather than always
   the last-writer-wins repository packet.
3. A complete, lossless Git model: divergence, operation progress, index flags,
   submodules, sparse checkout, worktrees, unusual/non-UTF-8 paths, and evidence
   failures.
4. First-class task and intent evidence with confidence and provenance.
5. Scoped instruction resolution with directory inheritance, precedence,
   conflict, applicability, and staleness.
6. A multi-root/mixed-language repository model instead of one root-level
   `RepoKind` enum.
7. Structured validation requirements stating trigger, coverage, omissions,
   cwd, environment, safety, cost, and authority.
8. Minimal validation result/evidence capture distinguishing planned,
   attempted, passed, failed, skipped, blocked, unknown, and stale.
9. A durable run/handoff record containing entry baseline, agent-attributable
   delta, assumptions, omissions, validation evidence, and completion state.
10. An explicit machine contract for every JSON surface, including schemas,
    versions, partial/degraded results, path encoding, and stable error output.

## Three systemic risks

1. **Warnings do not compose into a preparation verdict.** `run` detects stale
   or unsafe entry state, logs a warning, and launches anyway
   (`src/cli.rs:673-703`, `src/cli.rs:755-759`). Consumers can mistake
   “packet emitted” for “safe to work.”
2. **Absence and failure collapse together.** Best-effort Git calls return
   `Option`; a failed branch diff becomes an empty vector, and malformed session
   markers disappear from reports (`src/git.rs:146-159`, `src/git.rs:261-267`,
   `src/sessions.rs:147-174`). Empty output can look like negative evidence.
3. **The model mirrors commands, not the preparation lifecycle.** There are
   good structs for current renderings, but no first-class Task, Instruction,
   ValidationRequirement/Result, Evidence, Coverage, or Uncertainty. The result
   is locally polished output without an end-to-end claim that the agent is
   prepared.

## Maturity summary

Toimija reliably answers “what does ordinary local Git state look like, and
what cheap hints follow from it?” It does not yet reliably answer “is this agent
prepared to edit this real repository, what is unknown, and what evidence will
make the work trustworthy?” The next release should strengthen correctness and
honesty before adding languages, remote coordination, or richer provenance.

