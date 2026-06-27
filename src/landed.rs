//! "Did this branch's work land?" answered as a *content* question, not an
//! ancestry one. Squash merges preserve the tree but sever the commit DAG, so
//! `git merge-base --is-ancestor` (and patch-id checks) report false negatives
//! after a squash.
//!
//! For each path the branch touched (`git diff --name-only <merge_base> HEAD`),
//! we ask whether the branch's *own change* to it is already present in `base`:
//!
//! 1. the change applies cleanly forward onto base → still absent → not reflected;
//! 2. it reverse-applies cleanly → exactly present → reflected;
//! 3. base diverged on the file (neither applies) → fall back to line
//!    containment: reflected iff every line the branch added is present in base
//!    and every line it removed is absent. This recognizes a change that was
//!    squash-landed and then edited further — the case a plain tree or merge
//!    comparison misreports as "not landed".
//!
//! All checks run against a scratch index seeded from `base`, so the real index
//! and working tree are never touched.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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
    /// Touched paths whose change is not yet present in `base` (the unlanded work).
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

    // The branch's own surface: paths it changed relative to its fork point.
    let touched = name_only(runner, repo_root, &merge_base, "HEAD");

    // Which of those paths carry a change that is NOT yet present in `base`? See
    // the module docs for the per-path forward/reverse/containment decision —
    // robust to `base` editing the same file after the branch's change landed,
    // which a plain tree or merge comparison misreports as "not landed".
    let Some(unreflected) = unreflected_paths(runner, repo_root, base, &merge_base, &touched)
    else {
        return LandedReport {
            base: base.to_string(),
            branch,
            head,
            status: LandedStatus::Unknown,
            reason: format!(
                "could not stage `{base}` to test whether the branch's change is present"
            ),
            touched: touched.len(),
            reflected: 0,
            differing: Vec::new(),
        };
    };
    let differing: Vec<String> = touched
        .iter()
        .filter(|path| unreflected.contains(*path))
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

/// `git diff --name-only <from> <to>` as a sorted, de-duplicated set. Empty on
/// any git failure; the caller has already established a merge-base, so an empty
/// set here honestly means the branch changed nothing.
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

