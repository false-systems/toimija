# Public quality guarantees

Status values mean tested (observable test exists), partial (some coverage
exists), or gap (the behaviour is not release-proven).

| Guarantee | Observable behaviour | Commands | Failure behaviour | Proof/status |
|---|---|---|---|---|
| State separation | staged, unstaged, untracked, ignored, and conflict paths remain distinct | context --json | preserve categories or fail; never claim clean | tests/qa_p0.rs; core categories tested |
| Freshness | packet is bound to the inspected Git/filesystem state | refresh, current --verify | stale or unsafe status, non-zero exit | tests/cli_verify.rs; partial mutation matrix |
| Safe interrupted operations | merge/rebase/etc. are visible and not verified as safe | context, current --verify | explicit unsafe operation | tests/qa_p0.rs; merge only |
| Deterministic structured state | equivalent input yields equivalent JSON semantics and token | context --json | stable ordering; uncertainty remains explicit | tests/qa_p0.rs; partial |
| Atomic persistence | readers see old or new packet, not a partial file | refresh, current | preserve prior artifact on failed replacement | src/atomic_write.rs; race gap |
| Honest validation | proposed commands are labelled as plans and never executed | validate --plan, context | unavailable/inferred/configured is explicit | tests/cli_validate.rs; partial |
| JSON hygiene | JSON stdout parses without log contamination | all --json commands | diagnostics go to stderr | tests/qa_p0.rs; partial inventory |
| Bounded output | budgets truncate explicitly and retain high-value safety sections | context --budget | honest truncation marker | tests/cli_context.rs; partial |
| Path safety | hostile paths do not escape scope or forge headings/commands | context, refresh, run | render safely or fail | existing path tests; symlink gap |
| Distribution | clean packaged binary starts and performs smoke workflow | release artifact | clear install/runtime failure | no current proof; gap |
| Compatibility | prior generated state has a documented read/regenerate policy | current/verify | explicit schema rejection or migration | no version fixtures; gap |

The first five rows are release-blocking. A green line-coverage number cannot
substitute for these observable guarantees.
