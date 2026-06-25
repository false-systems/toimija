//! Git-derived relevance.
//!
//! Toimija reports Git *state*; this layer mines bounded Git *history* to help
//! an agent decide what to read first and what to ignore. It is advisory and
//! pure Git — `git status`/`diff`/`log` (and `blame` only when cheap). It does
//! not parse source, talk to a network, or build an index. Every hint carries
//! an honest [`RelevanceSource`] so the reader knows it is heuristic, not
//! semantic truth.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::command::CommandRunner;
use crate::git::run_git_ok;

/// Top-level directories that hold many sibling projects. When a changed path
/// sits under one, the scope area is the *second* level (`apps/foo`) rather than
/// the coarse container, so a monorepo does not collapse to a single area.
const CONTAINER_DIRS: &[&str] = &["apps", "crates", "packages", "libs", "services"];

/// The relevance hints attached to a context packet. Every field is empty when
/// the underlying Git data is unavailable (shallow clone, no history, …), so an
/// absent section always means "could not derive", never "nothing to see".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelevancePacket {
    pub scope: Option<ScopeHints>,
    pub read_order: Vec<ReadOrderEntry>,
    pub coupling: Vec<CouplingHint>,
    pub risk: Vec<RiskHint>,
    pub warnings: Vec<String>,
}

impl RelevancePacket {
    /// True when there is nothing to render — used to keep the packet quiet
    /// rather than emit empty headings.
    pub fn is_empty(&self) -> bool {
        self.scope.is_none()
            && self.read_order.is_empty()
            && self.coupling.is_empty()
            && self.risk.is_empty()
            && self.warnings.is_empty()
    }
}

/// What the branch is touching versus the areas it leaves alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeHints {
    pub in_play: Vec<ScopeArea>,
    pub out_of_scope: Vec<ScopeArea>,
    pub source: RelevanceSource,
}

/// A top-level area (directory or notable file group) with the paths that put
/// it in play and a short, evidence-only reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeArea {
    pub label: String,
    pub paths: Vec<PathBuf>,
    pub reason: String,
}

/// One ranked group of paths to read, lower `priority` first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadOrderEntry {
    pub paths: Vec<PathBuf>,
    pub reason: String,
    pub priority: u8,
    pub source: RelevanceSource,
}

/// Two files that have historically changed together within the mined window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CouplingHint {
    pub left: PathBuf,
    pub right: PathBuf,
    pub count: usize,
    pub window: String,
    pub source: RelevanceSource,
}

/// A file flagged as risky by a cheap, bounded Git-history heuristic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskHint {
    pub path: PathBuf,
    pub kind: RiskKind,
    pub reason: String,
    pub source: RelevanceSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskKind {
    HighChurn,
    YoungFile,
    OldLoadBearing,
}

/// Where a hint came from. Kept honest so an agent can weigh it: `GitLog`/
/// `GitBlame`/`MergeBaseDiff` are real Git evidence; `Heuristic` is a
/// file-pattern guess; `Unavailable` means the data could not be derived.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelevanceSource {
    MergeBaseDiff,
    WorkingTreeAndIndex,
    GitLog,
    GitBlame,
    Heuristic,
    Unavailable,
}

/// Compute the relevance hints for a changed surface. Pure Git: no source
/// parsing, no network. `changed` is the already-filtered changed surface;
/// `merge_base`/`head_available` describe the Git state the hints are derived
/// from. Each sub-computation degrades to empty when its Git data is missing.
pub fn compute_relevance(
    runner: &impl CommandRunner,
    repo_root: &Path,
    changed: &[PathBuf],
    merge_base: Option<&str>,
    head_available: bool,
) -> RelevancePacket {
    RelevancePacket {
        scope: compute_scope(runner, repo_root, changed, merge_base, head_available),
        ..RelevancePacket::default()
    }
}

/// The scope-area key for a path: its top-level component, or `container/child`
/// when the top level is a known container dir. `None` for an empty path.
fn area_key(path: &Path) -> Option<String> {
    let segments: Vec<&str> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => segment.to_str(),
            _ => None,
        })
        .collect();
    match segments.as_slice() {
        [] => None,
        [only] => Some((*only).to_string()),
        [first, second, ..] => {
            if CONTAINER_DIRS.contains(first) {
                Some(format!("{first}/{second}"))
            } else {
                Some((*first).to_string())
            }
        }
    }
}

/// Tracked top-level directory areas (container dirs expanded one level), used
/// as the universe for "out of scope". Empty when HEAD has no tree.
fn tracked_area_dirs(runner: &impl CommandRunner, repo_root: &Path) -> Vec<String> {
    let Some(top) = run_git_ok(runner, repo_root, &["ls-tree", "--name-only", "-d", "HEAD"]) else {
        return Vec::new();
    };
    let mut areas = Vec::new();
    for dir in top.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if CONTAINER_DIRS.contains(&dir) {
            let spec = format!("HEAD:{dir}");
            match run_git_ok(runner, repo_root, &["ls-tree", "--name-only", "-d", &spec]) {
                Some(children) if !children.trim().is_empty() => {
                    for child in children
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                    {
                        areas.push(format!("{dir}/{child}"));
                    }
                }
                _ => areas.push(dir.to_string()),
            }
        } else {
            areas.push(dir.to_string());
        }
    }
    areas
}

