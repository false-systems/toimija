//! Snapshot materialization — freeze the working tree as an immutable git tree
//! object, so a worker can read content *at a version* via
//! `git cat-file` instead of live disk. That closes the time-of-check /
//! time-of-use gap: the bytes a reader sees cannot change under it, because it
//! reads frozen git objects, not the mutable worktree.
//!
//! The tree is **tree-identity.v1**: the identity of a working State,
//! computed by the canonical procedure in `crate::tree_identity`.
//! Toimija has one tree algorithm. Every tree it writes
//! — snapshots, leases, repository points, session packets — is that one, and
//! every record that carries a tree names the algorithm beside it.
//!
//! The tree oid is the **content address** of the snapshot, and its identity;
//! the consistency token read beside it is a **freshness check**, which does
//! not see every change to the tree (docs/reference.md, "Repository points"). Git's
//! object database is the store — free dedup
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
use crate::consistency::Consistency;
use crate::tree_identity::{Step, TreeIdentityAlgorithm};

/// The tree-identity algorithm Toimija computes. Named here, so that moving
/// to a later version is a decision made in this file.
pub const TREE_ALGORITHM: TreeIdentityAlgorithm = TreeIdentityAlgorithm::V1;

/// What a stored tree that names no algorithm was computed with: a procedure
/// of Toimija's own that is not tree-identity.v1, in a form nothing recorded.
/// Such a tree keeps
/// exactly the meaning it had when written: it is read as this, never
/// recomputed or relabelled as tree-identity.v1.
pub const LEGACY_TREE_ALGORITHM: &str = "toimija-legacy";

fn legacy_tree_algorithm() -> String {
    LEGACY_TREE_ALGORITHM.to_string()
}

/// The worktree moved while its tree was being written: the token read before
/// the write and the one read after it differ. The tree may hold either state,
/// or one in between, so it is paired with neither token. (The tree itself may
/// not have moved at all: an index-only change moves the token and not the
/// tree. What this knows is that the token moved, and it says only that.)
#[derive(Debug, thiserror::Error)]
#[error(
    "the worktree changed while the tree was being written: the consistency token read after \
     the write differs from the one read before, so which state the tree holds is unknown"
)]
pub struct WorktreeMoved;

/// Whether `error` is, anywhere in its chain, a [`WorktreeMoved`].
pub fn is_worktree_moved(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<WorktreeMoved>())
}

/// Write the tree of the repository `read` was read from and return it, but
/// only as the tree of the state whose consistency token is `token`: the token
/// is read again once the tree is written, and a different one is a
/// [`WorktreeMoved`] error. Every caller that pairs a tree with a token
/// (snapshots, repository points, session packets) goes through here.
///
/// The re-read is one git process ([`crate::git::token_state_at`]), reusing
/// `read`'s root and git dir. It sees only what the token fingerprints, and
/// not a change undone before it reads (A → B → A); see docs/reference.md,
/// "Repository points", for what the token does not observe.
pub fn tree_at_token(
    runner: &impl CommandRunner,
    worktree: &crate::git::Worktree,
    read: &crate::git::GitState,
    token: &str,
) -> Result<String> {
    let tree = materialize(runner, worktree)?;
    let mut settled = crate::git::token_state_at(runner, &read.repo_root, read.git_dir.clone())?;
    crate::context::normalize_git_state(&mut settled);
    if Consistency::fingerprint(&settled).token != token {
        return Err(WorktreeMoved.into());
    }
    Ok(tree)
}

/// Materialize the working tree as its tree-identity.v1 git tree object and
/// return the oid: every tracked and untracked non-ignored path, without the
/// generated namespaces `.toimija`, `.teko` and `.sykli`, with
/// `.toimija/repo.yaml` forced in even when ignored. Uses a throwaway index, so
/// the real index is untouched.
pub fn materialize(runner: &impl CommandRunner, worktree: &crate::git::Worktree) -> Result<String> {
    let repo_root = &worktree.root;
    // A *unique* throwaway index per run: concurrent `toimija snapshot`
    // invocations in one repo must not clobber or delete each other's index.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let git_dir = match &worktree.absolute_git_dir {
        Some(git_dir) => git_dir.clone(),
        None => git_dir(runner, repo_root)?,
    };
    let index = git_dir.join(format!(
        "toimija-snapshot-{}-{nanos}.index",
        std::process::id()
    ));

    let tree = materialize_into(runner, repo_root, &index);
    // Always remove the throwaway index — on success and on failure.
    let _ = std::fs::remove_file(&index);
    tree
}

