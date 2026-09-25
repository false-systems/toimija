//! Snapshot materialization — freeze the working tree as an immutable git tree
//! object, so a worker can read content *at a version* via
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
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::command::CommandRunner;

/// Materialize the full working tree (tracked + staged + unstaged + untracked,
/// honoring `.gitignore`) as a git tree object; return its oid. Uses a throwaway
/// index, so the real index is untouched. Toimija's generated output is excluded
/// so the tree matches what the consistency token fingerprints.
pub fn materialize(runner: &impl CommandRunner, repo_root: &Path) -> Result<String> {
    // A *unique* throwaway index per run: concurrent `toimija snapshot`
    // invocations in one repo must not clobber or delete each other's index.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let index = git_dir(runner, repo_root)?.join(format!(
        "toimija-snapshot-{}-{nanos}.index",
        std::process::id()
    ));

    let tree = materialize_into(runner, repo_root, &index);
    // Always remove the throwaway index — on success and on failure.
    let _ = std::fs::remove_file(&index);
    tree
}

fn materialize_into(runner: &impl CommandRunner, repo_root: &Path, index: &Path) -> Result<String> {
    // Seed the scratch index from HEAD first, so *tracked* files are present even
    // when they match `.gitignore` — then overlay the worktree with `add -A`.
    // Into an empty index, `add -A` treats every path as untracked and applies
    // `.gitignore`, which would drop committed-but-ignored content from the tree.
    // `read-tree` errors harmlessly on an unborn HEAD (no tracked files yet).
    let _ = run_git(runner, repo_root, index, &["read-tree", "HEAD"]);
    // Exclude generated namespaces before Git can stat a disappearing atomic
    // write. `[.]` avoids treating an ignored directory as an explicit path.
    run_git(
        runner,
        repo_root,
        index,
        &[
            "add",
            "-A",
            "--",
            ":(exclude,top,glob)[.]toimija/**",
            ":(exclude,top,glob)[.]teko/**",
        ],
    )?;
    let _ = run_git(
        runner,
        repo_root,
        index,
        &[
            "rm",
            "--cached",
            "-r",
            "--ignore-unmatch",
            ".toimija",
            ".teko",
        ],
    );
    if repo_root.join(".toimija/repo.yaml").is_file() {
        run_git(
            runner,
            repo_root,
            index,
            &["add", "-f", "--", ".toimija/repo.yaml"],
        )?;
    }
    Ok(run_git(runner, repo_root, index, &["write-tree"])?
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

/// Whether `id` may name a lease: one to 128 ASCII letters, digits, `-` or
/// `_`. The id becomes both a file name under `.toimija/snapshots/` and a ref
/// name, so anything that could be a path — `..`, a separator, an absolute
/// path — is refused. Minted ids and session ids (`<unix-secs>-<pid>`) fit.
pub fn is_valid_lease_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn ensure_valid_lease_id(id: &str) -> Result<()> {
    if !is_valid_lease_id(id) {
        bail!("invalid snapshot lease id `{id}`: expected 1-128 ASCII letters, digits, `-` or `_`");
    }
    Ok(())
}

/// The git ref that pins a lease's tree.
pub fn lease_ref(id: &str) -> String {
    format!("refs/toimija/snapshots/{id}")
}

/// Acquire a lease on `tree`: pin it with a git ref so `git gc` retains it while
/// held, and record a marker. This is AHTI's `begin_snapshot` — but the "active
/// reader refcount that blocks reclamation" is git reachability, cross-process.
pub fn acquire_lease(
    runner: &impl CommandRunner,
    repo_root: &Path,
    id: &str,
    tree: &str,
    token: &str,
) -> Result<SnapshotLease> {
    ensure_valid_lease_id(id)?;
    // Checked before the ref exists, so a refusal leaves nothing pinned.
    let dir = repo_root.join(SNAPSHOTS_SUBDIR);
    crate::owned::ensure_dir(repo_root, &dir).context("failed to record the snapshot lease")?;
    plain_git(runner, repo_root, &["update-ref", &lease_ref(id), tree])?;
    let lease = SnapshotLease {
        id: id.to_string(),
        tree: tree.to_string(),
        token: token.to_string(),
        pid: std::process::id(),
        created_at: now_secs(),
    };
    let json = serde_json::to_string_pretty(&lease)?;
    let path = dir.join(format!("{id}.json"));
    crate::owned::replace(repo_root, &path, format!("{json}\n").as_bytes())?;
    Ok(lease)
}

/// Release a lease: delete the ref (git gc may now reclaim the tree if nothing
/// else references it) and remove the marker. Idempotent. An id that is not a
/// valid lease id is refused before anything is touched.
pub fn release_lease(runner: &impl CommandRunner, repo_root: &Path, id: &str) -> Result<()> {
    ensure_valid_lease_id(id)?;
    let marker = repo_root.join(SNAPSHOTS_SUBDIR).join(format!("{id}.json"));
    // Refuse a symlinked `.toimija/snapshots` before touching the ref.
    crate::owned::dir_exists(repo_root, marker.parent().expect("a marker has a parent"))
        .context("failed to release the snapshot lease")?;
    // Ignore a missing ref so release is idempotent.
    let _ = plain_git(runner, repo_root, &["update-ref", "-d", &lease_ref(id)]);
    crate::owned::remove_file(repo_root, &marker)
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
fn git_dir(runner: &impl CommandRunner, repo_root: &Path) -> Result<PathBuf> {
    let output = plain_git(runner, repo_root, &["rev-parse", "--absolute-git-dir"])?;
    Ok(PathBuf::from(output.trim()))
}

/// Run `git` with a throwaway `GIT_INDEX_FILE`, capturing stdout.
fn run_git(
    runner: &impl CommandRunner,
    repo_root: &Path,
    index: &Path,
    args: &[&str],
) -> Result<String> {
    let output = runner
        .run_with_env(
            repo_root,
            "git",
            args,
            &[("GIT_INDEX_FILE", index.as_os_str())],
        )
        .with_context(|| format!("failed to run git {args:?}"))?;
    if output.status != 0 {
        bail!("git {args:?} failed: {}", output.stderr_lossy().trim());
    }
    Ok(output.stdout_lossy().into_owned())
}

/// Run `git` against the ambient index (for read-only queries like rev-parse).
fn plain_git(runner: &impl CommandRunner, repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = runner
        .run(repo_root, "git", args)
        .with_context(|| format!("failed to run git {args:?}"))?;
    if output.status != 0 {
        bail!("git {args:?} failed: {}", output.stderr_lossy().trim());
    }
    Ok(output.stdout_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_ids_are_names_never_paths() {
        for valid in ["1782951227-52574", "abc_DEF-123", &"a".repeat(128)] {
            assert!(is_valid_lease_id(valid), "{valid}");
        }
        for invalid in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "/abs",
            "a\\b",
            "a.json",
            "a b",
            &"a".repeat(129),
        ] {
            assert!(!is_valid_lease_id(invalid), "{invalid}");
        }
    }
}
