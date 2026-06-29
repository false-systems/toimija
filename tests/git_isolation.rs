//! Regression test for the fixture git-environment leak.
//!
//! When the suite runs under git's own pre-commit hook, git exports `GIT_DIR`
//! (and friends) pointing at the real repo. A fixture's `git init` that inherits
//! those would mutate the ambient repo instead of its tempdir — once observed
//! flipping `core.bare = true` and bricking a sibling worktree mid-session.
//!
//! `common::run_git` builds its command through `git_at`, which removes those
//! redirecting variables. This test asserts the scrub on the command itself —
//! deterministic, with no global-env mutation (which is `unsafe` on edition
//! 2024 and would race parallel tests anyway).

mod common;

use std::path::Path;

#[test]
fn git_at_scrubs_the_redirecting_environment() {
    let command = common::git_at(Path::new("."));
    // `env_remove` shows up in `get_envs()` as a `(key, None)` override.
    let scrubbed: Vec<String> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect();

    // The variable that actually caused the corruption, plus the rest of the
    // redirecting family, must all be removed.
    for var in [
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        assert!(
            scrubbed.contains(&var.to_string()),
            "{var} must be scrubbed so fixtures cannot escape to the ambient repo"
        );
    }
}
