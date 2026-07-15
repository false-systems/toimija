# Final release recommendation

1. **Public preview:** No, not until the watcher flake is explained and the
   P0 matrix is green. A preview can use an explicitly experimental label after
   that gate.
2. **Stable public release:** No. Packaging, upgrade, fuzz, security, scale,
   and cross-platform gates remain open.
3. **Blocking defects/gaps:** missed watch event, incomplete interrupted-Git
   matrix, concurrent-write proof, JSON compatibility policy, hostile-path
   security coverage, package smoke, and performance baselines.
4. **Initial platforms:** Linux and macOS normal non-bare worktrees. Windows
   stays unsupported until a CI runner and filesystem tests exist.
5. **Stable commands:** context, context --json, refresh, current --verify
   --json, validate --plan, and doctor after the P0 gates pass.
6. **Experimental commands:** watch, run, adopt, audit-plan, snapshot,
   sessions, handoff, and platform-sensitive repository shapes.
7. **Honest performance envelope:** only the measured local baseline is known:
   release startup is about 0.29s for version output and the binary is 4.2 MiB.
   No repository-size latency or memory claim is valid yet.
8. **Supported shapes:** ordinary checked-out, non-bare Git repositories with
   a local worktree and available Git; Rust/Elixir plus the currently declared
   minimum language packs.
9. **Unsupported or unqualified shapes:** bare, partial/promisor, sparse,
   submodule-heavy, nested, linked-worktree edge cases, and hostile symlink
   layouts until matrix rows are explicitly green or documented unsupported.
10. **Smallest defensible promise:** Toimija compiles a local Git-aware packet
    for a normal checkout, labels inferred and unavailable data, writes live
    artifacts atomically, and refuses stale or unsafe verification. It does not
    promise complete repository understanding, command execution, or universal
    Git-shape support.
