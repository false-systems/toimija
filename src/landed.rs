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
//! 3. base diverged on the file (neither applies) → **ambiguous**. We do not
//!    decide landing from a textual heuristic. We do record a line-containment
//!    signal — are the lines the branch added present in base, and the lines it
//!    removed absent? — but only as *evidence* on the ambiguous report. A false
//!    "landed" is the dangerous direction for branch cleanup, so the default
//!    never claims landed without a clean forward/reverse apply proof.
//!
//! All checks run against a scratch index seeded from `base`, so the real index
//! and working tree are never touched.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::command::CommandRunner;
use crate::error::ToimijaError;
use crate::git::run_git_ok;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandedStatus {
    /// Every touched path's change is proven present in `base`, or HEAD is
    /// `base` or behind it: every commit on HEAD is reachable from `base`.
    Landed,
    /// Every touched path's change is proven still absent.
    NotLanded,
    /// Touched paths split cleanly into reflected and not — none ambiguous.
    Partial,
    /// At least one touched path could not be proven either way (base diverged on
    /// it). The conservative default: never claim landed without a clean proof.
    Ambiguous,
    /// Cannot tell: no merge-base with the base, or HEAD has commits of its
    /// own that change nothing (a commit and its revert).
    Unknown,
}

/// A touched path whose change could be neither cleanly applied nor reverse-
/// applied to `base` (base diverged on it). The heuristic is evidence only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmbiguousPath {
    pub path: String,
    /// Line-containment heuristic: every line the branch added appears in base and
    /// every line it removed is absent. Reported as evidence; it never decides
    /// landing. Gate a future `--heuristic`/`--allow-line-containment` on this.
    pub line_containment: bool,
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
    /// How many of those touched paths are proven present in `base`.
    pub reflected: usize,
    /// Touched paths proven not yet present in `base` (the unlanded work).
    pub differing: Vec<String>,
    /// Touched paths where base diverged and landing could not be proven.
    pub ambiguous: Vec<AmbiguousPath>,
}

/// The repository root at `cwd` and HEAD's short oid, read together with one
/// `git rev-parse`. Where HEAD names no commit the call fails as a whole; the
/// root is then read alone, as [`crate::git::discover_repo_root`] reads it, and
/// there is no HEAD to name. An answer that cannot be split in two (a root
/// with a newline in it) is read the same way, with HEAD read on its own.
pub fn locate(
    runner: &impl CommandRunner,
    cwd: &Path,
) -> Result<(PathBuf, Option<String>), ToimijaError> {
    let output = runner
        .run(
            cwd,
            "git",
            &["rev-parse", "--show-toplevel", "--short", "HEAD"],
        )
        .map_err(|_| ToimijaError::CommandNotFound("git".to_string()))?;
    if output.status != 0 {
        return Ok((crate::git::discover_repo_root(runner, cwd)?, None));
    }
    let split = output
        .stdout
        .strip_suffix(b"\n")
        .and_then(|body| {
            let (root, head) = body.split_at(body.iter().position(|byte| *byte == b'\n')?);
            Some((root, &head[1..]))
        })
        .filter(|(_, head)| !head.contains(&b'\n'))
        .map(|(root, head)| {
            (
                crate::path_codec::from_git_bytes(root),
                String::from_utf8_lossy(head).trim().to_string(),
            )
        })
        .filter(|(root, _)| root.is_absolute());
    match split {
        Some((root, head)) => Ok((root, Some(head).filter(|oid| !oid.is_empty()))),
        None => {
            let root = crate::git::discover_repo_root(runner, cwd)?;
            let head = short_head(runner, &root);
            Ok((root, head))
        }
    }
}

fn short_head(runner: &impl CommandRunner, repo_root: &Path) -> Option<String> {
    run_git_ok(runner, repo_root, &["rev-parse", "--short", "HEAD"])
        .map(|out| out.trim().to_string())
        .filter(|oid| !oid.is_empty())
}

