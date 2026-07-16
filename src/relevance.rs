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
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::adapters::LanguageAdapter;
use crate::command::CommandRunner;
use crate::coverage::Coverage;
use crate::git::{run_git_observed, run_git_ok};

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
    /// Coverage of the bounded history mining behind `coupling` and `risk`
    /// (R10, TA-005): empty hints with `available` coverage mean the history
    /// really contains no signal; `failed` means `git log` did not deliver and
    /// the hints are unavailable, not absent.
    #[serde(default = "Coverage::unrecorded")]
    pub history_coverage: Coverage,
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

/// The compiled Git workset: what the branch is touching versus the areas it
/// leaves alone. This is evidence, not the session's declared scope.
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
    #[serde(with = "crate::path_codec::vec")]
    pub paths: Vec<PathBuf>,
    pub reason: String,
}

/// One ranked group of paths to read, lower `priority` first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadOrderEntry {
    #[serde(with = "crate::path_codec::vec")]
    pub paths: Vec<PathBuf>,
    pub reason: String,
    pub priority: u8,
    pub source: RelevanceSource,
}

/// A changed file with a companion that is absent from this diff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CouplingHint {
    #[serde(with = "crate::path_codec::one")]
    pub changed: PathBuf,
    #[serde(with = "crate::path_codec::one")]
    pub missing: PathBuf,
    pub reason: String,
    pub source: RelevanceSource,
}

/// A file flagged as risky by a cheap, bounded Git-history heuristic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskHint {
    #[serde(with = "crate::path_codec::one")]
    pub path: PathBuf,
    pub kind: RiskKind,
    pub reason: String,
    pub action: String,
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

pub struct RelevanceInput<'a> {
    pub adapter: &'a dyn LanguageAdapter,
    pub changed: &'a [PathBuf],
    pub branch_added: &'a [PathBuf],
    pub scope_source: RelevanceSource,
    pub merge_base: Option<&'a str>,
    pub head_available: bool,
}

/// Compute the relevance hints for a changed surface. Pure Git: no source
/// parsing, no network. `changed` is the already-filtered changed surface;
/// `merge_base`/`head_available` describe the Git state the hints are derived
/// from. Each sub-computation degrades to empty when its Git data is missing.
pub fn compute_relevance(
    runner: &impl CommandRunner,
    repo_root: &Path,
    input: RelevanceInput<'_>,
) -> RelevancePacket {
    let started = Instant::now();
    let changed = input.changed;
    let scope = compute_scope(
        runner,
        repo_root,
        changed,
        input.scope_source,
        input.head_available,
    );
    debug!(
        event = "toimija.relevance.scope.computed",
        in_play = scope.as_ref().map_or(0, |scope| scope.in_play.len()),
        out_of_scope = scope.as_ref().map_or(0, |scope| scope.out_of_scope.len())
    );
    let read_order = compute_read_order(changed, input.branch_added);
    debug!(
        event = "toimija.relevance.read_order.computed",
        entries = read_order.len()
    );

    // History mining starts from the merge-base, not HEAD, so the branch's own
    // commits do not count as historical evidence (they would make any file the
    // branch repeatedly edits look "coupled"). Falls back to HEAD when there is
    // no merge-base. Bounded to one `git log` call, reused for coupling and risk.
    let log_ref = input.merge_base.unwrap_or("HEAD");
    let history = mine_log_commits(runner, repo_root, log_ref, DEFAULT_LOG_WINDOW_COMMITS);
    let history_coverage = match &history {
        Ok(_) => Coverage::available(HISTORY_SOURCE),
        Err(reason) => Coverage::failed(HISTORY_SOURCE, reason.clone()),
    };
    let mut warnings = Vec::new();
    let (coupling, risk) = match &history {
        Ok(commits) if !commits.is_empty() => {
            let companion_tests = companion_tests_by_changed(repo_root, input.adapter, changed);
            let coupling = compute_coupling(commits, changed, &companion_tests);
            debug!(
                event = "toimija.relevance.coupling.computed",
                coupling_entries = coupling.len(),
                commits_scanned = commits.len()
            );
            // Young surface comes from two places: files added in base history
            // (mined) and files this branch itself added (never in base-history
            // mining, but the youngest surface there is).
            let base_added =
                mine_added_files(runner, repo_root, log_ref, DEFAULT_LOG_WINDOW_COMMITS);
            let base_tracked = mine_tracked_files(runner, repo_root, log_ref);
            let branch_added_set: BTreeSet<String> = input
                .branch_added
                .iter()
                .filter_map(|path| path.to_str().map(str::to_string))
                .collect();
            let risk = compute_risk(
                commits,
                &base_added,
                &base_tracked,
                &branch_added_set,
                changed,
                &companion_tests,
            );
            debug!(
                event = "toimija.relevance.risk.computed",
                risk_entries = risk.len()
            );
            (coupling, risk)
        }
        _ => (Vec::new(), Vec::new()),
    };
    if !changed.is_empty() {
        match &history {
            // Failure is a blind spot, not an empty observation — say so with
            // words that cannot be read as "there is nothing to see".
            Err(_) => {
                debug!(event = "toimija.relevance.history.failed");
                warnings.push(
                    "Relevance history collection failed. Coupling and risk hints are unavailable, not absent."
                        .to_string(),
                );
            }
            Ok(commits) if commits.is_empty() => {
                debug!(event = "toimija.relevance.history.unavailable");
                warnings.push(
                    "Relevance history unavailable: shallow or insufficient Git history."
                        .to_string(),
                );
            }
            Ok(_) => {}
        }
    }
    debug!(
        event = "toimija.relevance.computed",
        changed_files_count = changed.len(),
        commits_scanned = history.as_ref().map_or(0, Vec::len),
        coupling_entries = coupling.len(),
        risk_entries = risk.len(),
        duration_ms = started.elapsed().as_millis() as u64
    );

    RelevancePacket {
        scope,
        read_order,
        coupling,
        risk,
        warnings,
        history_coverage,
    }
}