/// Of the branch's `touched` paths, which are NOT yet reflected in `base`?
///
/// For each path we ask: is the branch's own change to it (its `merge_base ->
/// HEAD` diff) already present in `base`? This is decided against a scratch index
/// seeded from `base` (via `GIT_INDEX_FILE`), so the real index and working tree
/// are never touched. `None` if that scratch index could not be staged (surfaced
/// as `Unknown`).
fn unreflected_paths(
    runner: &impl CommandRunner,
    repo_root: &Path,
    base: &str,
    merge_base: &str,
    touched: &BTreeSet<String>,
) -> Option<BTreeSet<String>> {
    if touched.is_empty() {
        return Some(BTreeSet::new());
    }

    let index_path =
        std::env::temp_dir().join(format!("toimija-landed-{}.index", std::process::id()));
    let staged = Command::new("git")
        .args(["read-tree", base])
        .current_dir(repo_root)
        .env("GIT_INDEX_FILE", &index_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !staged {
        let _ = std::fs::remove_file(&index_path);
        return None;
    }

    let mut unreflected = BTreeSet::new();
    for path in touched {
        if !path_reflected(runner, repo_root, &index_path, base, merge_base, path) {
            unreflected.insert(path.clone());
        }
    }

    let _ = std::fs::remove_file(&index_path);
    Some(unreflected)
}

/// Is the branch's change to `path` already present in `base`?
///
/// Two sound, git-decided cases first, then a content tiebreaker:
/// 1. the change applies cleanly forward onto base → base is still at the
///    pre-change state → NOT reflected;
/// 2. the change applies cleanly in reverse → it is exactly present → reflected;
/// 3. otherwise base has diverged on this file (neither applies), so fall back to
///    line containment: reflected iff every line the branch *added* is present in
///    base and every line it *removed* is absent. This is what recognizes a
///    squash-landing that base later edited further — the case a tree or merge
///    comparison reports as a false "not landed".
fn path_reflected(
    runner: &impl CommandRunner,
    repo_root: &Path,
    index_path: &Path,
    base: &str,
    merge_base: &str,
    path: &str,
) -> bool {
    // `--binary` so binary changes round-trip; apply needs the full patch.
    let patch = run_git_ok(
        runner,
        repo_root,
        &["diff", "--binary", merge_base, "HEAD", "--", path],
    )
    .unwrap_or_default();
    if patch.trim().is_empty() {
        return true; // the branch changed nothing here
    }
    if applies(repo_root, index_path, &patch, Direction::Forward) {
        return false; // cleanly addable → absent from base
    }
    if applies(repo_root, index_path, &patch, Direction::Reverse) {
        return true; // cleanly removable → present in base
    }
    line_contained(runner, repo_root, base, merge_base, path)
}

#[derive(Clone, Copy)]
enum Direction {
    Forward,
    Reverse,
}

/// Does `patch` apply cleanly (in the given direction) to the content staged in
/// `index_path`? `--check` only tests; it writes nothing.
fn applies(repo_root: &Path, index_path: &Path, patch: &str, direction: Direction) -> bool {
    let mut args = vec!["apply", "--cached", "--check"];
    if matches!(direction, Direction::Reverse) {
        args.push("--reverse");
    }
    let mut child = match Command::new("git")
        .args(&args)
        .current_dir(repo_root)
        .env("GIT_INDEX_FILE", index_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        // The exit status, not this write, is the source of truth.
        let _ = stdin.write_all(patch.as_bytes());
    }
    child.wait().map(|status| status.success()).unwrap_or(false)
}

/// Content tiebreaker for a path where base has diverged: is the branch's change
/// substantively present? Reflected iff every line the branch added (relative to
/// `merge_base`) appears in `base` and every line it removed does not. Compares
/// line multisets, so it is position-independent (robust to the added line no
/// longer being at end-of-file). Heuristic, but only reached once both clean
/// apply checks have failed, which bounds its blast radius.
fn line_contained(
    runner: &impl CommandRunner,
    repo_root: &Path,
    base: &str,
    merge_base: &str,
    path: &str,
) -> bool {
    let head = blob_lines(runner, repo_root, "HEAD", path);
    let ancestor = blob_lines(runner, repo_root, merge_base, path);
    let base_lines: BTreeSet<String> = blob_lines(runner, repo_root, base, path)
        .into_iter()
        .collect();

    let added = multiset_difference(&head, &ancestor);
    let removed = multiset_difference(&ancestor, &head);
    added.iter().all(|line| base_lines.contains(line))
        && removed.iter().all(|line| !base_lines.contains(line))
}

fn blob_lines(runner: &impl CommandRunner, repo_root: &Path, rev: &str, path: &str) -> Vec<String> {
    run_git_ok(runner, repo_root, &["show", &format!("{rev}:{path}")])
        .map(|text| text.lines().map(ToString::to_string).collect())
        .unwrap_or_default()
}

/// Lines present in `a` beyond their count in `b` — i.e. what `a` adds over `b`,
/// honoring duplicates so three added blank lines are not masked by one in `b`.
fn multiset_difference(a: &[String], b: &[String]) -> Vec<String> {
    let mut remaining: std::collections::HashMap<&String, usize> = std::collections::HashMap::new();
    for line in b {
        *remaining.entry(line).or_default() += 1;
    }
    let mut surplus = Vec::new();
    for line in a {
        match remaining.get_mut(line) {
            Some(count) if *count > 0 => *count -= 1,
            _ => surplus.push(line.clone()),
        }
    }
    surplus
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
            format!("none of the {n} touched path(s) are reflected in `{base}` yet")
        }
        LandedStatus::Partial => {
            format!("{reflected}/{n} touched path(s) reflected in `{base}`; the rest are not yet")
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

    #[test]
    fn multiset_difference_honors_duplicate_counts() {
        let a = lines(&["x", "x", "y", "z"]);
        let b = lines(&["x", "y"]);
        // One x and one y are covered by b; the surplus x and z remain.
        assert_eq!(multiset_difference(&a, &b), lines(&["x", "z"]));
        assert!(multiset_difference(&b, &b).is_empty());
    }

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }
}