/// Assess whether the current `HEAD`'s branch surface is reflected in `base`.
/// `head` is HEAD's short oid as [`locate`] read it. Infallible: any git hiccup
/// (missing base, no merge-base) degrades to `Unknown` with an actionable
/// reason rather than erroring, mirroring `doctor`.
pub fn assess_landing(
    runner: &impl CommandRunner,
    repo_root: &Path,
    head: Option<String>,
    base: &str,
) -> LandedReport {
    let branch = run_git_ok(runner, repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|out| out.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());

    let merge_base = run_git_ok(runner, repo_root, &["merge-base", base, "HEAD"])
        .map(|out| out.trim().to_string())
        .filter(|oid| !oid.is_empty());

    let Some(merge_base) = merge_base else {
        info!(event = "toimija.landed.no_merge_base", base);
        return unknown_report(
            base,
            branch,
            head,
            0,
            format!(
                "no merge-base with `{base}` — unrelated histories, or `{base}` is not fetched (try `git fetch`)"
            ),
        );
    };

    // HEAD at its base, or behind it: every commit on HEAD is reachable from
    // the base, so nothing it holds can be missing there. Ancestry is proof
    // in this direction. HEAD's short oid is the unique prefix git printed for
    // it, so the merge-base starting with it is HEAD itself, with no git
    // process to ask.
    if head
        .as_deref()
        .is_some_and(|short| merge_base.starts_with(short))
    {
        info!(event = "toimija.landed.head_in_base", base);
        return LandedReport {
            base: base.to_string(),
            branch,
            head,
            status: LandedStatus::Landed,
            reason: format!("HEAD is in `{base}`: every commit on it is reachable from `{base}`"),
            touched: 0,
            reflected: 0,
            differing: Vec::new(),
            ambiguous: Vec::new(),
        };
    }

    // The branch's own surface: paths it changed relative to its fork point.
    let touched = name_only(runner, repo_root, &merge_base, "HEAD");

    // Per-path: is the branch's change present, absent, or unprovable? See the
    // module docs for the forward/reverse/ambiguous decision.
    let Some(assessment) = assess_paths(runner, repo_root, base, &merge_base, &touched) else {
        return unknown_report(
            base,
            branch,
            head,
            touched.len(),
            format!("could not stage `{base}` to test whether the branch's change is present"),
        );
    };

    let touched_count = touched.len();
    let status = classify(
        assessment.reflected,
        assessment.unreflected.len(),
        assessment.ambiguous.len(),
    );
    let reason = reason_for(
        status,
        touched_count,
        assessment.reflected,
        assessment.ambiguous.len(),
        base,
    );

    info!(
        event = "toimija.landed.assessed",
        base,
        status = ?status,
        touched = touched_count,
        reflected = assessment.reflected,
        ambiguous = assessment.ambiguous.len()
    );

    LandedReport {
        base: base.to_string(),
        branch,
        head,
        status,
        reason,
        touched: touched_count,
        reflected: assessment.reflected,
        differing: assessment.unreflected,
        ambiguous: assessment.ambiguous,
    }
}

