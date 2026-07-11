# E. Scenario audit

## 1. Clean Rust repository, one-file bug fix

- **Current:** detects Rust, clean branch/worktree, changed source/test hints,
  inferred Cargo plan, and history relevance.
- **Misses / message:** no task contract, scoped instructions, ownership, actual
  dependency impact, or CI requirements; still presents a polished packet.
- **Misleading path:** inferred standard Cargo commands look like complete proof.
- **Correct:** ready-with-warnings only after task and applicable policy are
  resolved; state which validation requirements are discovered versus assumed.

## 2. Rust workspace with changes across two crates

- **Current:** nearest manifests can yield two focused `cargo test -p` commands;
  whole workspace defaults remain.
- **Misses / message:** Cargo workspace membership/features/build scripts and
  reverse dependencies are not discovered.
- **Misleading path:** both crate tests pass while a dependent crate or custom
  workspace gate breaks.
- **Correct:** native Cargo metadata maps packages/dependencies/targets; focused
  checks are explicitly partial and full requirements remain visible.

## 3. Elixir umbrella with shared dependency changes

- **Current:** app-local `.ex/.exs` changes map to app test paths.
- **Misses / message:** root `mix.exs`, `mix.lock`, config, and shared library
  changes map to no owning app; plan defaults to global `mix test`.
- **Misleading path:** focused set can understate umbrella-wide impact.
- **Correct:** root/shared triggers mark every affected app or deliberately fall
  back with a coverage reason.

## 4. Partially staged file with unrelated local edits

- **Current:** preserves the file in staged and unstaged lists; snapshot refuses
  mixed state (`tests/cli_snapshot.rs:137-151`).
- **Misses / message:** task/branch surface deduplicates path identity and cannot
  say which hunks belong to the task or predated the run.
- **Misleading path:** aggregate read-order treats both intents as one change.
- **Correct:** preserve layers, capture entry baseline, flag mixed intent, and
  avoid attributing unstaged content to staged/branch intent.

## 5. Merge conflict in progress

- **Current:** conflict path and merge operation enter consistency; verify and
  snapshot refuse.
- **Misses / message:** operation progress/index stages and conflict sides are
  absent; `run` merely warns then launches.
- **Misleading path:** agent receives normal preparation around an unsafe state.
- **Correct:** block broad launch by default; provide conflict-specific packet
  with stage 1/2/3 evidence and explicit recovery purpose override.

## 6. Rebase in progress

- **Current:** detects rebase directory in consistency.
- **Misses / message:** current step, todo/done, original head, onto, stopped
  commit, and rewritten range; `run` launches after warning.
- **Correct:** block or enter a rebase-recovery mode with operation evidence.

## 7. Detached HEAD in CI

- **Current:** detects detached HEAD and warns; no merge-base may be available.
- **Misses / message:** CI base/head refs, event metadata, shallow boundary, and
  intended comparison.
- **Misleading path:** “broad work risky” is noise for normal CI while branch
  intent may be wrong.
- **Correct:** distinguish CI observation from interactive editing; accept an
  explicit base/task contract and report remote freshness unknown.

## 8. Large monorepo with mixed Rust and Elixir

- **Current:** root Cargo marker selects Rust; Rust adapter and recursive watch
  cover the whole root.
- **Misses / message:** Elixir and nested roots vanish; all untracked files and
  changed content may be scanned/hashed.
- **Misleading path:** packet says `kind: Rust` and proposes only Cargo gates.
- **Correct:** project-root inventory, multi-adapter plan, bounded stage budgets,
  and degraded coverage instead of pretending the repo is singular.

## 9. Repository with undocumented custom validation

- **Current:** infers standard language commands unless config has any command.
- **Misses / message:** CI scripts, Make/Just/mise tasks, generated checks,
  services, secrets, and manual steps.
- **Misleading path:** inferred plan looks complete.
- **Correct:** label coverage incomplete, discover authoritative local evidence,
  and require config/human confirmation for unknown gates.

## 10. One agent stops halfway and another continues

- **Current:** optional explicit handoff snapshots current dirt and note; stale
  session may remain.
- **Misses / message:** no automatic run events, entry dirt, attributable delta,
  validation results, assumptions, or completion status.
- **Misleading path:** successor treats all dirt as predecessor work.
- **Correct:** append run events during explicit commands, derive a crash-safe
  handoff, and preserve pre-existing state separately.

## 11. Two agents work on separate machines

- **Current:** no shared state; proposal 0004 is draft.
- **Misses / message:** duplicate work, stale ref observation, host identity, and
  concurrency reconciliation.
- **Misleading path:** local `sessions` empty means “no local marker,” not “no
  other agent.”
- **Correct:** say coverage is local-only. Later, share immutable/append-only
  summaries through explicitly synchronized Git refs without network in Toimija.

## 12. Validation passes locally but required CI checks are unknown

- **Current:** no result capture and no CI requirement inventory.
- **Misleading path:** handoff can carry only suggested focused/fast commands.
- **Correct:** local pass is evidence for named requirements only; unknown CI
  coverage remains blocking or explicitly unresolved.

## 13. Repository contains malicious build instructions

- **Current:** validation strings are printed, not executed; argv avoids shell
  expansion. Snapshot invokes Git directly and can run configured filters.
- **Misses / message:** instruction trust, control-character sanitization, Git
  helper/filter trust, and command safety classification.
- **Correct:** treat repository text/config as untrusted, sanitize display,
  classify commands, and require opt-in before code execution.

## 14. Context budget omits the relevant test or ownership file

- **Current:** fixed section order then tail truncation; only a generic marker.
- **Misses / message:** candidate inventory, mandatory items, omission IDs and
  reasons; ownership is not discovered at all.
- **Correct:** select items before render, preserve mandatory policy/test items,
  and emit a machine-readable omissions manifest.

## 15. Branch intent inference is wrong

- **Current:** compares with guessed trunk and calls file count “Branch intent.”
- **Misses / message:** stacked base, issue/prompt/PR evidence, confidence, and
  contradiction.
- **Correct:** call Git result branch surface; show intent candidates with
  provenance/confidence and ask for task confirmation only when disagreement
  materially changes safe scope.

