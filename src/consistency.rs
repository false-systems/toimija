//! Packet consistency — a versioned freshness token over the Git state a packet
//! was compiled from.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::git::{ChangeStatus, ChangedFile, GitState};

pub const CONSISTENCY_ALGORITHM: &str = "toimija-consistency-v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consistency {
    pub algorithm: String,
    pub head: HeadState,
    pub index_fingerprint: String,
    pub worktree_fingerprint: String,
    pub untracked_fingerprint: String,
    pub git_operation: GitOperationState,
    pub conflict_state: ConflictState,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HeadState {
    Normal { branch: String, oid: String },
    Detached { oid: String },
    Unborn { branch: Option<String> },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictState {
    pub has_conflicts: bool,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitOperationState {
    None,
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    Unverifiable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Consistent,
    Stale {
        packet_token: String,
        current_token: String,
    },
    UnsupportedConsistencyAlgorithm(String),
    UnsafeGitOperation(GitOperationState),
}

impl Consistency {
    pub fn fingerprint(git: &GitState) -> Consistency {
        let head = head_state(git);
        let git_operation = git_operation_state(git);
        let conflict_state = ConflictState {
            has_conflicts: !git.conflicted_files.is_empty(),
            files: stable_paths(git.conflicted_files.clone()),
        };
        let index_fingerprint = fingerprint_changed("index", &git.repo_root, &git.staged_files);
        let worktree_fingerprint =
            fingerprint_changed("worktree", &git.repo_root, &git.unstaged_files);
        let untracked_fingerprint =
            fingerprint_paths("untracked", &git.repo_root, &git.untracked_files);
        let token = token(
            &head,
            &index_fingerprint,
            &worktree_fingerprint,
            &untracked_fingerprint,
            &git_operation,
            &conflict_state,
        );
        Consistency {
            algorithm: CONSISTENCY_ALGORITHM.to_string(),
            head,
            index_fingerprint,
            worktree_fingerprint,
            untracked_fingerprint,
            git_operation,
            conflict_state,
            token,
        }
    }

    pub fn verify(&self, live: &GitState) -> Verdict {
        if self.algorithm != CONSISTENCY_ALGORITHM {
            return Verdict::UnsupportedConsistencyAlgorithm(self.algorithm.clone());
        }
        let fresh = Consistency::fingerprint(live);
        if fresh.git_operation != GitOperationState::None || fresh.conflict_state.has_conflicts {
            Verdict::UnsafeGitOperation(fresh.git_operation)
        } else if self.token == fresh.token {
            Verdict::Consistent
        } else {
            Verdict::Stale {
                packet_token: self.token.clone(),
                current_token: fresh.token,
            }
        }
    }
}

impl Verdict {
    pub fn is_consistent(&self) -> bool {
        matches!(self, Verdict::Consistent)
    }
}

fn head_state(git: &GitState) -> HeadState {
    match (&git.branch, &git.head_oid, git.detached) {
        (_, Some(oid), true) => HeadState::Detached { oid: oid.clone() },
        (Some(branch), Some(oid), false) => HeadState::Normal {
            branch: branch.clone(),
            oid: oid.clone(),
        },
        (branch, None, _) => HeadState::Unborn {
            branch: branch.clone(),
        },
        _ => HeadState::Unknown,
    }
}

fn git_operation_state(git: &GitState) -> GitOperationState {
    let Some(git_dir) = git_dir_path(git) else {
        return GitOperationState::Unverifiable {
            reason: "git_dir_unavailable".to_string(),
        };
    };
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        GitOperationState::Rebase
    } else if git_dir.join("MERGE_HEAD").exists() {
        GitOperationState::Merge
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        GitOperationState::CherryPick
    } else if git_dir.join("REVERT_HEAD").exists() {
        GitOperationState::Revert
    } else if git_dir.join("BISECT_LOG").exists() {
        GitOperationState::Bisect
    } else {
        GitOperationState::None
    }
}

fn git_dir_path(git: &GitState) -> Option<PathBuf> {
    let git_dir = git.git_dir.as_ref()?;
    if git_dir.is_absolute() {
        Some(git_dir.clone())
    } else {
        Some(git.repo_root.join(git_dir))
    }
}

fn fingerprint_changed(kind: &str, repo_root: &Path, files: &[ChangedFile]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for file in files {
        if is_generated_output(&file.path) {
            continue;
        }
        lines.push(format!(
            "{kind}\t{}\t{}\t{}",
            status(&file.status),
            file.index_entry.as_deref().unwrap_or("worktree"),
            metadata(repo_root, &file.path)
        ));
    }
    hash_lines(lines)
}

fn fingerprint_paths(kind: &str, repo_root: &Path, paths: &[PathBuf]) -> String {
    let mut lines = Vec::new();
    for path in paths {
        if is_generated_output(path) {
            continue;
        }
        lines.push(format!("{kind}\t{}", metadata(repo_root, path)));
    }
    hash_lines(lines)
}

fn token(
    head: &HeadState,
    index: &str,
    worktree: &str,
    untracked: &str,
    operation: &GitOperationState,
    conflicts: &ConflictState,
) -> String {
    let lines = vec![
        format!("algorithm\t{CONSISTENCY_ALGORITHM}"),
        format!("head\t{}", serde_json::to_string(head).unwrap_or_default()),
        format!("index\t{index}"),
        format!("worktree\t{worktree}"),
        format!("untracked\t{untracked}"),
        format!(
            "operation\t{}",
            serde_json::to_string(operation).unwrap_or_default()
        ),
        format!(
            "conflicts\t{}",
            serde_json::to_string(conflicts).unwrap_or_default()
        ),
    ];
    hash_lines(lines)
}

fn hash_lines(mut lines: Vec<String>) -> String {
    lines.sort();
    let mut hasher = Sha256::new();
    for line in lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Fingerprint one path by **content**, not by mtime/size. The same bytes
/// fingerprint identically no matter when the file was touched (no false
/// "changed" from a bare `touch`), and different bytes always differ (no false
/// "unchanged" from a same-size, same-mtime edit). This is what makes the token
/// a trustworthy version key rather than a staleness heuristic.
///
/// `mode` is kept (a `chmod +x` is a real change). Symlinks fingerprint their
/// target, never the file they point at. A directory entry here is a
/// gitlink/submodule — classified, not recursed. Only changed/untracked paths
/// reach this, so the content reads are bounded.
fn metadata(repo_root: &Path, path: &Path) -> String {
    let normalized = normalize_path(path);
    let absolute = repo_root.join(path);
    match fs::symlink_metadata(&absolute) {
        Ok(meta) => {
            let mode = mode(&meta);
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&absolute)
                    .map(|target| normalize_path(&target))
                    .unwrap_or_default();
                format!(
                    "{normalized}\tsymlink\t{mode:o}\t{}",
                    hash_bytes(target.as_bytes())
                )
            } else if meta.is_dir() {
                format!("{normalized}\tdir\t{mode:o}")
            } else {
                let content = match fs::read(&absolute) {
                    Ok(bytes) => hash_bytes(&bytes),
                    Err(_) => "unreadable".to_string(),
                };
                format!("{normalized}\tfile\t{mode:o}\t{content}")
            }
        }
        Err(_) => format!("{normalized}\tmissing"),
    }
}

