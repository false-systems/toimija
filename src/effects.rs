//! Effects a wrapped session had on the world outside the working tree,
//! observed after the fact.
//!
//! Toimija records what it can see at session entry — HEAD, the upstream
//! ref, the open pull requests for the branch — and compares at exit. The
//! result is a list of effect names (`commit`, `push`, `open_pr`) plus a
//! coverage flag: when an inspection could not run, absence of an effect is
//! not evidence that it did not happen, and consumers must not read it that
//! way. This is detection, not prevention; preventing an undeclared effect is
//! a sandbox's job (Rauha), not a Git observer's.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::command::CommandRunner;
use crate::git::run_git_observed;

pub const COMMIT: &str = "commit";
pub const PUSH: &str = "push";
pub const OPEN_PR: &str = "open_pr";

/// Whether every inspection behind `effects_observed` actually ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EffectsCoverage {
    #[default]
    Complete,
    Partial,
}

/// What entry recorded, to compare against at exit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryObservation {
    /// OID of the branch's upstream ref at entry; `None` when the branch had
    /// no upstream (a later upstream appearing is itself a push).
    pub upstream_oid: Option<String>,
    /// Open PR numbers for the branch at entry; `None` when `gh` could not be
    /// asked, which makes `open_pr` coverage partial.
    pub open_prs: Option<Vec<u64>>,
    /// The upstream question could not be asked at all (git failed to run).
    pub upstream_unknown: bool,
}

/// What exit sees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitObservation {
    pub head: Option<String>,
    pub upstream_oid: Option<String>,
    pub upstream_unknown: bool,
    /// Is the upstream ref reachable from HEAD? A push from this session
    /// leaves the upstream at (or behind) HEAD; a fetch of someone else's
    /// commits moves the upstream to something HEAD does not contain.
    pub upstream_reachable_from_head: Option<bool>,
    pub open_prs: Option<Vec<u64>>,
}

/// Pure derivation: which effects happened between entry and exit, and
/// whether every inspection behind that answer ran.
pub fn derive(
    entry_head: Option<&str>,
    entry: &EntryObservation,
    exit: &ExitObservation,
) -> (Vec<String>, EffectsCoverage) {
    let mut observed = Vec::new();
    let mut coverage = EffectsCoverage::Complete;

    match (entry_head, exit.head.as_deref()) {
        (Some(before), Some(after)) => {
            if before != after {
                observed.push(COMMIT.to_string());
            }
        }
        _ => coverage = EffectsCoverage::Partial,
    }

    if entry.upstream_unknown || exit.upstream_unknown {
        coverage = EffectsCoverage::Partial;
    } else {
        match (&entry.upstream_oid, &exit.upstream_oid) {
            (before, Some(after)) if before.as_deref() != Some(after.as_str()) => {
                match exit.upstream_reachable_from_head {
                    Some(true) => observed.push(PUSH.to_string()),
                    // The upstream moved to something HEAD does not contain:
                    // a fetch of someone else's push, not ours.
                    Some(false) => {}
                    None => coverage = EffectsCoverage::Partial,
                }
            }
            _ => {}
        }
    }

    match (&entry.open_prs, &exit.open_prs) {
        (Some(before), Some(after)) => {
            if after.iter().any(|number| !before.contains(number)) {
                observed.push(OPEN_PR.to_string());
            }
        }
        _ => coverage = EffectsCoverage::Partial,
    }

    (observed, coverage)
}

/// Record what entry can see. Never fails: what cannot be asked is `None`
/// and marks the answer partial later.
pub fn observe_entry(
    runner: &impl CommandRunner,
    repo_root: &Path,
    branch: Option<&str>,
) -> EntryObservation {
    let (upstream_oid, upstream_unknown) = upstream_oid(runner, repo_root);
    EntryObservation {
        upstream_oid,
        open_prs: branch.and_then(|branch| open_prs(repo_root, branch)),
        upstream_unknown,
    }
}

/// Record what exit can see, against the entry observation's shape.
pub fn observe_exit(
    runner: &impl CommandRunner,
    repo_root: &Path,
    branch: Option<&str>,
) -> ExitObservation {
    let head = run_git_observed(runner, repo_root, &["rev-parse", "HEAD"])
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string());
    let (upstream_oid, upstream_unknown) = upstream_oid(runner, repo_root);
    let upstream_reachable_from_head = match (&upstream_oid, &head) {
        (Some(upstream), Some(head)) => is_ancestor(runner, repo_root, upstream, head),
        _ => None,
    };
    ExitObservation {
        head,
        upstream_oid,
        upstream_unknown,
        upstream_reachable_from_head,
        open_prs: branch.and_then(|branch| open_prs(repo_root, branch)),
    }
}

/// `(oid, unknown)`: `Ok(None)` shape is "no upstream", which is an answer;
/// `unknown` is "git could not be asked".
fn upstream_oid(runner: &impl CommandRunner, repo_root: &Path) -> (Option<String>, bool) {
    match runner.run(
        repo_root,
        "git",
        &["rev-parse", "--verify", "-q", "@{upstream}"],
    ) {
        Ok(output) if output.status == 0 => (
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string()),
            false,
        ),
        // Non-zero here means "no upstream configured": a real answer.
        Ok(_) => (None, false),
        Err(_) => (None, true),
    }
}

