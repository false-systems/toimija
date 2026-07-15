# Regression catalogue

Every confirmed defect gets a stable ID, a user-visible guarantee, a minimal
fixture, and a test that survives implementation changes.

| ID | Defect/guarantee | Test | Status |
|---|---|---|---|
| R08-001 | Git paths with spaces/control bytes survive collection/rendering | tests/git_state.rs path cases | protected |
| R08-002 | Inherited GIT_DIR cannot redirect fixture Git commands | tests/git_isolation.rs | protected |
| R09-001 | Staged rename retains old and new paths | tests/git_state.rs staged rename case | protected |
| P0-001 | Staged and unstaged intent must not collapse | tests/qa_p0.rs | protected |
| P0-002 | Merge in progress cannot verify as safe | tests/qa_p0.rs | protected |
| P0-003 | JSON stdout remains parseable with diagnostics | tests/qa_p0.rs | protected |
| P0-004 | Watch observes relevant changes within a bounded deadline | tests/cli_watch.rs | flaky; release blocker — issue #143 |

When a new bug is found, first add the smallest reproducer, then change
production code only if the intended contract says the behaviour is wrong.
