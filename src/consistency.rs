//! Packet consistency — a versioned freshness token over the Git state a packet
//! was compiled from.
//!
//! The token is a freshness check, not an identity: it fingerprints what
//! `git status` lists, and some changes to a State's tree never reach that list
//! (issue #239). Identity belongs to the repository point, whose id is derived
//! from the tree itself (`crate::semantic`).

use std::fs;
use std::path::{Component, Path, PathBuf};

use faija_terms::TreeIdentityAlgorithm;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::git::{ChangeStatus, ChangedFile, GitState};

/// The token Toimija computes now. v3 leaves out exactly what
/// tree-identity.v1 leaves out ([`LEAVES_OUT_AS`]); v2 watched `.sykli/`, so
/// a gate writing receipts there moved the token while the tree held still.
/// The token's own hash covers this name (see `token`), so tokens of two
/// versions never coincide.
pub const CONSISTENCY_ALGORITHM: &str = "toimija-consistency-v3";

/// The tree algorithm whose generated namespaces the token leaves out. Pinned
/// to v1, never `TreeIdentityAlgorithm::CURRENT`: what a v3 token observes is
/// fixed with v3, and a later tree algorithm is a later token version.
const LEAVES_OUT_AS: TreeIdentityAlgorithm = TreeIdentityAlgorithm::V1;

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
    #[serde(with = "crate::path_codec::vec")]
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
pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
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
    crate::path_codec::fingerprint(path)
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

/// The namespaces whose content the token leaves out, apart from the
/// configuration forced in: tree-identity.v1's frozen list.
pub(crate) fn generated_namespaces() -> &'static [&'static str] {
    LEAVES_OUT_AS.namespaces()
}

/// Whether a repo-relative path is generated tool state the token leaves out:
/// a path under one of tree-identity.v1's frozen namespaces (`.toimija`,
/// `.teko`, `.sykli`), unless it is configuration v1 forces in
/// (`.toimija/repo.yaml`). Exactly what the tree leaves out, read from the
/// vendored faija-terms rather than listed again here ([`LEAVES_OUT_AS`]), so
/// a refresh, a session, a snapshot or a gate's receipt never moves the token
/// while the tree holds still.
pub(crate) fn is_generated_output(path: &Path) -> bool {
    let Some(Component::Normal(first)) = path.components().next() else {
        return false;
    };
    LEAVES_OUT_AS
        .namespaces()
        .iter()
        .any(|namespace| first == *namespace)
        && !LEAVES_OUT_AS
            .config_paths()
            .iter()
            .any(|config| path == Path::new(config))
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
            ahead: None,
            behind: None,
            ahead_behind_ref: None,
            base: None,
            merge_base: Some("0000000".to_string()),
            detached: false,
            dirty: false,
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            conflicted_files: Vec::new(),
            conflict_entries: Vec::new(),
            branch_files: Vec::new(),
            branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
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
        assert!(matches!(stored.verify(&git), Verdict::Consistent));
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
            original_path: None,
            similarity: None,
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
        live.untracked_files.push(PathBuf::from(".teko/teko.redb"));
        assert!(matches!(stored.verify(&live), Verdict::Consistent));
    }

    /// The token leaves out exactly what faija's tree-identity.v1 leaves out:
    /// its frozen namespaces, minus the configuration it forces in. Read from
    /// the vendored faija-terms, so there is no second list to drift; pinned
    /// to v1, not `CURRENT`, so a later tree algorithm cannot change what a
    /// `toimija-consistency-v3` token means.
    #[test]
    fn the_token_leaves_out_exactly_what_tree_identity_v1_leaves_out() {
        let v1 = faija_terms::TreeIdentityAlgorithm::V1;
        assert_eq!(v1.namespaces(), [".toimija", ".teko", ".sykli"]);
        for namespace in v1.namespaces() {
            for path in ["", "receipt.json", "runs/r1/journal"] {
                let path = Path::new(namespace).join(path);
                assert!(is_generated_output(&path), "{}", path.display());
            }
        }
        for config in v1.config_paths() {
            assert!(!is_generated_output(Path::new(config)), "{config}");
        }
        // Work content, including look-alikes the tree keeps too.
        for work in [
            "src/main.rs",
            "x/.sykli/receipt.json",
            ".sykli-notes/a",
            "sykli/a",
            ".SYKLI/a",
            ".toimija-x/a",
        ] {
            assert!(!is_generated_output(Path::new(work)), "{work}");
        }
    }

    /// A gate writes its receipt into `.sykli/`, which the tree leaves out:
    /// whether git can see the receipt or not, the packet stays fresh.
    #[test]
    fn sykli_output_does_not_make_a_packet_stale() {
        let stored = Consistency::fingerprint(&git_state());
        let mut live = git_state();
        live.untracked_files
            .push(PathBuf::from(".sykli/receipt.json"));
        live.untracked_files
            .push(PathBuf::from(".sykli/runs/r1/journal"));
        live.unstaged_files.push(ChangedFile {
            path: PathBuf::from(".sykli/tracked.json"),
            status: ChangeStatus::Modified,
            index_entry: None,
            original_path: None,
            similarity: None,
        });
        assert!(matches!(stored.verify(&live), Verdict::Consistent));
        assert_eq!(
            crate::context::refresh_token(&live),
            crate::context::refresh_token(&git_state())
        );
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
            original_path: None,
            similarity: None,
        });
        live.untracked_files
            .push(PathBuf::from(".toimija/runs/kisko.jsonl"));
        assert!(!matches!(stored.verify(&live), Verdict::Consistent));
    }

    #[test]
    fn index_fingerprint_is_order_independent() {
        let mut a = git_state();
        a.staged_files = vec![
            ChangedFile {
                path: PathBuf::from("a.rs"),
                status: ChangeStatus::Added,
                index_entry: None,
                original_path: None,
                similarity: None,
            },
            ChangedFile {
                path: PathBuf::from("b.rs"),
                status: ChangeStatus::Modified,
                index_entry: None,
                original_path: None,
                similarity: None,
            },
        ];
        let mut b = git_state();
        b.staged_files = vec![
            ChangedFile {
                path: PathBuf::from("b.rs"),
                status: ChangeStatus::Modified,
                index_entry: None,
                original_path: None,
                similarity: None,
            },
            ChangedFile {
                path: PathBuf::from("a.rs"),
                status: ChangeStatus::Added,
                index_entry: None,
                original_path: None,
                similarity: None,
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
