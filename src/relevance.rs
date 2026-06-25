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
    let scope = compute_scope(runner, repo_root, changed, merge_base, head_available);
    let read_order = compute_read_order(changed);

    // History mining is bounded to one `git log` call, reused for coupling (and
    // risk, later). Shallow/empty history degrades to no hints plus one warning.
    let history = mine_log_commits(runner, repo_root, DEFAULT_LOG_WINDOW_COMMITS);
    let mut warnings = Vec::new();
    let coupling = match &history {
        Some(commits) if !commits.is_empty() => compute_coupling(commits, changed),
        _ => Vec::new(),
    };
    if !changed.is_empty() && history.as_ref().is_none_or(|commits| commits.is_empty()) {
        warnings.push(
            "Relevance history unavailable: shallow or insufficient Git history.".to_string(),
        );
    }

    RelevancePacket {
        scope,
        read_order,
        coupling,
        warnings,
        ..RelevancePacket::default()
    }
}

/// Default bounded history window for log mining. Conservative so the work stays
/// cheap on large repos; overridable via config later.
const DEFAULT_LOG_WINDOW_COMMITS: usize = 200;
/// A pair must co-change at least this many times to be worth surfacing.
const MIN_COUPLING_COUNT: usize = 2;
/// Cap on emitted coupling hints.
const MAX_COUPLING_HINTS: usize = 5;

/// Mine the last `window` non-merge commits as lists of changed file paths.
/// Returns `None` when `git log` fails, `Some(vec![])` when there is no history.
/// The `@@` marker prefixes each commit hash so file paths (which never start
/// with `@@`) parse unambiguously.
fn mine_log_commits(
    runner: &impl CommandRunner,
    repo_root: &Path,
    window: usize,
) -> Option<Vec<Vec<String>>> {
    let limit = window.to_string();
    let stdout = run_git_ok(
        runner,
        repo_root,
        &[
            "log",
            "--no-merges",
            "-n",
            &limit,
            "--name-only",
            "--pretty=format:@@%H",
        ],
    )?;
    let mut commits: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut started = false;
    for line in stdout.lines() {
        if line.starts_with("@@") {
            if started {
                commits.push(std::mem::take(&mut current));
            }
            started = true;
        } else {
            let file = line.trim();
            if !file.is_empty() {
                current.push(file.to_string());
            }
        }
    }
    if started {
        commits.push(current);
    }
    Some(commits)
}

/// Count how often each pair of *changed* files appeared together in the same
/// commit across the mined window. Only pairs at or above [`MIN_COUPLING_COUNT`]
/// are surfaced, ranked by count then path. Bounded by the changed-surface size.
fn compute_coupling(commits: &[Vec<String>], changed: &[PathBuf]) -> Vec<CouplingHint> {
    let changed_set: BTreeSet<&str> = changed.iter().filter_map(|path| path.to_str()).collect();
    if changed_set.len() < 2 {
        return Vec::new();
    }
    let mut pair_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for files in commits {
        let mut present: Vec<&str> = files
            .iter()
            .map(String::as_str)
            .filter(|file| changed_set.contains(file))
            .collect();
        present.sort_unstable();
        present.dedup();
        for (i, left) in present.iter().enumerate() {
            for right in &present[i + 1..] {
                *pair_counts
                    .entry((left.to_string(), right.to_string()))
                    .or_default() += 1;
            }
        }
    }
    let window = format!("last {DEFAULT_LOG_WINDOW_COMMITS} commits");
    let mut hints: Vec<CouplingHint> = pair_counts
        .into_iter()
        .filter(|(_, count)| *count >= MIN_COUPLING_COUNT)
        .map(|((left, right), count)| CouplingHint {
            left: PathBuf::from(left),
            right: PathBuf::from(right),
            count,
            window: window.clone(),
            source: RelevanceSource::GitLog,
        })
        .collect();
    hints.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(a.left.cmp(&b.left))
            .then(a.right.cmp(&b.right))
    });
    hints.truncate(MAX_COUPLING_HINTS);
    hints
}