/// Run the canonical procedure, step by step as the plan returns it,
/// against the scratch `index`. The steps are the plan's; the running is
/// here: the process runner, the error handling. A step this function does
/// not handle fails to compile here, so a changed plan cannot be run
/// half-understood.
fn materialize_into(runner: &impl CommandRunner, repo_root: &Path, index: &Path) -> Result<String> {
    let mut tree = None;
    for step in TREE_ALGORITHM.plan() {
        match step {
            // Tolerated: an unborn HEAD has no tracked files yet.
            Step::SeedFromHead => {
                let _ = run_git(runner, repo_root, index, &["read-tree", "HEAD"]);
            }
            Step::OverlayWorktree { excludes } => {
                let mut args = vec!["add", "-A", "--"];
                args.extend(excludes.iter().map(String::as_str));
                run_git(runner, repo_root, index, &args)?;
            }
            // A failure here would leave generated state in the tree, so it is
            // an error, never ignored.
            Step::DropNamespaces { paths } => {
                let mut args = vec!["rm", "--cached", "-r", "-q", "--ignore-unmatch"];
                args.extend(paths.iter().map(String::as_str));
                run_git(runner, repo_root, index, &args)?;
            }
            Step::ForceAddConfig { path } => {
                if repo_root.join(&path).is_file() {
                    run_git(runner, repo_root, index, &["add", "-f", "--", &path])?;
                }
            }
            Step::WriteTree => {
                tree = Some(
                    run_git(runner, repo_root, index, &["write-tree"])?
                        .trim()
                        .to_string(),
                );
            }
        }
    }
    tree.with_context(|| format!("{} wrote no tree", TREE_ALGORITHM.id()))
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
    /// The algorithm `tree` was computed with. A marker written before Toimija
    /// recorded one reads as [`LEGACY_TREE_ALGORITHM`].
    #[serde(default = "legacy_tree_algorithm")]
    pub tree_algorithm: String,
    pub token: String,
    /// The version of `token`. A marker written before Toimija recorded one
    /// has none, and none is claimed for it: its token is of an earlier
    /// version, whichever that toimija computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_algorithm: Option<String>,
    pub pid: u32,
    pub created_at: u64,
}

/// Whether `id` may name a lease. The id becomes a file name under
/// `.toimija/snapshots/` and a ref name, so the rule is about path safety:
/// 1-128 ASCII letters, digits, `.`, `-`, or `_`, not starting with `.`.
/// That refuses `.`, `..`, hidden names, separators (`/`, `\`), and NUL,
/// and still accepts the ids earlier versions minted, and dotted ones they
/// took from `TOIMIJA_SESSION` such as `my.session`, so those leases can be
/// released.
pub fn is_valid_lease_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn ensure_valid_lease_id(id: &str) -> Result<()> {
    if !is_valid_lease_id(id) {
        bail!(
            "invalid snapshot lease id `{id}`: expected 1-128 ASCII letters, digits, `.`, `-` \
             or `_`, not starting with `.`"
        );
    }
    Ok(())
}

/// The git ref that pins a lease's tree.
pub fn lease_ref(id: &str) -> String {
    format!("refs/toimija/snapshots/{id}")
}

