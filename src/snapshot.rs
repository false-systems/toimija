//! Snapshot materialization — freeze the working tree as an immutable git tree
//! object, so a reader (the Kisko harness) can read content *at a version* via
//! `git cat-file` instead of live disk. That closes the time-of-check /
//! time-of-use gap: the bytes a reader sees cannot change under it, because it
//! reads frozen git objects, not the mutable worktree.
//!
//! The tree oid is the **content address** of the snapshot; the consistency
//! token is its **version**. Git's object database is the store — free dedup
//! (unchanged subtrees share oids), free GC (`git gc` reclaims unreferenced
//! objects), free diff/distribution.
//!
//! This is the one place Toimija writes into `.git` (blob + tree objects, all
//! initially unreferenced). Callers gate it behind a *safe* repo state — no
//! in-progress git operation, no conflicts — so `write-tree` never has to deal
//! with an unmergeable index.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Toimija's own generated outputs, dropped from the snapshot tree so a
/// materialization never captures the packet it is about to describe (mirrors the
/// consistency token's exclusion).
const GENERATED_FILES: &[&str] = &[".toimija/current.md", ".toimija/current.json"];
const GENERATED_DIRS: &[&str] = &[".toimija/sessions"];

/// Materialize the full working tree (tracked + staged + unstaged + untracked,
/// honoring `.gitignore`) as a git tree object; return its oid. Uses a throwaway
/// index, so the real index is untouched. Toimija's generated output is excluded
/// so the tree matches what the consistency token fingerprints.
pub fn materialize(repo_root: &Path) -> Result<String> {
    let index = git_dir(repo_root)?.join("toimija-snapshot.index");
    // Start from a clean throwaway index every time.
    let _ = std::fs::remove_file(&index);

    // Stage the entire worktree, then drop generated output. `--ignore-unmatch`
    // keeps `rm` quiet when the path is gitignored or absent.
    run_git(repo_root, &index, &["add", "-A"])?;
    for file in GENERATED_FILES {
        let _ = run_git(
            repo_root,
            &index,
            &["rm", "--cached", "--ignore-unmatch", file],
        );
    }
    for dir in GENERATED_DIRS {
        let _ = run_git(
            repo_root,
            &index,
            &["rm", "--cached", "-r", "--ignore-unmatch", dir],
        );
    }

    let tree = run_git(repo_root, &index, &["write-tree"])?;
    let _ = std::fs::remove_file(&index);
    Ok(tree.trim().to_string())
}

/// Absolute git dir (handles linked worktrees, where `.git` is a file).
fn git_dir(repo_root: &Path) -> Result<PathBuf> {
    let output = plain_git(repo_root, &["rev-parse", "--absolute-git-dir"])?;
    Ok(PathBuf::from(output.trim()))
}

/// Run `git` with a throwaway `GIT_INDEX_FILE`, capturing stdout.
fn run_git(repo_root: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .env("GIT_INDEX_FILE", index)
        .output()
        .with_context(|| format!("failed to run git {args:?}"))?;
    if !output.status.success() {
        bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `git` against the ambient index (for read-only queries like rev-parse).
fn plain_git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("failed to run git {args:?}"))?;
    if !output.status.success() {
        bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