/// Boring, file-pattern categories for read ordering. Order of the variants is
/// the read priority: packaging first, generated/unknown last. No source
/// parsing — purely path and name patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadCategory {
    Packaging,
    Docs,
    Manifest,
    Tests,
    Source,
    Lockfile,
    Generated,
}

impl ReadCategory {
    /// Lower reads first. Stable and total so ranking is deterministic.
    fn priority(self) -> u8 {
        match self {
            ReadCategory::Packaging => 1,
            ReadCategory::Docs => 2,
            ReadCategory::Manifest => 3,
            ReadCategory::Tests => 4,
            ReadCategory::Source => 5,
            ReadCategory::Lockfile => 6,
            ReadCategory::Generated => 7,
        }
    }

    fn reason(self) -> &'static str {
        match self {
            ReadCategory::Packaging => "packaging surface",
            ReadCategory::Docs => "changed docs",
            ReadCategory::Manifest => "manifest / dependency source",
            ReadCategory::Tests => "tests",
            ReadCategory::Source => "source files",
            ReadCategory::Lockfile => "lockfile — dependency ripple, read after the manifest",
            ReadCategory::Generated => "generated or unknown",
        }
    }
}

const LOCKFILES: &[&str] = &[
    "cargo.lock",
    "mix.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "go.sum",
    "poetry.lock",
    "gemfile.lock",
    "composer.lock",
];

const MANIFESTS: &[&str] = &[
    "cargo.toml",
    "mix.exs",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "setup.py",
    "gemfile",
    "composer.json",
];

fn classify_path(path: &Path) -> ReadCategory {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    let segments: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => segment.to_str().map(str::to_ascii_lowercase),
            _ => None,
        })
        .collect();
    let has_segment = |segment: &str| segments.iter().any(|seg| seg == segment);
    let is_yaml = ext == "yml" || ext == "yaml";

    if LOCKFILES.contains(&name.as_str()) {
        return ReadCategory::Lockfile;
    }
    if MANIFESTS.contains(&name.as_str()) || name.ends_with(".gemspec") {
        return ReadCategory::Manifest;
    }
    if name == "dockerfile"
        || name == "containerfile"
        || ext == "dockerfile"
        || ((name.starts_with("docker-compose") || name.starts_with("compose")) && is_yaml)
        || has_segment("deploy")
        || has_segment("k8s")
        || has_segment("charts")
        || has_segment("helm")
        || has_segment("infra")
    {
        return ReadCategory::Packaging;
    }
    if has_segment("generated")
        || name.ends_with(".pb.go")
        || name.ends_with(".pb.rs")
        || name.ends_with("_pb2.py")
        || name.contains(".generated.")
        || name.ends_with(".min.js")
        || name.ends_with(".min.css")
    {
        return ReadCategory::Generated;
    }
    if matches!(ext.as_str(), "md" | "mdx" | "rst")
        || has_segment("docs")
        || matches!(
            stem.as_str(),
            "README" | "CHANGELOG" | "LICENSE" | "CONTRIBUTING" | "AUTHORS"
        )
    {
        return ReadCategory::Docs;
    }
    if has_segment("test")
        || has_segment("tests")
        || has_segment("spec")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.starts_with("test_")
        || name.contains("_spec.")
        || name.contains(".spec.")
    {
        return ReadCategory::Tests;
    }
    ReadCategory::Source
}

