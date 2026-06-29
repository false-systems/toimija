//! Shared helpers for the CLI integration tests.
//!
//! `run_git` and `git_available` spawn the real `git` binary. `run_git`
//! deliberately scrubs the `GIT_*` environment so a fixture can **never** act on
//! the ambient repository.
//!
//! Why this matters: when these tests run under git's own pre-commit hook, git
//! exports `GIT_DIR`/`GIT_INDEX_FILE`/`GIT_WORK_TREE`/... pointing at the real
//! repo. An un-scrubbed `git init` in a tempdir would then mutate that real repo
//! instead of its sandbox — observed in practice flipping `core.bare = true` and
//! bricking a sibling worktree mid-session (worktrees share one `.git`, so the
//! blast radius crosses worktrees). Scrubbing keeps every fixture hermetic.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// Git environment variables that redirect git at a specific repo, index, or
/// worktree. A leaked value (e.g. inherited from a pre-commit hook) would make a
/// fixture's `git` operate on the ambient repo instead of its own `cwd`.
const REDIRECTING_GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_CEILING_DIRECTORIES",
];

/// A `git` command pinned to `cwd` with the redirecting environment scrubbed, so
/// it can only ever act on the repo at `cwd`. Public so the isolation regression
/// test can assert the scrub directly.
pub fn git_at(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd);
    for var in REDIRECTING_GIT_ENV {
        command.env_remove(var);
    }
    command
}

/// Whether `git` is on PATH. Reads `git --version` only; touches no repository.
pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Run `git <args>` in `path`, asserting success. Hermetic: see module docs.
pub fn run_git(path: &Path, args: &[&str]) {
    let output = git_at(path).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