fn unknown_report(
    base: &str,
    branch: String,
    head: Option<String>,
    touched: usize,
    reason: String,
) -> LandedReport {
    LandedReport {
        base: base.to_string(),
        branch,
        head,
        status: LandedStatus::Unknown,
        reason,
        touched,
        reflected: 0,
        differing: Vec::new(),
        ambiguous: Vec::new(),
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

struct PathAssessment {
    reflected: usize,
    unreflected: Vec<String>,
    ambiguous: Vec<AmbiguousPath>,
}

/// Bucket the branch's `touched` paths into reflected / unreflected / ambiguous,
/// each verdict decided against a scratch index seeded from `base` (via
/// `GIT_INDEX_FILE`), so the real index and working tree are never touched.
/// `None` if that scratch index could not be staged (surfaced as `Unknown`).
fn assess_paths(
    runner: &impl CommandRunner,
    repo_root: &Path,
    base: &str,
    merge_base: &str,
    touched: &BTreeSet<String>,
) -> Option<PathAssessment> {
    let mut assessment = PathAssessment {
        reflected: 0,
        unreflected: Vec::new(),
        ambiguous: Vec::new(),
    };
    if touched.is_empty() {
        return Some(assessment);
    }

    let index_path =
        std::env::temp_dir().join(format!("toimija-landed-{}.index", std::process::id()));
    let staged = crate::command::git_command(repo_root)
        .args(["read-tree", base])
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

    for path in touched {
        match path_verdict(runner, repo_root, &index_path, base, merge_base, path) {
            PathVerdict::Reflected => assessment.reflected += 1,
            PathVerdict::Unreflected => assessment.unreflected.push(path.clone()),
            PathVerdict::Ambiguous { line_containment } => {
                assessment.ambiguous.push(AmbiguousPath {
                    path: path.clone(),
                    line_containment,
                });
            }
        }
    }

    let _ = std::fs::remove_file(&index_path);
    Some(assessment)
}

enum PathVerdict {
    Reflected,
    Unreflected,
    Ambiguous { line_containment: bool },
}

/// Is the branch's change to `path` present in `base`? Two sound, git-decided
/// cases, then ambiguity:
/// 1. applies cleanly forward → base is still at the pre-change state → absent;
/// 2. reverse-applies cleanly → exactly present → reflected;
/// 3. otherwise base diverged here → ambiguous, carrying the line-containment
///    heuristic as evidence (never as a landed decision).
fn path_verdict(
    runner: &impl CommandRunner,
    repo_root: &Path,
    index_path: &Path,
    base: &str,
    merge_base: &str,
    path: &str,
) -> PathVerdict {
    // `--binary` so binary changes round-trip; apply needs the full patch.
    let patch = run_git_ok(
        runner,
        repo_root,
        &["diff", "--binary", merge_base, "HEAD", "--", path],
    )
    .unwrap_or_default();
    if patch.trim().is_empty() {
        return PathVerdict::Reflected; // the branch changed nothing here
    }
    if applies(repo_root, index_path, &patch, Direction::Forward) {
        return PathVerdict::Unreflected; // cleanly addable → absent from base
    }
    if applies(repo_root, index_path, &patch, Direction::Reverse) {
        return PathVerdict::Reflected; // cleanly removable → present in base
    }
    PathVerdict::Ambiguous {
        line_containment: line_contained(runner, repo_root, base, merge_base, path),
    }
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
    let mut child = match crate::command::git_command(repo_root)
        .args(&args)
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

/// Line-containment evidence for a diverged path: does base appear to contain the
/// branch's change? True iff every line the branch added (relative to
/// `merge_base`) appears in `base` and every line it removed does not. Position-
/// independent (line multisets), so robust to the added line no longer being at
/// end-of-file. A heuristic — reported as evidence on an ambiguous verdict, never
/// used to decide landing.
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

/// Pure classification — kept free of git so it is directly testable. Ambiguity
/// dominates: any unprovable path means we cannot certify `Landed`.
fn classify(reflected: usize, unreflected: usize, ambiguous: usize) -> LandedStatus {
    match (reflected, unreflected, ambiguous) {
        (0, 0, 0) => LandedStatus::Unknown,
        (_, _, a) if a > 0 => LandedStatus::Ambiguous,
        (_, 0, 0) => LandedStatus::Landed,
        (0, _, 0) => LandedStatus::NotLanded,
        _ => LandedStatus::Partial,
    }
}

fn reason_for(
    status: LandedStatus,
    touched: usize,
    reflected: usize,
    ambiguous: usize,
    base: &str,
) -> String {
    match status {
        LandedStatus::Landed => {
            format!("all {touched} touched path(s) are reflected in `{base}`")
        }
        LandedStatus::NotLanded => {
            format!("none of the {touched} touched path(s) are reflected in `{base}` yet")
        }
        LandedStatus::Partial => {
            format!(
                "{reflected}/{touched} touched path(s) reflected in `{base}`; the rest are not yet"
            )
        }
        LandedStatus::Ambiguous => {
            format!(
                "git could not prove {ambiguous} of {touched} touched path(s) are reflected in `{base}` — base diverged there; see the ambiguous list"
            )
        }
        LandedStatus::Unknown => "branch introduced no changes versus its merge-base".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_covers_each_outcome() {
        assert_eq!(classify(0, 0, 0), LandedStatus::Unknown);
        assert_eq!(classify(5, 0, 0), LandedStatus::Landed);
        assert_eq!(classify(0, 5, 0), LandedStatus::NotLanded);
        assert_eq!(classify(3, 2, 0), LandedStatus::Partial);
        // Any ambiguous path prevents a confident verdict.
        assert_eq!(classify(4, 0, 1), LandedStatus::Ambiguous);
        assert_eq!(classify(0, 0, 3), LandedStatus::Ambiguous);
        assert_eq!(classify(2, 2, 1), LandedStatus::Ambiguous);
    }

    #[test]
    fn reason_names_the_base_and_counts() {
        let reason = reason_for(LandedStatus::Partial, 3, 1, 0, "origin/main");
        assert!(reason.contains("1/3"));
        assert!(reason.contains("origin/main"));
    }

    #[test]
    fn ambiguous_reason_reports_the_unprovable_count() {
        let reason = reason_for(LandedStatus::Ambiguous, 4, 1, 2, "origin/main");
        assert!(reason.contains("2 of 4"));
        assert!(reason.contains("diverged"));
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
