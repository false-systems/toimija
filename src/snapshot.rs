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

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Toimija's own generated outputs, dropped from the snapshot tree so a
/// materialization never captures the packet it is about to describe (mirrors the
/// consistency token's exclusion).
const GENERATED_FILES: &[&str] = &[".toimija/current.md", ".toimija/current.json"];
const GENERATED_DIRS: &[&str] = &[".toimija/sessions", ".toimija/snapshots"];

/// Materialize the full working tree (tracked + staged + unstaged + untracked,
/// honoring `.gitignore`) as a git tree object; return its oid. Uses a throwaway
/// index, so the real index is untouched. Toimija's generated output is excluded
/// so the tree matches what the consistency token fingerprints.
pub fn materialize(repo_root: &Path) -> Result<String> {
    // A *unique* throwaway index per run: concurrent `toimija snapshot`
    // invocations in one repo must not clobber or delete each other's index.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let index = git_dir(repo_root)?.join(format!(
        "toimija-snapshot-{}-{nanos}.index",
        std::process::id()
    ));

    let tree = materialize_into(repo_root, &index);
    // Always remove the throwaway index — on success and on failure.
    let _ = std::fs::remove_file(&index);
    tree
}

fn materialize_into(repo_root: &Path, index: &Path) -> Result<String> {
    // Seed the scratch index from HEAD first, so *tracked* files are present even
    // when they match `.gitignore` — then overlay the worktree with `add -A`.
    // Into an empty index, `add -A` treats every path as untracked and applies
    // `.gitignore`, which would drop committed-but-ignored content from the tree.
    // `read-tree` errors harmlessly on an unborn HEAD (no tracked files yet).
    let _ = run_git(repo_root, index, &["read-tree", "HEAD"]);
    // Overlay worktree changes (modifications, additions, deletions), then drop
    // generated output. `--ignore-unmatch` keeps `rm` quiet when absent.
    run_git(repo_root, index, &["add", "-A"])?;
    for file in GENERATED_FILES {
        let _ = run_git(
            repo_root,
            index,
            &["rm", "--cached", "--ignore-unmatch", file],
        );
    }
    for dir in GENERATED_DIRS {
        let _ = run_git(
            repo_root,
            index,
            &["rm", "--cached", "-r", "--ignore-unmatch", dir],
        );
    }
    Ok(run_git(repo_root, index, &["write-tree"])?
        .trim()
        .to_string())
}

/// Directory, relative to the repo root, holding snapshot-lease markers.
pub const SNAPSHOTS_SUBDIR: &str = ".toimija/snapshots";

/// A held snapshot: a session registered as reading `tree` at version `token`.
/// The retention itself is the git ref `refs/toimija/snapshots/<id>` (git gc
/// keeps ref-reachable objects); this marker is awareness metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotLease {
    pub id: String,
    pub tree: String,
    pub token: String,
    pub pid: u32,
    pub created_at: u64,
}

/// The git ref that pins a lease's tree.
pub fn lease_ref(id: &str) -> String {
    format!("refs/toimija/snapshots/{id}")
}

/// Acquire a lease on `tree`: pin it with a git ref so `git gc` retains it while
/// held, and record a marker. This is AHTI's `begin_snapshot` — but the "active
/// reader refcount that blocks reclamation" is git reachability, cross-process.
pub fn acquire_lease(repo_root: &Path, id: &str, tree: &str, token: &str) -> Result<SnapshotLease> {
    plain_git(repo_root, &["update-ref", &lease_ref(id), tree])?;
    let lease = SnapshotLease {
        id: id.to_string(),
        tree: tree.to_string(),
        token: token.to_string(),
        pid: std::process::id(),
        created_at: now_secs(),
    };
    let dir = repo_root.join(SNAPSHOTS_SUBDIR);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let json = serde_json::to_string_pretty(&lease)?;
    let path = dir.join(format!("{id}.json"));
    fs::write(&path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(lease)
}

/// Release a lease: delete the ref (git gc may now reclaim the tree if nothing
/// else references it) and remove the marker. Idempotent.
pub fn release_lease(repo_root: &Path, id: &str) -> Result<()> {
    // Ignore a missing ref so release is idempotent.
    let _ = plain_git(repo_root, &["update-ref", "-d", &lease_ref(id)]);
    let _ = fs::remove_file(repo_root.join(SNAPSHOTS_SUBDIR).join(format!("{id}.json")));
    Ok(())
}

/// Read all lease markers, newest first. A malformed marker is skipped.
pub fn list_leases(repo_root: &Path) -> Result<Vec<SnapshotLease>> {
    let dir = repo_root.join(SNAPSHOTS_SUBDIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()));
        }
    };
    let mut leases = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path)
            && let Ok(lease) = serde_json::from_str::<SnapshotLease>(&raw)
        {
            leases.push(lease);
        }
    }
    leases.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(leases)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
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