/// Group the changed surface into read-order categories, one entry per present
/// category, ranked by [`ReadCategory::priority`]. Deterministic: paths within
/// an entry are sorted.
fn compute_read_order(changed: &[PathBuf]) -> Vec<ReadOrderEntry> {
    let mut grouped: BTreeMap<u8, (ReadCategory, Vec<PathBuf>)> = BTreeMap::new();
    for path in changed {
        let category = classify_path(path);
        grouped
            .entry(category.priority())
            .or_insert_with(|| (category, Vec::new()))
            .1
            .push(path.clone());
    }
    grouped
        .into_values()
        .map(|(category, mut paths)| {
            paths.sort();
            ReadOrderEntry {
                paths,
                reason: category.reason().to_string(),
                priority: category.priority(),
                source: RelevanceSource::Heuristic,
            }
        })
        .collect()
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

    #[test]
    fn classify_path_recognises_boring_categories() {
        assert_eq!(
            classify_path(Path::new("Dockerfile")),
            ReadCategory::Packaging
        );
        assert_eq!(
            classify_path(Path::new("deploy/container/ahti.toml")),
            ReadCategory::Packaging
        );
        assert_eq!(classify_path(Path::new("docs/ops.md")), ReadCategory::Docs);
        assert_eq!(
            classify_path(Path::new("ahti-query/Cargo.toml")),
            ReadCategory::Manifest
        );
        assert_eq!(
            classify_path(Path::new("Cargo.lock")),
            ReadCategory::Lockfile
        );
        assert_eq!(
            classify_path(Path::new("tests/foo.rs")),
            ReadCategory::Tests
        );
        assert_eq!(
            classify_path(Path::new("src/foo_test.rs")),
            ReadCategory::Tests
        );
        assert_eq!(
            classify_path(Path::new("api/types.pb.go")),
            ReadCategory::Generated
        );
        assert_eq!(
            classify_path(Path::new("src/cold_scan.rs")),
            ReadCategory::Source
        );
    }

    #[test]
    fn mine_log_parses_marker_format() {
        let runner = FakeRunner::default().with(
            &[
                "log",
                "--no-merges",
                "-n",
                "200",
                "--name-only",
                "--pretty=format:@@%H",
            ],
            "@@hash1\nsrc/a.rs\nsrc/b.rs\n\n@@hash2\nsrc/a.rs\n",
        );
        let commits = mine_log_commits(&runner, Path::new("/repo"), 200).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0], vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(commits[1], vec!["src/a.rs"]);
    }

    #[test]
    fn coupling_counts_pairs_among_changed_files() {
        let commits = vec![
            vec!["a.rs".to_string(), "b.rs".to_string(), "z.rs".to_string()],
            vec!["a.rs".to_string(), "b.rs".to_string()],
            vec!["a.rs".to_string(), "c.rs".to_string()],
        ];
        let changed = vec![
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"),
            PathBuf::from("c.rs"),
        ];
        let hints = compute_coupling(&commits, &changed);
        // a~b co-changed twice (kept); a~c once (below threshold); z not changed.
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].left, PathBuf::from("a.rs"));
        assert_eq!(hints[0].right, PathBuf::from("b.rs"));
        assert_eq!(hints[0].count, 2);
        assert_eq!(hints[0].source, RelevanceSource::GitLog);
    }

    #[test]
    fn coupling_empty_with_fewer_than_two_changed() {
        let hints = compute_coupling(&[vec!["a.rs".to_string()]], &[PathBuf::from("a.rs")]);
        assert!(hints.is_empty());
    }

    #[test]
    fn relevance_warns_on_unavailable_history() {
        // FakeRunner returns failure for every command, so log mining yields None.
        let runner = FakeRunner::default();
        let changed = vec![PathBuf::from("src/a.rs")];
        let packet = compute_relevance(&runner, Path::new("/repo"), &changed, Some("base"), true);
        assert!(packet.coupling.is_empty());
        assert!(
            packet
                .warnings
                .iter()
                .any(|warning| warning.contains("Relevance history unavailable")),
            "expected a history-unavailable warning, got {:?}",
            packet.warnings
        );
    }

    #[test]
    fn read_order_ranks_manifest_before_lockfile_and_source() {
        let changed = vec![
            PathBuf::from("Cargo.lock"),
            PathBuf::from("src/cold_scan.rs"),
            PathBuf::from("ahti-query/Cargo.toml"),
            PathBuf::from("Dockerfile"),
            PathBuf::from("docs/ops.md"),
        ];
        let order = compute_read_order(&changed);
        let reasons: Vec<&str> = order.iter().map(|entry| entry.reason.as_str()).collect();
        // packaging, docs, manifest, source, lockfile — manifest before its ripple.
        assert_eq!(
            reasons,
            vec![
                "packaging surface",
                "changed docs",
                "manifest / dependency source",
                "source files",
                "lockfile — dependency ripple, read after the manifest",
            ]
        );
        // priorities are strictly increasing.
        let priorities: Vec<u8> = order.iter().map(|entry| entry.priority).collect();
        assert!(priorities.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