fn is_ancestor(
    runner: &impl CommandRunner,
    repo_root: &Path,
    ancestor: &str,
    descendant: &str,
) -> Option<bool> {
    match runner.run(
        repo_root,
        "git",
        &["merge-base", "--is-ancestor", ancestor, descendant],
    ) {
        Ok(output) if output.status == 0 => Some(true),
        Ok(output) if output.status == 1 => Some(false),
        _ => None,
    }
}

/// Open pull request numbers for `branch`, via `gh`. `None` when `gh` is not
/// on PATH, does not answer within a short deadline, fails, or answers with
/// something that is not a list of numbers — every one of which is "could not
/// look", never "none open".
fn open_prs(repo_root: &Path, branch: &str) -> Option<Vec<u64>> {
    if which::which("gh").is_err() {
        return None;
    }
    let mut child = std::process::Command::new("gh")
        .args(["pr", "list", "--head", branch, "--json", "number"])
        .current_dir(repo_root)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(8);
    let output = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait_with_output().ok()?,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !output.status.success() {
        return None;
    }
    let entries: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    entries
        .iter()
        .map(|entry| entry.get("number").and_then(serde_json::Value::as_u64))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(upstream: Option<&str>, prs: Option<&[u64]>) -> EntryObservation {
        EntryObservation {
            upstream_oid: upstream.map(str::to_string),
            open_prs: prs.map(<[u64]>::to_vec),
            upstream_unknown: false,
        }
    }

    fn exit(
        head: &str,
        upstream: Option<&str>,
        reachable: Option<bool>,
        prs: Option<&[u64]>,
    ) -> ExitObservation {
        ExitObservation {
            head: Some(head.to_string()),
            upstream_oid: upstream.map(str::to_string),
            upstream_unknown: false,
            upstream_reachable_from_head: reachable,
            open_prs: prs.map(<[u64]>::to_vec),
        }
    }

    #[test]
    fn a_quiet_session_observes_nothing_with_complete_coverage() {
        let (observed, coverage) = derive(
            Some("aaa"),
            &entry(Some("up1"), Some(&[])),
            &exit("aaa", Some("up1"), Some(true), Some(&[])),
        );
        assert!(observed.is_empty());
        assert_eq!(coverage, EffectsCoverage::Complete);
    }

    #[test]
    fn a_moved_head_is_a_commit() {
        let (observed, coverage) = derive(
            Some("aaa"),
            &entry(Some("up1"), Some(&[])),
            &exit("bbb", Some("up1"), Some(false), Some(&[])),
        );
        assert_eq!(observed, vec![COMMIT]);
        assert_eq!(coverage, EffectsCoverage::Complete);
    }

    #[test]
    fn an_upstream_that_moved_to_something_head_contains_is_a_push() {
        let (observed, _) = derive(
            Some("aaa"),
            &entry(Some("up1"), Some(&[])),
            &exit("bbb", Some("bbb"), Some(true), Some(&[])),
        );
        assert_eq!(observed, vec![COMMIT, PUSH]);
        // A branch that had no upstream and now has one was pushed too.
        let (observed, _) = derive(
            Some("aaa"),
            &entry(None, Some(&[])),
            &exit("aaa", Some("aaa"), Some(true), Some(&[])),
        );
        assert_eq!(observed, vec![PUSH]);
    }

    #[test]
    fn an_upstream_that_moved_away_from_head_is_someone_elses_push() {
        let (observed, coverage) = derive(
            Some("aaa"),
            &entry(Some("up1"), Some(&[])),
            &exit("aaa", Some("theirs"), Some(false), Some(&[])),
        );
        assert!(observed.is_empty());
        assert_eq!(coverage, EffectsCoverage::Complete);
    }

    #[test]
    fn a_new_pull_request_number_is_open_pr() {
        let (observed, coverage) = derive(
            Some("aaa"),
            &entry(Some("up1"), Some(&[41])),
            &exit("aaa", Some("up1"), Some(true), Some(&[41, 42])),
        );
        assert_eq!(observed, vec![OPEN_PR]);
        assert_eq!(coverage, EffectsCoverage::Complete);
    }

    #[test]
    fn anything_that_could_not_be_asked_makes_coverage_partial_not_quiet() {
        // gh unavailable at entry.
        let (observed, coverage) = derive(
            Some("aaa"),
            &entry(Some("up1"), None),
            &exit("aaa", Some("up1"), Some(true), Some(&[7])),
        );
        assert!(observed.is_empty());
        assert_eq!(coverage, EffectsCoverage::Partial);
        // HEAD unknown at entry.
        let (_, coverage) = derive(
            None,
            &entry(Some("up1"), Some(&[])),
            &exit("aaa", Some("up1"), Some(true), Some(&[])),
        );
        assert_eq!(coverage, EffectsCoverage::Partial);
        // Upstream moved but reachability could not be decided: no claim, partial.
        let (observed, coverage) = derive(
            Some("aaa"),
            &entry(Some("up1"), Some(&[])),
            &exit("aaa", Some("up2"), None, Some(&[])),
        );
        assert!(observed.is_empty());
        assert_eq!(coverage, EffectsCoverage::Partial);
        // git itself could not be asked about the upstream.
        let mut unknown = entry(None, Some(&[]));
        unknown.upstream_unknown = true;
        let (_, coverage) = derive(Some("aaa"), &unknown, &exit("aaa", None, None, Some(&[])));
        assert_eq!(coverage, EffectsCoverage::Partial);
    }
}