/// `sha256:<hex>` over raw bytes — the content address of a file (or a symlink
/// target). Toimija's own digest, so it need not match git's blob hashing.
fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(unix)]
fn mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode(_meta: &fs::Metadata) -> u32 {
    0
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn status(status: &ChangeStatus) -> String {
    match status {
        ChangeStatus::Added => "added".to_string(),
        ChangeStatus::Modified => "modified".to_string(),
        ChangeStatus::Deleted => "deleted".to_string(),
        ChangeStatus::Renamed => "renamed".to_string(),
        ChangeStatus::Copied => "copied".to_string(),
        ChangeStatus::Untracked => "untracked".to_string(),
        ChangeStatus::Unknown(value) => format!("unknown:{value}"),
    }
}

/// Whether a repo-relative path is generated state under `.toimija/`. Everything
/// under that namespace is excluded from the digest so harness output
/// (`runs/`), session packets, snapshots, and refresh output cannot
/// self-invalidate the packet. The tracked manifest `.toimija/repo.yaml` is
/// deliberately NOT generated — a change to it is a real change the packet
/// should reflect.
fn is_generated_output(path: &Path) -> bool {
    let mut components = path.components().map(|c| c.as_os_str().to_str());
    if components.next() != Some(Some(".toimija")) {
        return false;
    }
    components.next() != Some(Some("repo.yaml"))
}

fn stable_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut paths = paths;
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{ChangeStatus, ChangedFile};
    use std::path::PathBuf;

    fn git_state() -> GitState {
        GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: Some(PathBuf::from("/repo/.git")),
            branch: Some("feat/x".to_string()),
            head_oid: Some("a1b2c3d4e5".to_string()),
            upstream: None,
            base: None,
            merge_base: Some("0000000".to_string()),
            detached: false,
            dirty: false,
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            conflicted_files: Vec::new(),
            branch_files: Vec::new(),
            recent_commits: Vec::new(),
        }
    }

    fn git_state_in(repo: &Path) -> GitState {
        let mut git = git_state();
        git.repo_root = repo.to_path_buf();
        git.git_dir = Some(repo.join(".git"));
        git
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let git = git_state();
        assert_eq!(
            Consistency::fingerprint(&git),
            Consistency::fingerprint(&git)
        );
    }

    #[test]
    fn identical_state_verifies_as_consistent() {
        let git = git_state();
        let stored = Consistency::fingerprint(&git);
        assert!(stored.verify(&git).is_consistent());
    }

    #[test]
    fn head_states_are_explicit() {
        let mut normal = git_state();
        assert!(matches!(
            Consistency::fingerprint(&normal).head,
            HeadState::Normal { .. }
        ));
        normal.detached = true;
        normal.branch = None;
        assert!(matches!(
            Consistency::fingerprint(&normal).head,
            HeadState::Detached { .. }
        ));
        normal.head_oid = None;
        normal.branch = Some("main".to_string());
        normal.detached = false;
        assert!(matches!(
            Consistency::fingerprint(&normal).head,
            HeadState::Unborn { .. }
        ));
    }

    #[test]
    fn git_operations_are_explicit() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".git")).unwrap();
        for (marker, expected) in [
            ("MERGE_HEAD", GitOperationState::Merge),
            ("CHERRY_PICK_HEAD", GitOperationState::CherryPick),
            ("REVERT_HEAD", GitOperationState::Revert),
            ("BISECT_LOG", GitOperationState::Bisect),
        ] {
            let git_dir = temp.path().join(".git");
            let marker_path = git_dir.join(marker);
            std::fs::write(&marker_path, "").unwrap();
            assert_eq!(
                Consistency::fingerprint(&git_state_in(temp.path())).git_operation,
                expected
            );
            std::fs::remove_file(marker_path).unwrap();
        }
        std::fs::create_dir(temp.path().join(".git/rebase-merge")).unwrap();
        assert_eq!(
            Consistency::fingerprint(&git_state_in(temp.path())).git_operation,
            GitOperationState::Rebase
        );
    }

    #[test]
    fn a_moved_head_is_stale_with_a_reason() {
        let stored = Consistency::fingerprint(&git_state());
        let mut live = git_state();
        live.head_oid = Some("ffffffff".to_string());
        match stored.verify(&live) {
            Verdict::Stale {
                packet_token,
                current_token,
            } => {
                assert_ne!(packet_token, current_token);
            }
            Verdict::Consistent => panic!("expected stale"),
            other => panic!("unexpected verdict: {other:?}"),
        }
    }

    #[test]
    fn a_changed_working_tree_is_stale() {
        let stored = Consistency::fingerprint(&git_state());
        let mut live = git_state();
        live.dirty = true;
        live.unstaged_files.push(ChangedFile {
            path: PathBuf::from("src/lib.rs"),
            status: ChangeStatus::Modified,
            index_entry: None,
        });
        match stored.verify(&live) {
            Verdict::Stale {
                packet_token,
                current_token,
            } => {
                assert_ne!(packet_token, current_token);
            }
            Verdict::Consistent => panic!("expected stale"),
            other => panic!("unexpected verdict: {other:?}"),
        }
    }

    #[test]
    fn toimija_generated_output_does_not_make_a_packet_stale() {
        // A refresh writes .toimija/current.*; run sessions write under
        // .toimija/sessions/; harnesses write under .toimija/runs/. Those
        // generated artifacts must not invalidate the packet that authorized
        // them.
        let stored = Consistency::fingerprint(&git_state());
        let mut live = git_state();
        live.untracked_files
            .push(PathBuf::from(".toimija/current.json"));
        live.untracked_files
            .push(PathBuf::from(".toimija/current.md"));
        live.untracked_files
            .push(PathBuf::from(".toimija/sessions/1-1/session.json"));
        live.untracked_files
            .push(PathBuf::from(".toimija/runs/kisko.jsonl"));
        live.untracked_files
            .push(PathBuf::from(".toimija/anything/tool-output.json"));
        assert!(stored.verify(&live).is_consistent());
    }

    #[test]
    fn a_repo_yaml_change_is_still_a_real_change() {
        // repo.yaml is a tracked manifest, not generated output — a change to it
        // must invalidate the packet.
        let stored = Consistency::fingerprint(&git_state());
        let mut live = git_state();
        live.unstaged_files.push(ChangedFile {
            path: PathBuf::from(".toimija/repo.yaml"),
            status: ChangeStatus::Modified,
            index_entry: None,
        });
        live.untracked_files
            .push(PathBuf::from(".toimija/runs/kisko.jsonl"));
        assert!(!stored.verify(&live).is_consistent());
    }

    #[test]
    fn index_fingerprint_is_order_independent() {
        let mut a = git_state();
        a.staged_files = vec![
            ChangedFile {
                path: PathBuf::from("a.rs"),
                status: ChangeStatus::Added,
                index_entry: None,
            },
            ChangedFile {
                path: PathBuf::from("b.rs"),
                status: ChangeStatus::Modified,
                index_entry: None,
            },
        ];
        let mut b = git_state();
        b.staged_files = vec![
            ChangedFile {
                path: PathBuf::from("b.rs"),
                status: ChangeStatus::Modified,
                index_entry: None,
            },
            ChangedFile {
                path: PathBuf::from("a.rs"),
                status: ChangeStatus::Added,
                index_entry: None,
            },
        ];
        assert_eq!(
            Consistency::fingerprint(&a).index_fingerprint,
            Consistency::fingerprint(&b).index_fingerprint
        );
    }

    #[test]
    fn metadata_is_content_based_not_mtime_or_size() {
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let rel = Path::new("f.txt");
        let abs = dir.path().join(rel);

        std::fs::write(&abs, b"hello").unwrap();
        let original = metadata(dir.path(), rel);

        // A bare touch (mtime moves, bytes identical) must NOT change the
        // fingerprint — the old mtime basis would have.
        let handle = std::fs::OpenOptions::new().write(true).open(&abs).unwrap();
        handle
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            original,
            metadata(dir.path(), rel),
            "a touch with identical content must not change the fingerprint"
        );

        // A same-length edit ("hello" -> "world", both 5 bytes) MUST change it —
        // a size+mtime basis could miss this.
        std::fs::write(&abs, b"world").unwrap();
        assert_ne!(
            original,
            metadata(dir.path(), rel),
            "a same-size content change must change the fingerprint"
        );
    }
}
