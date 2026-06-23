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
    run_clippy()?;
    // Test gate: the suite must be green before the commit lands. Runs after
    // clippy so the cheaper lint failures surface first.
    run_tests()?;
    Ok(())
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

/// Run a `cargo` subcommand and abort the commit if it exits non-zero.
fn run_gate(args: &[&str], what: &str) -> Result<()> {
    let status = Command::new(cargo()).args(args).status()?;
    if !status.success() {
        bail!("{what}; commit aborted. Fix the problem and commit again.");
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
    match std::env::args().nth(1).as_deref() {
        Some("install-pre-commit-hook") => {
            install_self_as_hook(&locate_project_root()?)?;
            eprintln!("Installed git pre-commit hook (.git/hooks/pre-commit).");
            eprintln!(
                "It runs rustfmt on staged crates, re-stages, then clippy -D warnings and tests."
            );
        }
        other => {
            if let Some(arg) = other {
                eprintln!("Unknown task: {arg}\n");
            }
            eprintln!("toimija dev tasks:");
            eprintln!("  cargo xtask install-pre-commit-hook   install the git pre-commit hook");
        }
    }
    Ok(())
}
