//! Toimija developer tasks.
//!
//! This binary doubles as the git `pre-commit` hook (via `devx-pre-commit`).
//! When git invokes it, `argv[0]` ends with `pre-commit`, so it runs the hook
//! logic; otherwise it acts as a small dev CLI.
//!
//! Install the hook with:
//!
//! ```text
//! cargo xtask install-pre-commit-hook
//! ```

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};
use devx_pre_commit::{install_self_as_hook, locate_project_root, PreCommitContext};

fn run_hook() -> Result<()> {
    let mut ctx = PreCommitContext::from_git_diff(locate_project_root()?)?;
    // Never touch generated code, only hand-written sources.
    ctx.retain_staged_files(|path| {
        path.components()
            .all(|component| component.as_os_str() != OsStr::new("generated"))
    });
    // Format the staged Rust crates and re-stage the results so the commit
    // records the formatted code rather than rejecting it.
    ctx.rustfmt()?;
    ctx.stage_new_changes()?;
    // Lint gate: a commit must not introduce clippy warnings. This mirrors the
    // CI step (`cargo clippy --all-targets -- -D warnings`) so failures surface
    // locally instead of after a push.
    aborting_commit(run_clippy())?;
    // Test gate: the suite must be green before the commit lands. Runs after
    // clippy so the cheaper lint failures surface first.
    aborting_commit(run_tests())?;
    Ok(())
}

/// Hook-path phrasing: the same gate failure aborts a commit rather than a
/// standalone `cargo xtask gate` run.
fn aborting_commit(result: Result<()>) -> Result<()> {
    result.map_err(|error| error.context("commit aborted. Fix the problem and commit again."))
}

/// The full repo gate behind one entrypoint (guardrails R-R.1): the same three
/// checks `sykli.json` declares, in cheapest-first order — plus xtask's own tests,
/// which `default-members = ["."]` hides from plain `cargo test` (xtask is
/// already compiled to run this, so its tests cost almost nothing here). A
/// human, CI, and the harness run this identically as `cargo xtask gate`.
fn run_full_gate() -> Result<()> {
    // `cargo xtask` aliases work from any workspace subdir; unpinned, a
    // `cargo test` run from inside `xtask/` would gate the xtask package
    // instead of toimija. (Not `locate_project_root`: that returns `GIT_DIR`
    // verbatim when set, which would point the hook path into `.git/`.)
    std::env::set_current_dir(repo_toplevel()?)?;
    run_gate(&["fmt", "--check"], "formatting differs from rustfmt")?;
    run_clippy()?;
    run_tests()?;
    run_gate(&["test", "-p", "xtask"], "xtask tests failed")?;
    Ok(())
}

/// The repo root via `git rev-parse --show-toplevel`, with the redirecting git
/// env scrubbed so an inherited `GIT_DIR` can never steer it.
fn repo_toplevel() -> Result<PathBuf> {
    let mut command = Command::new("git");
    command.args(["rev-parse", "--show-toplevel"]);
    for var in REDIRECTING_GIT_ENV {
        command.env_remove(var);
    }
    let output = command.output()?;
    if !output.status.success() {
        bail!("not inside a git repository — run `cargo xtask gate` from the repo");
    }
    Ok(PathBuf::from(
        String::from_utf8(output.stdout)?.trim_end().to_string(),
    ))
}

fn run_clippy() -> Result<()> {
    run_gate(
        &["clippy", "--all-targets", "--", "-D", "warnings"],
        "clippy reported issues",
    )
}

fn run_tests() -> Result<()> {
    run_gate(&["test"], "tests failed")
}

/// Git environment variables that redirect git at a specific repo, index, or
/// worktree. Git exports these when it invokes a hook; if they leak into the
/// gates' `cargo` process, the integration tests' `git` calls and the
/// `toimija`-under-test both act on the real repo instead of their tempdirs —
/// once observed flipping `core.bare = true` and bricking a sibling worktree.
const REDIRECTING_GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_CEILING_DIRECTORIES",
];

/// Run a `cargo` subcommand and fail if it exits non-zero. The git
/// redirection env is scrubbed so the gates run hermetically against tempdirs.
fn run_gate(args: &[&str], what: &str) -> Result<()> {
    let mut command = Command::new(cargo());
    command.args(args);
    for var in REDIRECTING_GIT_ENV {
        command.env_remove(var);
    }
    let status = command.status()?;
    if !status.success() {
        bail!("{what}");
    }
    Ok(())
}

/// Honour the `CARGO` env var git/cargo sets, falling back to `cargo` on PATH.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn main() -> Result<()> {
    // Git runs the hook as `.git/hooks/pre-commit`, so argv[0] contains it.
    if std::env::args()
        .next()
        .is_some_and(|arg0| arg0.contains("pre-commit"))
    {
        return run_hook();
    }
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("install-pre-commit-hook") => {
            install_self_as_hook(&locate_project_root()?)?;
            eprintln!("Installed git pre-commit hook (.git/hooks/pre-commit).");
            eprintln!(
                "It runs rustfmt on staged crates, re-stages, then clippy -D warnings and tests."
            );
        }
        Some("gate") => run_full_gate()?,
        other => {
            eprintln!("toimija dev tasks:");
            eprintln!("  cargo xtask gate                      run the full gate: fmt --check, clippy -D warnings, tests");
            eprintln!("  cargo xtask install-pre-commit-hook   install the git pre-commit hook");
            // Fail closed: a caller that treats the exit code as a verdict (the
            // harness gate, CI) must never mistake "task not found" for a pass.
            if let Some(arg) = other {
                bail!("unknown task: {arg}");
            }
        }
    }
    Ok(())
}
