//! Packet consistency — a verifiable fingerprint of the Git state a packet was
//! compiled from, so a reader can confirm the packet still matches the repo
//! *before* trusting it, without re-deriving repo state itself.
//!
//! This is the primitive the provider-neutral False Systems harness depends on:
//! it must not read repo state until the Toimija packet can be verified. The
//! fingerprint is the thing it verifies.
//!
//! State-based, not time-based. Every field derives purely from `GitState`
//! (already in the packet), so the block stays deterministic — a stale packet is
//! caught by a changed digest, not by a clock. Determinism also keeps the
//! `current.json` snapshot tests stable.

use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::git::GitState;

/// Schema version of the consistency block. Bump when the fingerprint inputs
/// change, so a reader can refuse a packet it would verify differently.
pub const CONSISTENCY_SCHEMA: u32 = 1;

/// A verifiable fingerprint of the repo state a packet was compiled from.
/// Embedded in `.toimija/current.json`. Deterministic: derived entirely from
/// `GitState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consistency {
    pub schema: u32,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub merge_base: Option<String>,
    /// Hex digest over the working-tree + index state (staged / unstaged /
    /// untracked / conflicted / dirty). Changes whenever the tree the packet
    /// describes changes.
    pub worktree_digest: String,
}

impl Consistency {
    /// Fingerprint the state a packet was built from.
    pub fn fingerprint(git: &GitState) -> Consistency {
        Consistency {
            schema: CONSISTENCY_SCHEMA,
            head: git.head_oid.clone(),
            branch: git.branch.clone(),
            merge_base: git.merge_base.clone(),
            worktree_digest: worktree_digest(git),
        }
    }

    /// Verify this stored fingerprint against the live repo state, reporting the
    /// specific reasons a packet is stale — so a caller can say *why* a refresh is
    /// needed, not merely that one is.
    pub fn verify(&self, live: &GitState) -> Verdict {
        let fresh = Consistency::fingerprint(live);
        let mut reasons = Vec::new();
        if self.schema != fresh.schema {
            reasons.push(format!(
                "consistency schema changed ({} -> {})",
                self.schema, fresh.schema
            ));
        }
        if self.head != fresh.head {
            reasons.push(format!(
                "HEAD moved ({} -> {})",
                short(&self.head),
                short(&fresh.head)
            ));
        }
        if self.branch != fresh.branch {
            reasons.push(format!(
                "branch changed ({} -> {})",
                self.branch.as_deref().unwrap_or("none"),
                fresh.branch.as_deref().unwrap_or("none")
            ));
        }
        if self.merge_base != fresh.merge_base {
            reasons.push("merge-base moved".to_string());
        }
        if self.worktree_digest != fresh.worktree_digest {
            reasons.push("working tree changed (staged/unstaged/untracked)".to_string());
        }
        if reasons.is_empty() {
            Verdict::Consistent
        } else {
            Verdict::Stale { reasons }
        }
    }
}

/// The outcome of verifying a packet's fingerprint against the live repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The packet matches the current repo state; safe to trust.
    Consistent,
    /// The repo moved since the packet was built; it must be refreshed first.
    Stale { reasons: Vec<String> },
}

impl Verdict {
    pub fn is_consistent(&self) -> bool {
        matches!(self, Verdict::Consistent)
    }
}

/// Stable hex digest over the working-tree + index state. Lines are sorted so
/// git's ordering cannot affect the result, and `DefaultHasher` has a fixed seed
/// so the digest is reproducible across runs and machines. This is staleness
/// detection, not adversarial defense, so a 64-bit non-cryptographic hash (and
/// zero new dependencies) is the right tool.
fn worktree_digest(git: &GitState) -> String {
    let mut lines: Vec<String> = Vec::new();
    for file in &git.staged_files {
        lines.push(format!(
            "staged\t{:?}\t{}",
            file.status,
            file.path.display()
        ));
    }
    for file in &git.unstaged_files {
        lines.push(format!(
            "unstaged\t{:?}\t{}",
            file.status,
            file.path.display()
        ));
    }
    for path in &git.untracked_files {
        lines.push(format!("untracked\t{}", path.display()));
    }
    for path in &git.conflicted_files {
        lines.push(format!("conflicted\t{}", path.display()));
    }
    lines.push(format!("dirty\t{}", git.dirty));
    lines.sort();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in &lines {
        line.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// First 7 chars of an oid for messages; "none" when absent.
fn short(oid: &Option<String>) -> String {
    match oid {
        Some(oid) => oid.chars().take(7).collect(),
        None => "none".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{ChangeStatus, ChangedFile};
    use std::path::PathBuf;

    fn git_state() -> GitState {
        GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: None,
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
    fn a_moved_head_is_stale_with_a_reason() {
        let stored = Consistency::fingerprint(&git_state());
        let mut live = git_state();
        live.head_oid = Some("ffffffff".to_string());
        match stored.verify(&live) {
            Verdict::Stale { reasons } => {
                assert!(
                    reasons.iter().any(|r| r.contains("HEAD moved")),
                    "{reasons:?}"
                );
            }
            Verdict::Consistent => panic!("expected stale"),
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
        });
        match stored.verify(&live) {
            Verdict::Stale { reasons } => {
                assert!(
                    reasons.iter().any(|r| r.contains("working tree")),
                    "{reasons:?}"
                );
            }
            Verdict::Consistent => panic!("expected stale"),
        }
    }

    #[test]
    fn worktree_digest_is_order_independent() {
        let mut a = git_state();
        a.staged_files = vec![
            ChangedFile {
                path: PathBuf::from("a.rs"),
                status: ChangeStatus::Added,
            },
            ChangedFile {
                path: PathBuf::from("b.rs"),
                status: ChangeStatus::Modified,
            },
        ];
        let mut b = git_state();
        b.staged_files = vec![
            ChangedFile {
                path: PathBuf::from("b.rs"),
                status: ChangeStatus::Modified,
            },
            ChangedFile {
                path: PathBuf::from("a.rs"),
                status: ChangeStatus::Added,
            },
        ];
        assert_eq!(worktree_digest(&a), worktree_digest(&b));
    }
}