/// Default bounded history window for log mining. Conservative so the work stays
/// cheap on large repos; overridable via config later.
pub(crate) const DEFAULT_LOG_WINDOW_COMMITS: usize = 200;
/// A missing history partner must co-change at least this many times to be
/// worth surfacing.
const MIN_COUPLING_COUNT: usize = 2;
/// A missing history partner must appear in at least this fraction of the
/// changed file's edits before we suggest it. Conservative: silence beats a
/// wrong "you forgot this".
const MIN_COUPLING_CONFIDENCE_NUMERATOR: usize = 3;
const MIN_COUPLING_CONFIDENCE_DENOMINATOR: usize = 5;
/// Cap on emitted coupling hints.
const MAX_COUPLING_HINTS: usize = 5;
/// Cap on missing companions surfaced per changed file, so one hub file (e.g. a
/// CI workflow with many co-change partners) cannot fill every coupling slot.
const MAX_COUPLING_PER_FILE: usize = 2;
/// A file touched by at least this many commits in the window is "high churn".
const HIGH_CHURN_THRESHOLD: usize = 8;
/// Cap on emitted risk hints.
const MAX_RISK_HINTS: usize = 5;

/// The set of paths added (`--diff-filter=A`) within the window — used to flag
/// young/new surface. Empty on any failure.
fn mine_added_files(
    runner: &impl CommandRunner,
    repo_root: &Path,
    start_ref: &str,
    window: usize,
) -> BTreeSet<String> {
    let limit = window.to_string();
    run_git_ok(
        runner,
        repo_root,
        &[
            "log",
            start_ref,
            "--no-merges",
            "-n",
            &limit,
            "--diff-filter=A",
            "--name-only",
            "--pretty=format:",
        ],
    )
    .map(|stdout| {
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Files tracked at the mined base ref. Used to distinguish old/stable files
/// from genuinely new or untracked paths that simply have no history.
fn mine_tracked_files(
    runner: &impl CommandRunner,
    repo_root: &Path,
    start_ref: &str,
) -> BTreeSet<String> {
    run_git_ok(
        runner,
        repo_root,
        &["ls-tree", "-r", "--name-only", start_ref],
    )
    .map(|stdout| {
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

fn companion_tests_by_changed(
    repo_root: &Path,
    adapter: &dyn LanguageAdapter,
    changed: &[PathBuf],
) -> BTreeMap<PathBuf, Vec<PathBuf>> {
    changed
        .iter()
        .filter_map(|path| {
            let tests = adapter.companion_tests(repo_root, path);
            if tests.is_empty() {
                None
            } else {
                Some((path.clone(), tests))
            }
        })
        .collect()
}

/// Flag changed files that look risky from cheap Git-history signals. A file is
/// flagged at most once, in this precedence: high churn (many commits in the
/// window) > young because this branch added it > young because it was added
/// recently in base history. The branch's own new files rank above base-history
/// additions so they are not buried under the cap.
fn compute_risk(
    commits: &[Vec<String>],
    base_added: &BTreeSet<String>,
    base_tracked: &BTreeSet<String>,
    branch_added: &BTreeSet<String>,
    changed: &[PathBuf],
    companion_tests: &BTreeMap<PathBuf, Vec<PathBuf>>,
) -> Vec<RiskHint> {
    let mut churn: BTreeMap<&str, usize> = BTreeMap::new();
    for files in commits {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for file in files {
            if seen.insert(file.as_str()) {
                *churn.entry(file.as_str()).or_default() += 1;
            }
        }
    }
    let window = format!("last {DEFAULT_LOG_WINDOW_COMMITS} commits");
    let changed_set = changed_set(changed);
    // (rank, hint): lower rank shown first, ties broken by path.
    let mut ranked: Vec<(u8, RiskHint)> = Vec::new();
    for path in changed {
        let Some(key) = path.to_str() else {
            continue;
        };
        let count = churn.get(key).copied().unwrap_or(0);
        let has_test_in_diff = companion_tests.get(path).is_some_and(|tests| {
            tests
                .iter()
                .any(|test| changed_set.contains(test.as_path()))
        });
        let has_test_missing = companion_tests.get(path).is_some_and(|tests| {
            tests
                .iter()
                .any(|test| !changed_set.contains(test.as_path()))
        });
        let (rank, kind, reason, action) = if count >= HIGH_CHURN_THRESHOLD {
            (
                0,
                RiskKind::HighChurn,
                format!("high churn — {count} commits in the {window}"),
                "hot file — keep the change small and well-tested".to_string(),
            )
        } else if branch_added.contains(key) {
            (
                1,
                RiskKind::YoungFile,
                "added on this branch — new surface".to_string(),
                young_file_action(has_test_missing, has_test_in_diff),
            )
        } else if base_added.contains(key) {
            (
                2,
                RiskKind::YoungFile,
                "recently added — young surface".to_string(),
                young_file_action(has_test_missing, has_test_in_diff),
            )
        } else if count == 0 && base_tracked.contains(key) {
            (
                3,
                RiskKind::OldLoadBearing,
                format!("not touched in the {window} — stable/load-bearing surface"),
                "stable file — read surrounding context before changing".to_string(),
            )
        } else {
            continue;
        };
        ranked.push((
            rank,
            RiskHint {
                path: path.clone(),
                kind,
                reason,
                action,
                source: RelevanceSource::GitLog,
            },
        ));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.path.cmp(&b.1.path)));
    ranked
        .into_iter()
        .take(MAX_RISK_HINTS)
        .map(|(_, hint)| hint)
        .collect()
}

fn young_file_action(has_test_missing: bool, has_test_in_diff: bool) -> String {
    if has_test_missing && !has_test_in_diff {
        "new surface without a changed companion test — consider adding coverage".to_string()
    } else {
        "new surface — confirm coverage is intentional".to_string()
    }
}

/// The coverage source label for bounded history mining.
const HISTORY_SOURCE: &str = "git log --name-only (bounded window)";

/// Mine the last `window` non-merge commits as lists of changed file paths.
/// Returns `Err` with a safe diagnostic when `git log` fails, `Ok(vec![])`
/// when there is no history — the two must stay distinct (R10, TA-005).
/// The `@@` marker prefixes each commit hash so file paths (which never start
/// with `@@`) parse unambiguously.
pub(crate) fn mine_log_commits(
    runner: &impl CommandRunner,
    repo_root: &Path,
    start_ref: &str,
    window: usize,
) -> Result<Vec<Vec<String>>, String> {
    let limit = window.to_string();
    let stdout = run_git_observed(
        runner,
        repo_root,
        &[
            "log",
            start_ref,
            "--no-merges",
            "-n",
            &limit,
            "--name-only",
            "--pretty=format:@@%H",
        ],
    )
    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())?;
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
    Ok(commits)
}

/// Surface companions that are absent from the current diff. Primary hints come
/// from language adapter test mapping; secondary hints come from conservative
/// co-change history above support and confidence thresholds.
fn compute_coupling(
    commits: &[Vec<String>],
    changed: &[PathBuf],
    companion_tests: &BTreeMap<PathBuf, Vec<PathBuf>>,
) -> Vec<CouplingHint> {
    let changed_set: BTreeSet<&Path> = changed.iter().map(PathBuf::as_path).collect();
    let mut hints: BTreeMap<(PathBuf, PathBuf), CouplingHint> = BTreeMap::new();
    for (changed_path, tests) in companion_tests {
        for test in tests {
            if changed_set.contains(test.as_path()) {
                continue;
            }
            hints
                .entry((changed_path.clone(), test.clone()))
                .or_insert_with(|| CouplingHint {
                    changed: changed_path.clone(),
                    missing: test.clone(),
                    reason: "companion test exists but is not in this diff — confirm intentional"
                        .to_string(),
                    source: RelevanceSource::Heuristic,
                });
        }
    }

    let changed_keys: BTreeSet<&str> = changed.iter().filter_map(|path| path.to_str()).collect();
    if changed_keys.is_empty() {
        return hints.into_values().take(MAX_COUPLING_HINTS).collect();
    }

    let mut edit_counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut pair_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for files in commits {
        let mut present_changed: Vec<&str> = files
            .iter()
            .map(String::as_str)
            .filter(|file| changed_keys.contains(file))
            .collect();
        present_changed.sort_unstable();
        present_changed.dedup();
        for changed_file in &present_changed {
            *edit_counts.entry(*changed_file).or_default() += 1;
        }

        let mut all_files: Vec<&str> = files.iter().map(String::as_str).collect();
        all_files.sort_unstable();
        all_files.dedup();
        for changed_file in &present_changed {
            for partner in &all_files {
                if partner == changed_file || changed_keys.contains(partner) {
                    continue;
                }
                *pair_counts
                    .entry(((*changed_file).to_string(), (*partner).to_string()))
                    .or_default() += 1;
            }
        }
    }
    let window = format!("last {DEFAULT_LOG_WINDOW_COMMITS} commits");
    let history_hints: Vec<(usize, usize, CouplingHint)> = pair_counts
        .into_iter()
        .filter_map(|((changed_file, partner), count)| {
            if count < MIN_COUPLING_COUNT {
                return None;
            }
            let edits = edit_counts.get(changed_file.as_str()).copied().unwrap_or(0);
            if edits == 0
                || count * MIN_COUPLING_CONFIDENCE_DENOMINATOR
                    < edits * MIN_COUPLING_CONFIDENCE_NUMERATOR
            {
                return None;
            }
            Some((
                count,
                edits,
                CouplingHint {
                    changed: PathBuf::from(changed_file),
                    missing: PathBuf::from(partner),
                    reason: format!(
                        "changed together {count}/{edits} times in the {window}; missing from this diff — confirm intentional"
                    ),
                    source: RelevanceSource::GitLog,
                },
            ))
        })
        .collect();
    // Rank candidates so the caps keep the strongest signal, not the
    // alphabetically-first: test-mapping companions (a real test exists) first,
    // then history partners by confidence. Dedup by (changed, missing) — the
    // test-mapping hint wins.
    // (rank, support): rank 0 = test-mapping companions first, then history by
    // co-change support (count) descending. Confidence is the gate applied
    // above; among gated candidates, more co-changes is the stronger "you
    // usually touch this too" signal (9/11 beats a coincidental 2/2).
    let mut ranked: Vec<(u8, usize, CouplingHint)> = Vec::new();
    let mut seen: BTreeSet<(PathBuf, PathBuf)> = BTreeSet::new();
    for (key, hint) in hints {
        seen.insert(key);
        ranked.push((0, usize::MAX, hint));
    }
    for (count, _edits, hint) in history_hints {
        if !seen.insert((hint.changed.clone(), hint.missing.clone())) {
            continue;
        }
        ranked.push((1, count, hint));
    }
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(b.1.cmp(&a.1))
            .then(a.2.changed.cmp(&b.2.changed))
            .then(a.2.missing.cmp(&b.2.missing))
    });

    let mut per_file: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut result = Vec::new();
    for (_, _, hint) in ranked {
        let used = per_file.entry(hint.changed.clone()).or_default();
        if *used >= MAX_COUPLING_PER_FILE {
            continue;
        }
        *used += 1;
        result.push(hint);
        if result.len() >= MAX_COUPLING_HINTS {
            break;
        }
    }
    result
}

fn changed_set(changed: &[PathBuf]) -> BTreeSet<&Path> {
    changed.iter().map(PathBuf::as_path).collect()
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
    /// Source churn corroborated as dependency-bump fallout: the file sits
    /// under a changed manifest whose ecosystem lockfile changed in the same
    /// diff. Ranked dead last — reading the ripple is exactly the expensive
    /// grepping the relevance layer exists to cut.
    MechanicalRipple,
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
            ReadCategory::MechanicalRipple => 8,
        }
    }

    fn reason(self) -> &'static str {
        match self {
            ReadCategory::Packaging => "packaging surface",
            ReadCategory::Docs => "changed docs",
            ReadCategory::Manifest => "manifest / dependency source",
            ReadCategory::Tests => "tests",
            ReadCategory::Source => "source files",
            ReadCategory::Lockfile => {
                "lockfile — possible dependency-related change; inspect after the manifest"
            }
            ReadCategory::Generated => "generated or unknown",
            ReadCategory::MechanicalRipple => {
                "possible dependency-related change — source under a manifest whose lockfile \
                 changed in the same diff; inspect after the manifest"
            }
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

/// The dependency ecosystem a manifest file name belongs to, used to pair a
/// manifest change with its lockfile in the same diff. Names are lowercased.
fn manifest_ecosystem(name: &str) -> Option<&'static str> {
    match name {
        "cargo.toml" => Some("cargo"),
        "mix.exs" => Some("mix"),
        "package.json" => Some("npm"),
        "go.mod" => Some("go"),
        "pyproject.toml" => Some("python"),
        "gemfile" => Some("ruby"),
        "composer.json" => Some("php"),
        _ => None,
    }
}

/// The dependency ecosystem a lockfile name belongs to. Counterpart of
/// [`manifest_ecosystem`]; only ecosystems with both halves can corroborate.
fn lockfile_ecosystem(name: &str) -> Option<&'static str> {
    match name {
        "cargo.lock" => Some("cargo"),
        "mix.lock" => Some("mix"),
        "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" => Some("npm"),
        "go.sum" => Some("go"),
        "poetry.lock" => Some("python"),
        "gemfile.lock" => Some("ruby"),
        "composer.lock" => Some("php"),
        _ => None,
    }
}

