# Release test matrix

This is the capability matrix used for release decisions. Implemented means
there is a meaningful test on the branch or in main; Partial means coverage
does not yet prove the full contract.

| ID | Subsystem | Scenario | Level | Fixture/test | Expected behaviour | Platform | Implemented | CI tier | Blocking |
|---|---|---|---|---|---|---|---|---|---|
| P0-01 | Git state | staged + unstaged same path | CLI | tests/qa_p0.rs | both surfaces retained | primary | Yes | PR | Yes |
| P0-02 | Git state | untracked path | CLI | tests/qa_p0.rs | explicit untracked field | primary | Yes | PR | Yes |
| P0-03 | Git operation | conflicted merge | system | tests/qa_p0.rs | operation visible; verify unsafe | primary | Yes | PR | Yes |
| P0-04 | JSON | verbose diagnostics | CLI | tests/qa_p0.rs | parseable stdout; logs stderr | primary | Yes | PR | Yes |
| P0-05 | freshness | content mutation after refresh | CLI | tests/cli_verify.rs | stale non-zero result | primary | Yes | PR | Yes |
| P0-06 | persistence | concurrent refresh/read | component | new race test | no partial packet | all | Gap | scheduled | Yes |
| P0-07 | paths | newline/ANSI/invalid bytes | CLI | tests/git_state.rs | safe encoded output | primary | Partial | PR | Yes |
| P0-08 | writes | .toimija symlink replacement | security | new fixture | refuse; no escape | all | Gap | scheduled | Yes |
| P1-01 | lifecycle | refresh -> verify -> mutate -> refresh | system | new scenario | lifecycle remains distinct | primary | Partial | PR | Yes |
| P1-02 | CLI | every JSON command | CLI | contract suite | schema/streams/exit codes | primary | Partial | PR | Yes |
| P1-03 | Rust | workspace and shared crate | component | fixtures/rust_repo | scope/validation honest | primary | Partial | PR | Yes |
| P1-04 | Elixir | umbrella app/shared config | component | fixtures/elixir_repo | app vs broad scope | primary | Partial | PR | Yes |
| P1-05 | run | fake agent edits and stages | E2E | tests/cli_run.rs | exit state reflects both | primary | Partial | PR | Yes |
| P1-06 | package | release binary smoke | distribution | clean temp install | no source checkout needed | Linux/macOS | Gap | RC | Yes |
| P2-01 | worktree | linked worktrees diverge | system | fixture builder | identity not conflated | primary | Partial | scheduled | No |
| P2-02 | history | shallow clone/no base | component | clone fixture | unavailable is explicit | primary | Partial | PR | No |
| P2-03 | repository | sparse checkout | component | Git fixture | no false completeness | primary | Gap | scheduled | No |
| P2-04 | repository | submodule states | component | Git fixture | gitlink/dirty state explicit | primary | Gap | scheduled | No |
| P2-05 | watch | burst/create/delete/rename | system | tests/cli_watch.rs | bounded eventual refresh | primary | Flaky | PR | Yes |
| P2-06 | concurrency | two refresh writers | stress | generated fixture | safe old/new result | primary | Gap | scheduled | Yes |
| P3-01 | scale | 20/1k/25k/100k files | benchmark | generated repos | budgets recorded | primary | Gap | scheduled | No |
| P3-02 | fuzz | porcelain/path/config parsers | fuzz | corpus targets | no panic/traversal | primary | Gap | scheduled | Yes |
| P3-03 | upgrade | old current JSON/schema | compatibility | version fixtures | policy-consistent result | all | Gap | RC | Yes |
| P3-04 | platform | Linux/macOS/Windows policy | compatibility | CI matrix | declared support honest | matrix | Partial | RC | Yes |

The matrix is intentionally stricter than the existing test count. A test that
only asserts exit zero does not satisfy a row whose guarantee is state or
uncertainty preservation.