/// Acquire a lease on `tree`: pin it with a git ref so `git gc` retains it while
/// held, and record a marker. The "active reader refcount that blocks
/// reclamation" is git reachability, cross-process.
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
        tree_algorithm: TREE_ALGORITHM.id().to_string(),
        token: token.to_string(),
        token_algorithm: Some(crate::consistency::CONSISTENCY_ALGORITHM.to_string()),
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
    use crate::command::CommandOutput;
    use std::cell::RefCell;

    /// Records every git call; answers `write-tree` with a tree oid and
    /// everything else with an empty success.
    #[derive(Default)]
    struct RecordingRunner {
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.run_with_env(cwd, program, args, &[])
        }

        fn run_with_env(
            &self,
            _cwd: &Path,
            program: &str,
            args: &[&str],
            _env: &[(&str, &std::ffi::OsStr)],
        ) -> std::io::Result<CommandOutput> {
            assert_eq!(program, "git");
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            let stdout = if args == ["write-tree"] {
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904\n"
            } else {
                ""
            };
            Ok(CommandOutput {
                status: 0,
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    /// tree-identity.v1 as it is frozen, written out literally: these are
    /// the git operations, in this order, that Toimija must run — and the
    /// `.sykli` namespace is in both lists.
    #[test]
    fn materialize_runs_tree_identity_v1_step_for_step() {
        let repo = tempfile::tempdir().unwrap();
        let expected = |force_add: bool| {
            let mut calls: Vec<Vec<&str>> = vec![
                vec!["rev-parse", "--absolute-git-dir"],
                vec!["read-tree", "HEAD"],
                vec![
                    "add",
                    "-A",
                    "--",
                    ":(exclude,top,glob)[.]toimija/**",
                    ":(exclude,top,glob)[.]teko/**",
                    ":(exclude,top,glob)[.]sykli/**",
                ],
                vec![
                    "rm",
                    "--cached",
                    "-r",
                    "-q",
                    "--ignore-unmatch",
                    ".toimija",
                    ".teko",
                    ".sykli",
                ],
            ];
            if force_add {
                calls.push(vec!["add", "-f", "--", ".toimija/repo.yaml"]);
            }
            calls.push(vec!["write-tree"]);
            calls
        };

        // No repo.yaml: nothing to force in.
        let runner = RecordingRunner::default();
        let tree = materialize(
            &runner,
            &crate::git::Worktree::at_root(repo.path().to_path_buf()),
        )
        .unwrap();
        assert_eq!(tree, "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
        assert_eq!(*runner.calls.borrow(), expected(false));

        // With one, it is forced in after the namespaces are dropped.
        std::fs::create_dir_all(repo.path().join(".toimija")).unwrap();
        std::fs::write(repo.path().join(".toimija/repo.yaml"), "repo: {}\n").unwrap();
        let runner = RecordingRunner::default();
        materialize(
            &runner,
            &crate::git::Worktree::at_root(repo.path().to_path_buf()),
        )
        .unwrap();
        assert_eq!(*runner.calls.borrow(), expected(true));
    }

    /// A lease marker written before Toimija named its tree algorithm keeps the
    /// tree it recorded and reads as the legacy procedure's; a new one names
    /// tree-identity.v1, and both survive a round trip unchanged.
    #[test]
    fn a_lease_marker_without_an_algorithm_reads_as_legacy() {
        let old = r#"{"id":"1782951227-52574","tree":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                      "token":"sha256:abc","pid":52574,"created_at":1782951227}"#;
        let lease: SnapshotLease = serde_json::from_str(old).unwrap();
        assert_eq!(lease.tree, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(lease.tree_algorithm, LEGACY_TREE_ALGORITHM);
        assert_eq!(lease.token, "sha256:abc");
        assert_eq!(lease.token_algorithm, None, "no version is claimed for it");
        let again: SnapshotLease =
            serde_json::from_str(&serde_json::to_string(&lease).unwrap()).unwrap();
        assert_eq!(again, lease);
        assert!(
            !serde_json::to_string(&lease)
                .unwrap()
                .contains("token_algorithm")
        );

        let new = SnapshotLease {
            tree_algorithm: TREE_ALGORITHM.id().to_string(),
            token_algorithm: Some(crate::consistency::CONSISTENCY_ALGORITHM.to_string()),
            ..lease
        };
        let json = serde_json::to_value(&new).unwrap();
        assert_eq!(json["tree_algorithm"], "tree-identity.v1");
        assert_eq!(json["token_algorithm"], "toimija-consistency-v3");
        assert_eq!(serde_json::from_value::<SnapshotLease>(json).unwrap(), new);
    }

    #[test]
    fn lease_ids_are_names_never_paths() {
        for valid in [
            "1782951227-52574",
            "abc_DEF-123",
            "my.session",
            "a..b",
            &"a".repeat(128),
        ] {
            assert!(is_valid_lease_id(valid), "{valid}");
        }
        for invalid in [
            "",
            ".",
            "..",
            ".hidden",
            "../x",
            "a/b",
            "/abs",
            "a\\b",
            "a\0b",
            "a b",
            &"a".repeat(129),
        ] {
            assert!(!is_valid_lease_id(invalid), "{invalid}");
        }
    }
}
