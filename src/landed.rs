//! "Did this branch's work land?" answered as a *content* question, not an
//! ancestry one. Squash merges preserve the tree but sever the commit DAG, so
//! `git merge-base --is-ancestor` (and patch-id checks) report false negatives
//! after a squash. The robust signal is whether the branch's touched surface is
//! already reflected in the base tree.
//!
//! Primitive:
//! ```text
//! base_mb = git merge-base <base> HEAD
//! touched = git diff --name-only base_mb HEAD     # the branch's own surface
//! a path is "reflected" if it does NOT differ between <base> and HEAD
//! ```

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::command::CommandRunner;
use crate::git::run_git_ok;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandedStatus {
    /// Every path the branch touched is already reflected in the base tree.
    Landed,
    /// None of the touched paths are reflected — the work is still outstanding.
    NotLanded,
    /// Some touched paths are reflected, some are not.
    Partial,
    /// Cannot tell: no merge-base with the base, or the branch changed nothing.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedReport {
    /// The ref the branch is compared against (where work lands).
    pub base: String,
    /// Human label for what was assessed (branch name, or `HEAD` when detached).
    pub branch: String,
    /// Short HEAD oid, when resolvable.
    pub head: Option<String>,
    pub status: LandedStatus,
    pub reason: String,
    /// Number of paths the branch changed relative to its merge-base with `base`.
    pub touched: usize,
    /// How many of those touched paths are already reflected in `base`.
    pub reflected: usize,
    /// Touched paths that still differ from `base` (the work not yet reflected).
    pub differing: Vec<String>,
}

/// Assess whether the current `HEAD`'s branch surface is reflected in `base`.
/// Infallible: any git hiccup (missing base, no merge-base) degrades to
/// `Unknown` with an actionable reason rather than erroring, mirroring `doctor`.
pub fn assess_landing(runner: &impl CommandRunner, repo_root: &Path, base: &str) -> LandedReport {
    let branch = run_git_ok(runner, repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|out| out.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    let head = run_git_ok(runner, repo_root, &["rev-parse", "--short", "HEAD"])
        .map(|out| out.trim().to_string())
        .filter(|oid| !oid.is_empty());

    let merge_base = run_git_ok(runner, repo_root, &["merge-base", base, "HEAD"])
        .map(|out| out.trim().to_string())
        .filter(|oid| !oid.is_empty());

    let Some(merge_base) = merge_base else {
        info!(event = "toimija.landed.no_merge_base", base);
        return LandedReport {
            base: base.to_string(),
            branch,
            head,
            status: LandedStatus::Unknown,
            reason: format!(
                "no merge-base with `{base}` — unrelated histories, or `{base}` is not fetched (try `git fetch`)"
            ),
            touched: 0,
            reflected: 0,
            differing: Vec::new(),
        };
    };

    // Paths the branch changed relative to where it forked from `base`.
    let touched = name_only(runner, repo_root, &merge_base, "HEAD");
    // Paths that still differ between `base` and HEAD. A touched path absent here
    // means base and HEAD agree on it — the branch's content is reflected.
    let differing_all = name_only(runner, repo_root, base, "HEAD");
    let differing: Vec<String> = touched
        .iter()
        .filter(|path| differing_all.contains(*path))
        .cloned()
        .collect();

    let status = classify(touched.len(), differing.len());
    let reflected = touched.len() - differing.len();
    let reason = reason_for(status, &touched, reflected, base);

    info!(
        event = "toimija.landed.assessed",
        base,
        status = ?status,
        touched = touched.len(),
        reflected
    );

    LandedReport {
        base: base.to_string(),
        branch,
        head,
        status,
        reason,
        touched: touched.len(),
        reflected,
        differing,
    }
}

/// `git diff --name-only <from> <to>` as a sorted, de-duplicated set. Empty (and
/// thus a `Landed`-leaning result) on any git failure — callers distinguish the
/// no-merge-base case before reaching here, so an empty set here means "no
/// differences", which is the honest reading of a successful empty diff.
fn name_only(
    runner: &impl CommandRunner,
    repo_root: &Path,
    from: &str,
    to: &str,
) -> BTreeSet<String> {
    run_git_ok(runner, repo_root, &["diff", "--name-only", from, to])
        .map(|out| {
            out.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Pure classification — the heart of the command, kept free of git so it is
/// directly testable. `touched` is the branch's surface size; `differing` is how
/// many of those paths are not yet reflected in the base.
fn classify(touched: usize, differing: usize) -> LandedStatus {
    match (touched, differing) {
        (0, _) => LandedStatus::Unknown,
        (_, 0) => LandedStatus::Landed,
        (t, d) if d == t => LandedStatus::NotLanded,
        _ => LandedStatus::Partial,
    }
}

fn reason_for(
    status: LandedStatus,
    touched: &BTreeSet<String>,
    reflected: usize,
    base: &str,
) -> String {
    let n = touched.len();
    match status {
        LandedStatus::Landed => {
            format!("all {n} touched path(s) are reflected in `{base}`")
        }
        LandedStatus::NotLanded => {
            format!("all {n} touched path(s) still differ from `{base}`")
        }
        LandedStatus::Partial => {
            format!("{reflected}/{n} touched path(s) reflected in `{base}`; the rest still differ")
        }
        LandedStatus::Unknown => "branch introduced no changes versus its merge-base".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_covers_every_quadrant() {
        assert_eq!(classify(0, 0), LandedStatus::Unknown);
        assert_eq!(classify(5, 0), LandedStatus::Landed);
        assert_eq!(classify(5, 5), LandedStatus::NotLanded);
        assert_eq!(classify(5, 2), LandedStatus::Partial);
    }

    #[test]
    fn reason_names_the_base_and_counts() {
        let touched: BTreeSet<String> = ["a", "b", "c"].iter().map(ToString::to_string).collect();
        let reason = reason_for(LandedStatus::Partial, &touched, 1, "origin/main");
        assert!(reason.contains("1/3"));
        assert!(reason.contains("origin/main"));
    }
}
