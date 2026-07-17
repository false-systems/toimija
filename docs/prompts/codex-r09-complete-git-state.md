# Toimija — R09: complete the supported local Git/worktree state

You are Codex in the `toimija` crate. The 2026-07-10 audit
(`docs/audit/2026-07-10/`) found that Toimija's Git model is honest about what
it parses but silent about what it does not: submodule movement can leave the
consistency token unchanged (TA-004, critical), divergence is invisible because
`# branch.ab` is parsed past (TA-007), and sparse checkout, index flags, and
linked-worktree identity do not exist in `GitState` at all (TA-008). Close work
package **R09** from `docs/audit/2026-07-10/J-remediation-program.md`: freshness
changes for every supported state transition; unsupported advanced state is
explicit, never presented as an ordinary clean checkout. `cargo fmt --check`,
clippy `-D warnings`, `cargo test`, `toimija doctor --strict` green at every
commit.

Sequencing — check before you start:

- **R08 (#127, lossless paths) must be merged.** You are building on
  `parse_porcelain_v2_bytes` and `path_codec`; branch from fresh `origin/main`
  after it lands.
- **R10 (collection coverage) must be landed.** R09's degraded/unsupported
  reporting is expressed through R10's `Coverage { status, reason, source }`
  values. If R10 has not landed, **stop and do R10 first** (it is a medium
  package; J-doc says "land before R09/R13"). Do not invent a parallel
  coverage abstraction inside R09.

Claim first: `gh issue create` titled "R09: complete Git/worktree state",
assign `@me`, branch `feat/git-worktree-state` from fresh `origin/main`.

## What the audit showed (verified)

- `parse_porcelain_v2_bytes` handles `# branch.head` and `# branch.upstream`
  but skips `# branch.ab`, so "two ahead / one behind" reads identically to
  "in sync" (TA-007).
- The porcelain v2 submodule field (`<sub>`, e.g. `SCMU`) is discarded by the
  fixed-position splits in `parse_changed_record`; the consistency fingerprint
  records only path/type/mode for a directory, so a submodule HEAD bump or
  inner dirt can leave the v2 token identical (TA-004).
- `GitState` has no worktree gitdir/common-dir, gitlink OIDs, sparse-checkout,
  intent-to-add, skip-worktree, or assume-unchanged fields; a sparse worktree
  with hidden index flags renders as a complete ordinary checkout (TA-008).

## Locked decisions

- **F-1 — worktree identity.** Capture `git rev-parse --git-dir
  --git-common-dir` (already partially collected) into explicit fields:
  `worktree_git_dir`, `common_git_dir`, and a derived `is_linked_worktree`.
  Two linked worktrees must report distinct worktree/gitdir identity and a
  shared common-dir (H-11). All Git runs stay on the hardened `CommandRunner`
  (R05) — no raw `Command::new("git")`.
- **F-2 — submodules are first-class observations.** Keep the porcelain
  `<sub>` flags per changed entry; collect index gitlink OIDs
  (`git ls-files -s -z`, mode `160000`) and, when readable, each submodule's
  worktree HEAD. A submodule HEAD, dirt, or index-gitlink change MUST change
  the v3 token (H-10). An unreadable submodule is degraded coverage, never
  silence.
- **F-3 — index and sparse flags are reported or degraded, never hidden.**
  Observe intent-to-add (porcelain `.A`), skip-worktree and assume-unchanged
  (`git ls-files -v -z`), and sparse-checkout status (`core.sparseCheckout` +
  `git sparse-checkout list` when enabled). Supported observations land in
  `GitState`; anything detected but not modeled becomes explicit degraded
  coverage with a reason (H-9). No negative claim from failed collection
  (J-doc rule 1).
- **F-4 — divergence is captured, not narrated.** Parse `# branch.ab` from the
  existing `-z` stream into `ahead`/`behind`; record upstream ref OID when
  resolvable. Remote freshness is always `unknown_without_fetch` — Toimija
  does not fetch (H-5). Data capture only: renaming/reframing how branch facts
  are *presented* is R11; do not rename existing JSON fields here (additive
  only, per J-doc rule 4).
- **F-5 — consistency bumps to v3.** The v3 algorithm pins the new
  freshness-bearing observations: gitlink OIDs and submodule dirt, index
  flags, sparse status. Branch ahead/behind stays out of the token (HEAD OID
  already covers local commits; upstream movement is not working-tree state).
  The verifier keeps reading v2 tokens by v2 rules — an old packet verifies or
  goes stale, it never becomes "unsupported" retroactively. New packets emit
  v3 only.
- **F-6 — no ignored-file scanning.** Completing the state model does not mean
  hashing ignored files; state their coverage policy instead (R09 spec).

## Forbidden moves

- No network, no fetch, no libgit2/gix dependency — Git CLI plumbing through
  the hardened runner only.
- No generic "Git facts framework"; extend `GitState`/`ParsedStatus` and the
  consistency inputs directly.
- Never let a detected-but-unsupported state (odd sparse mode, unreadable
  gitlink) collapse to an empty list or a clean claim — degrade or block.
- Don't touch branch-fact wording or "intent" framing (R04/R11 own that).
- One behavioral promise per PR: if you split, the v3 token bump lands exactly
  once, in the PR that completes its inputs — never a token algorithm that
  changes twice.

## Acceptance

- [ ] H-5: a branch two ahead / one behind reports exact counts, local ref
      OIDs, and `remote_freshness: unknown_without_fetch` in JSON.
- [ ] H-9: intent-to-add, assume-unchanged, skip-worktree, and sparse checkout
      are each either structured observations or explicit degraded coverage —
      proven by live-Git fixtures, not mocks.
- [ ] H-10: changing a submodule's HEAD, its inner dirt, or its index gitlink
      each changes the v3 token and the structured state.
- [ ] H-11: two linked worktrees report distinct worktree/gitdir and shared
      common-dir identity.
- [ ] Every material state transition above changes the v3 token **or**
      produces degraded/blocked coverage — no third outcome.
- [ ] Old v2 packets still verify by v2 rules; the schema/docs note the v3
      algorithm and its added inputs (`docs/joskus-contract.md`, README).
- [ ] fmt · clippy `-D warnings` · test · `doctor --strict` green.

When this lands, M1 (trustworthy substrate) needs only R11 and R12, and R24
(cross-worktree awareness) is unblocked. Update the issue with which H-tests
became `tests/git_state.rs` fixtures so the audit register can be closed
against evidence.