fn file_name_lower(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Directories whose source churn is a dependency-bump ripple candidate: the
/// parents of changed manifests whose ecosystem lockfile *also* changed in this
/// diff. Conservative by corroboration — a manifest edit alone (feature work)
/// or a lockfile edit alone never tags anything; a false `mechanical` tag
/// (telling the agent to skip authored intent) is worse than no tag.
fn ripple_roots(changed: &[PathBuf]) -> Vec<PathBuf> {
    let locked: BTreeSet<&'static str> = changed
        .iter()
        .filter_map(|path| lockfile_ecosystem(&file_name_lower(path)))
        .collect();
    if locked.is_empty() {
        return Vec::new();
    }
    changed
        .iter()
        .filter(|path| {
            manifest_ecosystem(&file_name_lower(path))
                .is_some_and(|ecosystem| locked.contains(ecosystem))
        })
        .map(|path| path.parent().unwrap_or(Path::new("")).to_path_buf())
        .collect()
}

/// Group the changed surface into read-order categories, one entry per present
/// category, ranked by [`ReadCategory::priority`]. Deterministic: paths within
/// an entry are sorted.
///
/// Source files under a corroborated dependency bump (see [`ripple_roots`]) are
/// re-tagged `MechanicalRipple` and ranked last — except files this branch
/// itself added: new files are authored intent, never dep-bump fallout.
fn compute_read_order(changed: &[PathBuf], branch_added: &[PathBuf]) -> Vec<ReadOrderEntry> {
    let ripple_roots = ripple_roots(changed);
    let branch_added: BTreeSet<&Path> = branch_added.iter().map(PathBuf::as_path).collect();
    let mut grouped: BTreeMap<u8, (ReadCategory, Vec<PathBuf>)> = BTreeMap::new();
    for path in changed {
        let mut category = classify_path(path);
        if category == ReadCategory::Source
            && !branch_added.contains(path.as_path())
            && ripple_roots.iter().any(|root| path.starts_with(root))
        {
            category = ReadCategory::MechanicalRipple;
        }
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
    source: RelevanceSource,
    head_available: bool,
) -> Option<ScopeHints> {
    if changed.is_empty() {
        return None;
    }

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
    use crate::adapters::generic::GenericAdapter;
    use crate::adapters::rust::RustAdapter;
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
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: Vec::new(),
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
                    stdout: Vec::new(),
                    stderr: Vec::new(),
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
        let scope = compute_scope(
            &runner,
            Path::new("/repo"),
            &changed,
            RelevanceSource::MergeBaseDiff,
            true,
        )
        .unwrap();
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
        let scope = compute_scope(
            &runner,
            Path::new("/repo"),
            &changed,
            RelevanceSource::WorkingTreeAndIndex,
            true,
        )
        .unwrap();
        assert_eq!(scope.source, RelevanceSource::WorkingTreeAndIndex);
        assert!(scope.out_of_scope.is_empty());
    }

    #[test]
    fn scope_is_none_without_changes() {
        let runner = FakeRunner::default();
        assert!(
            compute_scope(
                &runner,
                Path::new("/repo"),
                &[],
                RelevanceSource::Unavailable,
                true
            )
            .is_none()
        );
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
                "basesha",
                "--no-merges",
                "-n",
                "200",
                "--name-only",
                "--pretty=format:@@%H",
            ],
            "@@hash1\nsrc/a.rs\nsrc/b.rs\n\n@@hash2\nsrc/a.rs\n",
        );
        let commits = mine_log_commits(&runner, Path::new("/repo"), "basesha", 200).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0], vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(commits[1], vec!["src/a.rs"]);
    }

    #[test]
    fn history_is_mined_from_merge_base_not_head() {
        // The fake runner answers log only for the "basesha" ref. If mining used
        // HEAD instead of the merge-base, it would get nothing and no coupling.
        let runner = FakeRunner::default().with(
            &[
                "log",
                "basesha",
                "--no-merges",
                "-n",
                "200",
                "--name-only",
                "--pretty=format:@@%H",
            ],
            "@@h1\na.rs\nb.rs\n@@h2\na.rs\nb.rs\n",
        );
        let changed = vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")];
        let packet = compute_relevance(
            &runner,
            Path::new("/repo"),
            RelevanceInput {
                adapter: &GenericAdapter,
                changed: &changed,
                branch_added: &[],
                scope_source: RelevanceSource::MergeBaseDiff,
                merge_base: Some("basesha"),
                head_available: true,
            },
        );
        assert_eq!(
            packet.coupling.len(),
            0,
            "pairs already changed together should not be missing-companion hints"
        );
        assert!(packet.warnings.is_empty());
    }

    #[test]
    fn coupling_flags_existing_companion_test_left_out() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("tests")).unwrap();
        std::fs::write(temp.path().join("tests/foo.rs"), "").unwrap();
        let commits = Vec::new();
        let changed = vec![PathBuf::from("src/foo.rs")];
        let companions = companion_tests_by_changed(temp.path(), &RustAdapter, &changed);

        let hints = compute_coupling(&commits, &changed, &companions);

        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].changed, PathBuf::from("src/foo.rs"));
        assert_eq!(hints[0].missing, PathBuf::from("tests/foo.rs"));
        assert_eq!(hints[0].source, RelevanceSource::Heuristic);
    }

    #[test]
    fn coupling_does_not_flag_companion_test_already_changed() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("tests")).unwrap();
        std::fs::write(temp.path().join("tests/foo.rs"), "").unwrap();
        let commits = Vec::new();
        let changed = vec![PathBuf::from("src/foo.rs"), PathBuf::from("tests/foo.rs")];
        let companions = companion_tests_by_changed(temp.path(), &RustAdapter, &changed);

        let hints = compute_coupling(&commits, &changed, &companions);

        assert!(hints.is_empty());
    }

    #[test]
    fn coupling_flags_above_threshold_history_partner_not_changed() {
        let commits = vec![
            vec!["a.rs".to_string(), "b.rs".to_string(), "z.rs".to_string()],
            vec!["a.rs".to_string(), "b.rs".to_string()],
            vec!["a.rs".to_string(), "c.rs".to_string()],
        ];
        let changed = vec![PathBuf::from("a.rs")];
        let hints = compute_coupling(&commits, &changed, &BTreeMap::new());

        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].changed, PathBuf::from("a.rs"));
        assert_eq!(hints[0].missing, PathBuf::from("b.rs"));
        assert_eq!(hints[0].source, RelevanceSource::GitLog);
    }

    #[test]
    fn coupling_ignores_below_threshold_history_partner() {
        let commits = vec![
            vec!["a.rs".to_string(), "b.rs".to_string()],
            vec!["a.rs".to_string(), "b.rs".to_string()],
            vec!["a.rs".to_string(), "c.rs".to_string()],
            vec!["a.rs".to_string(), "d.rs".to_string()],
        ];
        let changed = vec![PathBuf::from("a.rs")];
        let hints = compute_coupling(&commits, &changed, &BTreeMap::new());

        assert!(hints.is_empty());
    }

    #[test]
    fn coupling_caps_per_file_and_keeps_high_confidence() {
        let mut commits: Vec<Vec<String>> = Vec::new();
        // hub.yml co-changes with three partners in 3 of its 5 edits (3/5).
        for _ in 0..3 {
            commits.push(vec![
                "hub.yml".to_string(),
                "a.yml".to_string(),
                "b.yml".to_string(),
                "c.yml".to_string(),
            ]);
        }
        for _ in 0..2 {
            commits.push(vec!["hub.yml".to_string()]);
        }
        // important.rs co-changes with critical.rs every time (confidence 1).
        for _ in 0..4 {
            commits.push(vec!["important.rs".to_string(), "critical.rs".to_string()]);
        }
        let changed = vec![PathBuf::from("hub.yml"), PathBuf::from("important.rs")];
        let hints = compute_coupling(&commits, &changed, &BTreeMap::new());

        // The high-confidence companion is not crowded out by the noisy hub file.
        assert!(
            hints
                .iter()
                .any(|hint| hint.changed == Path::new("important.rs")
                    && hint.missing == Path::new("critical.rs")),
            "high-confidence companion must survive the cap: {hints:?}"
        );
        // The hub file is capped per changed file.
        let hub = hints
            .iter()
            .filter(|hint| hint.changed == Path::new("hub.yml"))
            .count();
        assert!(hub <= MAX_COUPLING_PER_FILE, "hub file not capped: {hub}");
        assert!(hints.len() <= MAX_COUPLING_HINTS);
    }

    #[test]
    fn failed_history_mining_is_a_blind_spot_not_an_empty_observation() {
        // FakeRunner exits nonzero for every command, so log mining fails.
        let runner = FakeRunner::default();
        let changed = vec![PathBuf::from("src/a.rs")];
        let packet = compute_relevance(
            &runner,
            Path::new("/repo"),
            RelevanceInput {
                adapter: &GenericAdapter,
                changed: &changed,
                branch_added: &[],
                scope_source: RelevanceSource::MergeBaseDiff,
                merge_base: Some("base"),
                head_available: true,
            },
        );
        assert!(packet.coupling.is_empty());
        assert_eq!(
            packet.history_coverage.status,
            crate::coverage::CoverageStatus::Failed
        );
        assert!(
            packet
                .warnings
                .iter()
                .any(|warning| warning.contains("unavailable, not absent")),
            "expected a collection-failed warning, got {:?}",
            packet.warnings
        );
    }

    #[test]
    fn empty_history_is_a_successful_observation_with_a_shallow_warning() {
        // `git log` succeeds but reports no commits: coverage is available —
        // the emptiness is real — and the shallow-history warning still shows.
        let runner = FakeRunner::default().with(
            &[
                "log",
                "base",
                "--no-merges",
                "-n",
                "200",
                "--name-only",
                "--pretty=format:@@%H",
            ],
            "",
        );
        let changed = vec![PathBuf::from("src/a.rs")];
        let packet = compute_relevance(
            &runner,
            Path::new("/repo"),
            RelevanceInput {
                adapter: &GenericAdapter,
                changed: &changed,
                branch_added: &[],
                scope_source: RelevanceSource::MergeBaseDiff,
                merge_base: Some("base"),
                head_available: true,
            },
        );
        assert!(packet.history_coverage.is_available());
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
    fn risk_flags_high_churn_and_young_files() {
        let mut commits: Vec<Vec<String>> = (0..8).map(|_| vec!["hot.rs".to_string()]).collect();
        commits.push(vec!["calm.rs".to_string()]);
        commits.push(vec!["calm.rs".to_string()]);
        let added: BTreeSet<String> = ["new.rs".to_string()].into_iter().collect();
        let base_tracked: BTreeSet<String> = ["cold.rs".to_string()].into_iter().collect();
        let changed = vec![
            PathBuf::from("hot.rs"),
            PathBuf::from("calm.rs"),
            PathBuf::from("new.rs"),
            PathBuf::from("cold.rs"),
        ];
        let risk = compute_risk(
            &commits,
            &added,
            &base_tracked,
            &BTreeSet::new(),
            &changed,
            &BTreeMap::new(),
        );
        let summary: Vec<(&str, RiskKind)> = risk
            .iter()
            .map(|hint| (hint.path.to_str().unwrap(), hint.kind))
            .collect();
        // hot.rs = high churn; new.rs = young; cold.rs = untouched in the
        // history window; calm.rs (2 commits, not added) = neither.
        assert_eq!(
            summary,
            vec![
                ("hot.rs", RiskKind::HighChurn),
                ("new.rs", RiskKind::YoungFile),
                ("cold.rs", RiskKind::OldLoadBearing)
            ]
        );
        assert!(risk[0].action.contains("keep the change small"));
    }

    #[test]
    fn risk_prefers_churn_over_young() {
        let commits: Vec<Vec<String>> = (0..8).map(|_| vec!["both.rs".to_string()]).collect();
        let added: BTreeSet<String> = ["both.rs".to_string()].into_iter().collect();
        let risk = compute_risk(
            &commits,
            &added,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[PathBuf::from("both.rs")],
            &BTreeMap::new(),
        );
        assert_eq!(risk.len(), 1);
        assert_eq!(risk[0].kind, RiskKind::HighChurn);
    }

    #[test]
    fn young_file_without_changed_companion_test_gets_coverage_action() {
        let commits = vec![vec!["existing.rs".to_string()]];
        let added: BTreeSet<String> = ["src/foo.rs".to_string()].into_iter().collect();
        let changed = vec![PathBuf::from("src/foo.rs")];
        let companion_tests = [(
            PathBuf::from("src/foo.rs"),
            vec![PathBuf::from("tests/foo.rs")],
        )]
        .into_iter()
        .collect();

        let risk = compute_risk(
            &commits,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &added,
            &changed,
            &companion_tests,
        );

        assert_eq!(risk.len(), 1);
        assert_eq!(risk[0].kind, RiskKind::YoungFile);
        assert!(risk[0].action.contains("consider adding coverage"));
    }

    #[test]
    fn branch_added_files_are_flagged_young() {
        // Base history exists but never saw new.rs; the branch added it. It must
        // still be flagged young, even though base-history mining cannot see it.
        let runner = FakeRunner::default()
            .with(
                &[
                    "log",
                    "basesha",
                    "--no-merges",
                    "-n",
                    "200",
                    "--name-only",
                    "--pretty=format:@@%H",
                ],
                "@@h1\nexisting.rs\n",
            )
            .with(
                &[
                    "log",
                    "basesha",
                    "--no-merges",
                    "-n",
                    "200",
                    "--diff-filter=A",
                    "--name-only",
                    "--pretty=format:",
                ],
                "",
            );
        let changed = vec![PathBuf::from("new.rs"), PathBuf::from("existing.rs")];
        let branch_added = vec![PathBuf::from("new.rs")];
        let packet = compute_relevance(
            &runner,
            Path::new("/repo"),
            RelevanceInput {
                adapter: &GenericAdapter,
                changed: &changed,
                branch_added: &branch_added,
                scope_source: RelevanceSource::MergeBaseDiff,
                merge_base: Some("basesha"),
                head_available: true,
            },
        );
        assert!(
            packet
                .risk
                .iter()
                .any(|hint| hint.path == Path::new("new.rs")
                    && hint.kind == RiskKind::YoungFile
                    && hint.reason.contains("this branch")),
            "branch-added file should be young with a branch reason: {:?}",
            packet.risk
        );
        assert!(
            !packet
                .risk
                .iter()
                .any(|hint| hint.path == Path::new("existing.rs"))
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
        let order = compute_read_order(&changed, &[]);
        let reasons: Vec<&str> = order.iter().map(|entry| entry.reason.as_str()).collect();
        // packaging, docs, manifest, source, lockfile — manifest before a
        // possible dependency-related change.
        // (src/cold_scan.rs is outside ahti-query/, the bumped manifest's
        // subtree, so it is ordinary source, not a dependency-related candidate.)
        assert_eq!(
            reasons,
            vec![
                "packaging surface",
                "changed docs",
                "manifest / dependency source",
                "source files",
                "lockfile — possible dependency-related change; inspect after the manifest",
            ]
        );
        // priorities are strictly increasing.
        let priorities: Vec<u8> = order.iter().map(|entry| entry.priority).collect();
        assert!(priorities.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// The read-order group holding a path, by reason substring.
    fn group_of<'a>(order: &'a [ReadOrderEntry], path: &str) -> &'a str {
        order
            .iter()
            .find(|entry| entry.paths.iter().any(|p| p == Path::new(path)))
            .map(|entry| entry.reason.as_str())
            .unwrap_or_else(|| panic!("{path} missing from read order"))
    }

    #[test]
    fn dep_bump_ripple_is_tagged_and_ranked_last() {
        // The #12/#29 motivating shape: manifest bump + lockfile + source churn.
        let changed = vec![
            PathBuf::from("Cargo.toml"),
            PathBuf::from("Cargo.lock"),
            PathBuf::from("src/query.rs"),
            PathBuf::from("src/scan.rs"),
            PathBuf::from("README.md"),
        ];
        let order = compute_read_order(&changed, &[]);
        assert!(group_of(&order, "src/query.rs").contains("possible dependency-related change"));
        assert!(group_of(&order, "src/scan.rs").contains("possible dependency-related change"));
        // The ripple group is dead last, after even the lockfile.
        let last = order.last().unwrap();
        assert!(last.reason.contains("possible dependency-related change"));
        assert!(!last.reason.contains("skip"));
        assert_eq!(last.paths.len(), 2);
    }

    #[test]
    fn manifest_change_without_lockfile_never_tags_a_ripple() {
        // Editing Cargo.toml alone (a feature flag, metadata) is authored
        // intent; without the lockfile corroboration nothing is mechanical.
        let changed = vec![PathBuf::from("Cargo.toml"), PathBuf::from("src/query.rs")];
        let order = compute_read_order(&changed, &[]);
        assert_eq!(group_of(&order, "src/query.rs"), "source files");
    }

    #[test]
    fn lockfile_change_without_manifest_never_tags_a_ripple() {
        // A lone `cargo update` gives no governed subtree to corroborate.
        let changed = vec![PathBuf::from("Cargo.lock"), PathBuf::from("src/query.rs")];
        let order = compute_read_order(&changed, &[]);
        assert_eq!(group_of(&order, "src/query.rs"), "source files");
    }

    #[test]
    fn ripple_is_scoped_to_the_bumped_manifest_subtree() {
        let changed = vec![
            PathBuf::from("Cargo.lock"),
            PathBuf::from("crates/foo/Cargo.toml"),
            PathBuf::from("crates/foo/src/lib.rs"),
            PathBuf::from("crates/bar/src/lib.rs"),
        ];
        let order = compute_read_order(&changed, &[]);
        assert!(
            group_of(&order, "crates/foo/src/lib.rs")
                .contains("possible dependency-related change")
        );
        assert_eq!(group_of(&order, "crates/bar/src/lib.rs"), "source files");
    }

    #[test]
    fn branch_added_files_are_never_a_ripple() {
        // Adding a dependency for new feature code is the classic false
        // positive: manifest + lockfile + new source. New files are authored.
        let changed = vec![
            PathBuf::from("Cargo.toml"),
            PathBuf::from("Cargo.lock"),
            PathBuf::from("src/new_feature.rs"),
            PathBuf::from("src/query.rs"),
        ];
        let branch_added = vec![PathBuf::from("src/new_feature.rs")];
        let order = compute_read_order(&changed, &branch_added);
        assert_eq!(group_of(&order, "src/new_feature.rs"), "source files");
        assert!(group_of(&order, "src/query.rs").contains("possible dependency-related change"));
    }

    #[test]
    fn cross_ecosystem_lockfile_does_not_corroborate() {
        // A changed mix.lock must not make a Cargo.toml edit look like a bump.
        let changed = vec![
            PathBuf::from("mix.lock"),
            PathBuf::from("Cargo.toml"),
            PathBuf::from("src/query.rs"),
        ];
        let order = compute_read_order(&changed, &[]);
        assert_eq!(group_of(&order, "src/query.rs"), "source files");
    }

    #[test]
    fn tests_and_docs_are_never_tagged_as_ripple() {
        // Only Source-category churn is eligible: tests and docs keep their
        // groups even inside a bumped subtree.
        let changed = vec![
            PathBuf::from("Cargo.toml"),
            PathBuf::from("Cargo.lock"),
            PathBuf::from("tests/query.rs"),
            PathBuf::from("docs/query.md"),
        ];
        let order = compute_read_order(&changed, &[]);
        assert_eq!(group_of(&order, "tests/query.rs"), "tests");
        assert_eq!(group_of(&order, "docs/query.md"), "changed docs");
    }
}