fn compute_scope(
    runner: &impl CommandRunner,
    repo_root: &Path,
    changed: &[PathBuf],
    merge_base: Option<&str>,
    head_available: bool,
) -> Option<ScopeHints> {
    if changed.is_empty() {
        return None;
    }
    let source = if merge_base.is_some() {
        RelevanceSource::MergeBaseDiff
    } else {
        RelevanceSource::WorkingTreeAndIndex
    };

    let mut grouped: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in changed {
        if let Some(key) = area_key(path) {
            grouped.entry(key).or_default().push(path.clone());
        }
    }
    let in_play_keys: BTreeSet<String> = grouped.keys().cloned().collect();
    let in_play: Vec<ScopeArea> = grouped
        .into_iter()
        .map(|(label, paths)| ScopeArea {
            reason: format!(
                "{} changed file{}",
                paths.len(),
                if paths.len() == 1 { "" } else { "s" }
            ),
            label,
            paths,
        })
        .collect();

    // Out-of-scope is only honest when we can enumerate the repo's areas and
    // there is more than one — otherwise the structure is too unclear to claim
    // anything is "untouched".
    let out_of_scope = if head_available {
        let universe = tracked_area_dirs(runner, repo_root);
        if universe.len() <= 1 {
            Vec::new()
        } else {
            universe
                .into_iter()
                .filter(|area| !in_play_keys.contains(area))
                .map(|label| ScopeArea {
                    label,
                    paths: Vec::new(),
                    reason: "untouched by this branch".to_string(),
                })
                .collect()
        }
    } else {
        Vec::new()
    };

    Some(ScopeHints {
        in_play,
        out_of_scope,
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::command::CommandOutput;

    #[derive(Default)]
    struct FakeRunner {
        outputs: HashMap<Vec<String>, CommandOutput>,
    }

    impl FakeRunner {
        fn with(mut self, args: &[&str], stdout: &str) -> Self {
            self.outputs.insert(
                args.iter().map(|arg| arg.to_string()).collect(),
                CommandOutput {
                    status: 0,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                },
            );
            self
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            assert_eq!(program, "git");
            Ok(self
                .outputs
                .get(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
                .cloned()
                .unwrap_or(CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: String::new(),
                }))
        }
    }

    fn labels(areas: &[ScopeArea]) -> Vec<&str> {
        areas.iter().map(|area| area.label.as_str()).collect()
    }

    #[test]
    fn area_key_groups_top_level_and_containers() {
        assert_eq!(
            area_key(Path::new("Dockerfile")).as_deref(),
            Some("Dockerfile")
        );
        assert_eq!(area_key(Path::new("src/main.rs")).as_deref(), Some("src"));
        assert_eq!(
            area_key(Path::new("apps/vartio_core/lib/x.ex")).as_deref(),
            Some("apps/vartio_core")
        );
        assert_eq!(area_key(Path::new("")), None);
    }

    #[test]
    fn scope_splits_in_play_from_out_of_scope() {
        let runner = FakeRunner::default()
            .with(
                &["ls-tree", "--name-only", "-d", "HEAD"],
                "apps\ndeploy\ndocs\n",
            )
            .with(
                &["ls-tree", "--name-only", "-d", "HEAD:apps"],
                "vartio_core\nvartio_runtime\n",
            );
        let changed = vec![
            PathBuf::from("apps/vartio_core/lib/x.ex"),
            PathBuf::from("deploy/container/a.toml"),
        ];
        let scope =
            compute_scope(&runner, Path::new("/repo"), &changed, Some("abc123"), true).unwrap();
        assert_eq!(scope.source, RelevanceSource::MergeBaseDiff);
        assert_eq!(labels(&scope.in_play), vec!["apps/vartio_core", "deploy"]);
        // apps expanded one level; only the untouched areas remain.
        assert_eq!(
            labels(&scope.out_of_scope),
            vec!["apps/vartio_runtime", "docs"]
        );
    }

    #[test]
    fn scope_omits_out_of_scope_when_structure_unclear() {
        let runner = FakeRunner::default().with(&["ls-tree", "--name-only", "-d", "HEAD"], "src\n");
        let changed = vec![PathBuf::from("src/main.rs")];
        let scope = compute_scope(&runner, Path::new("/repo"), &changed, None, true).unwrap();
        assert_eq!(scope.source, RelevanceSource::WorkingTreeAndIndex);
        assert!(scope.out_of_scope.is_empty());
    }

    #[test]
    fn scope_is_none_without_changes() {
        let runner = FakeRunner::default();
        assert!(compute_scope(&runner, Path::new("/repo"), &[], Some("x"), true).is_none());
    }
}
