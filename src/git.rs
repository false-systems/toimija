use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::command::CommandRunner;
use crate::coverage::Coverage;
use crate::error::ToimijaError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitState {
    #[serde(with = "crate::path_codec::one")]
    pub repo_root: PathBuf,
    #[serde(default, with = "crate::path_codec::option")]
    pub git_dir: Option<PathBuf>,
    pub branch: Option<String>,
    pub head_oid: Option<String>,
    pub upstream: Option<String>,
    /// Commits on HEAD that `ahead_behind_ref` lacks, and the reverse. Counted
    /// from local refs only, never fetched, so they are as fresh as the last
    /// fetch. `None` when there is nothing to count against, and in a shallow
    /// repository, where Git cannot count past the cut; `ahead_behind_ref`
    /// then still names the ref.
    #[serde(default)]
    pub ahead: Option<u32>,
    #[serde(default)]
    pub behind: Option<u32>,
    /// What `ahead` and `behind` are counted against: the upstream, or, with
    /// none (or one that is gone), the remote default branch: `origin/<default>`,
    /// or that of the one remote there is when none is named `origin`.
    #[serde(default)]
    pub ahead_behind_ref: Option<String>,
    pub base: Option<String>,
    pub merge_base: Option<String>,
    pub detached: bool,
    pub dirty: bool,
    pub staged_files: Vec<ChangedFile>,
    pub unstaged_files: Vec<ChangedFile>,
    #[serde(with = "crate::path_codec::vec")]
    pub untracked_files: Vec<PathBuf>,
    #[serde(with = "crate::path_codec::vec")]
    pub conflicted_files: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_entries: Vec<ConflictEntry>,
    /// Files changed by commits on this branch since `merge_base`
    /// (`git diff --name-status <merge_base> HEAD`). Empty when there is no
    /// merge-base or the branch adds no commits beyond it — but only
    /// `branch_diff_coverage` says whether an empty list is a real observation
    /// or a blind spot.
    pub branch_files: Vec<ChangedFile>,
    /// Coverage of the merge-base diff behind `branch_files`:
    /// `available` means the (possibly empty) list is trustworthy; `degraded`
    /// means the diff could not apply (no merge-base); `failed` means the diff
    /// command itself failed and the list is unknown, not empty.
    #[serde(default = "Coverage::unrecorded")]
    pub branch_diff_coverage: Coverage,
    pub recent_commits: Vec<RecentCommit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFile {
    #[serde(with = "crate::path_codec::one")]
    pub path: PathBuf,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::path_codec::option"
    )]
    pub original_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity: Option<u8>,
    pub status: ChangeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_entry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictEntry {
    #[serde(with = "crate::path_codec::one")]
    pub path: PathBuf,
    pub stages: Vec<ConflictStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictStage {
    pub stage: u8,
    pub mode: String,
    pub oid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Unknown(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentCommit {
    pub oid: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedStatus {
    /// `# branch.oid`: the commit HEAD names, `None` for `(initial)` (unborn).
    pub head_oid: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    /// `# branch.ab +<ahead> -<behind>`: present only when the upstream exists.
    pub ahead_behind: Option<(u32, u32)>,
    pub detached: bool,
    pub staged_files: Vec<ChangedFile>,
    pub unstaged_files: Vec<ChangedFile>,
    pub untracked_files: Vec<PathBuf>,
    pub conflicted_files: Vec<PathBuf>,
    pub conflict_entries: Vec<ConflictEntry>,
}

/// The worktree as Git names it, found once per command ([`discover`]) and
/// passed down, so no reader asks for it again. A field is `None` when the
/// discovery did not establish it; the reader that needs it then asks Git
/// itself, as it did before discovery carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// `git rev-parse --show-toplevel`.
    pub root: PathBuf,
    /// `git rev-parse --git-dir` as Git answers it at the root: `.git` in a
    /// plain checkout, absolute in a linked worktree. The packet carries this
    /// spelling.
    pub git_dir: Option<PathBuf>,
    /// `git rev-parse --absolute-git-dir`: where a scratch index is written.
    pub absolute_git_dir: Option<PathBuf>,
    /// `git rev-parse --git-common-dir`, joined to the root: the refs and the
    /// config every worktree shares.
    pub common_dir: Option<PathBuf>,
    /// `git rev-parse --is-shallow-repository`, read with the discovery.
    pub shallow: Option<bool>,
}

/// One `git rev-parse` for everything a command needs to know about where it
/// is. `--show-toplevel` comes first: where it fails (outside a repository, a
/// bare one, inside `.git`), the whole call fails, as `--show-toplevel` alone
/// would. `--show-prefix` says whether Git ran at the root, which is where the
/// relative answers (`--git-dir`, `--git-common-dir`) are wanted.
const DISCOVERY: [&str; 7] = [
    "rev-parse",
    "--show-toplevel",
    "--show-prefix",
    "--git-dir",
    "--absolute-git-dir",
    "--git-common-dir",
    "--is-shallow-repository",
];

impl Worktree {
    /// Only the root is known; every other fact is read where it is needed.
    pub fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            git_dir: None,
            absolute_git_dir: None,
            common_dir: None,
            shallow: None,
        }
    }
}

/// Find the worktree at `cwd` with one git process (`DISCOVERY`); from a
/// subdirectory, a second one at the root, so the relative answers are the
/// root's. A path Git prints with a newline in it cannot be split from its
/// neighbours: then only the root is read, as [`discover_repo_root`] reads it,
/// and the rest is left to the readers that need it.
pub fn discover(runner: &impl CommandRunner, cwd: &Path) -> Result<Worktree, ToimijaError> {
    let output = runner
        .run(cwd, "git", &DISCOVERY)
        .map_err(|_| ToimijaError::CommandNotFound("git".to_string()))?;
    if output.status != 0 {
        return Err(ToimijaError::NotGitRepo);
    }
    match parse_discovery(&output.stdout) {
        Some((worktree, true)) => Ok(worktree),
        Some((from_below, false)) => {
            let at_root = runner
                .run(&from_below.root, "git", &DISCOVERY)
                .ok()
                .filter(|output| output.status == 0)
                .and_then(|output| parse_discovery(&output.stdout));
            Ok(match at_root {
                Some((worktree, true)) if worktree.root == from_below.root => worktree,
                // The root did not answer as the root: keep what holds from
                // below, and leave the relative answers to their readers.
                _ => Worktree {
                    git_dir: None,
                    common_dir: None,
                    ..from_below
                },
            })
        }
        None => discover_repo_root(runner, cwd).map(Worktree::at_root),
    }
}

/// The worktree [`DISCOVERY`] printed, and whether Git ran at its root. `None`
/// when the answer is not one line per question.
fn parse_discovery(stdout: &[u8]) -> Option<(Worktree, bool)> {
    let body = stdout.strip_suffix(b"\n")?;
    let lines: Vec<&[u8]> = body
        .split(|byte| *byte == b'\n')
        .map(trim_ascii_end)
        .collect();
    let [root, prefix, git_dir, absolute_git_dir, common_dir, shallow] = lines.as_slice() else {
        return None;
    };
    let shallow = match *shallow {
        b"true" => true,
        b"false" => false,
        _ => return None,
    };
    let root = crate::path_codec::from_git_bytes(root);
    let absolute_git_dir = crate::path_codec::from_git_bytes(absolute_git_dir);
    if !root.is_absolute() || !absolute_git_dir.is_absolute() {
        return None;
    }
    let at_root = prefix.is_empty();
    let worktree = Worktree {
        git_dir: Some(crate::path_codec::from_git_bytes(git_dir)),
        absolute_git_dir: Some(absolute_git_dir),
        // Relative to the directory git ran in; `join` keeps an absolute one.
        common_dir: Some(root.join(crate::path_codec::from_git_bytes(common_dir))),
        shallow: Some(shallow),
        root,
    };
    Some((worktree, at_root))
}

/// The repository root alone: one `git rev-parse --show-toplevel`. For the
/// commands that need nothing else; the rest discover ([`discover`]).
pub fn discover_repo_root(
    runner: &impl CommandRunner,
    cwd: &Path,
) -> Result<PathBuf, ToimijaError> {
    let output = runner
        .run(cwd, "git", &["rev-parse", "--show-toplevel"])
        .map_err(|_| ToimijaError::CommandNotFound("git".to_string()))?;
    if output.status != 0 {
        return Err(ToimijaError::NotGitRepo);
    }
    Ok(crate::path_codec::from_git_bytes(trim_ascii_end(
        &output.stdout,
    )))
}

/// One `git status` in porcelain v2 with branch headers: every changed file,
/// and HEAD and the branch in `# branch.oid` / `# branch.head`.
fn read_status(
    runner: &impl CommandRunner,
    repo_root: &Path,
) -> Result<(ParsedStatus, u128), ToimijaError> {
    let status_started = Instant::now();
    // A read must not take Git's optional index-refresh lock: another
    // process (an agent's own git) may hold it.
    let status_output = run_git_required_bytes(
        runner,
        repo_root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "-z",
        ],
    )?;
    let status_duration_ms = status_started.elapsed().as_millis();
    Ok((parse_porcelain_v2_bytes(&status_output), status_duration_ms))
}

fn read_git_dir(runner: &impl CommandRunner, repo_root: &Path) -> Option<PathBuf> {
    run_git_ok_bytes(runner, repo_root, &["rev-parse", "--git-dir"])
        .map(|bytes| crate::path_codec::from_git_bytes(trim_ascii_end(&bytes)))
}

/// The worktree's git dir as the packet spells it: the discovery's, or asked.
fn git_dir_of(runner: &impl CommandRunner, worktree: &Worktree) -> Option<PathBuf> {
    worktree
        .git_dir
        .clone()
        .or_else(|| read_git_dir(runner, &worktree.root))
}

fn is_dirty(parsed: &ParsedStatus) -> bool {
    has_work(
        &parsed.staged_files,
        &parsed.unstaged_files,
        &parsed.untracked_files,
        &parsed.conflicted_files,
    )
}

/// Whether any listed path is the repository's own work: one outside the
/// folders of generated tool state (`.toimija/` and `.sykli/`;
/// `.toimija/repo.yaml` is work). So a session marker, a handover or a gate's
/// receipt never makes a state dirty, whatever the repository's ignore files
/// say. The lists themselves stay what `git status` reported.
pub(crate) fn has_work(
    staged: &[ChangedFile],
    unstaged: &[ChangedFile],
    untracked: &[PathBuf],
    conflicted: &[PathBuf],
) -> bool {
    work_files(staged)
        .chain(work_files(unstaged))
        .next()
        .is_some()
        || work_paths(untracked)
            .chain(work_paths(conflicted))
            .next()
            .is_some()
}

/// The folders at the repository root that hold generated tool state:
/// Toimija's own, and sykli's receipts.
const GENERATED: [&str; 2] = [".toimija", ".sykli"];

/// Configuration inside a generated folder, which is the repository's work.
const CONFIGURATION: &str = crate::config::CONFIG_PATH;

/// Whether `path` (relative to the repository root) is the repository's own
/// work rather than generated tool state: Toimija's own files under
/// `.toimija/`, except its configuration, and `.sykli/`.
pub(crate) fn is_work(path: &Path) -> bool {
    let Some(std::path::Component::Normal(first)) = path.components().next() else {
        return true;
    };
    !GENERATED.iter().any(|folder| first == *folder) || path == Path::new(CONFIGURATION)
}

/// The files of `files` that are the repository's own work ([`is_work`]).
pub(crate) fn work_files(files: &[ChangedFile]) -> impl Iterator<Item = &ChangedFile> {
    files.iter().filter(|file| is_work(&file.path))
}

/// The paths of `paths` that are the repository's own work ([`is_work`]).
pub(crate) fn work_paths(paths: &[PathBuf]) -> impl Iterator<Item = &PathBuf> {
    paths.iter().filter(|path| is_work(path))
}

/// One read of `git status` for a worktree: what [`complete`] builds the whole
/// Git state from without reading it again.
#[derive(Debug, Clone)]
pub struct Status {
    repo_root: PathBuf,
    git_dir: Option<PathBuf>,
    parsed: ParsedStatus,
    duration_ms: u128,
}

/// Read `status` once for `worktree` (and the git dir, when the discovery did
/// not establish it).
pub fn read_status_of(
    runner: &impl CommandRunner,
    worktree: &Worktree,
) -> Result<Status, ToimijaError> {
    let (parsed, duration_ms) = read_status(runner, &worktree.root)?;
    Ok(Status {
        repo_root: worktree.root.clone(),
        git_dir: git_dir_of(runner, worktree),
        parsed,
        duration_ms,
    })
}

fn state_from_status(repo_root: &Path, git_dir: Option<PathBuf>, parsed: ParsedStatus) -> GitState {
    let dirty = is_dirty(&parsed);
    let detached = parsed.detached || (parsed.branch.is_none() && parsed.head_oid.is_some());
    GitState {
        repo_root: repo_root.to_path_buf(),
        git_dir,
        branch: parsed.branch,
        head_oid: parsed.head_oid,
        upstream: parsed.upstream,
        ahead: None,
        behind: None,
        ahead_behind_ref: None,
        base: None,
        merge_base: None,
        detached,
        dirty,
        staged_files: parsed.staged_files,
        unstaged_files: parsed.unstaged_files,
        untracked_files: parsed.untracked_files,
        conflicted_files: parsed.conflicted_files,
        conflict_entries: parsed.conflict_entries,
        branch_files: Vec::new(),
        branch_diff_coverage: Coverage::unrecorded(),
        recent_commits: Vec::new(),
    }
}

/// What one `git status` tells of `worktree`: HEAD, the branch and the working
/// tree, and nothing read from refs or history. One git process: what `run`
/// reads when its agent exits.
pub fn status_state(
    runner: &impl CommandRunner,
    worktree: &Worktree,
) -> Result<GitState, ToimijaError> {
    let (parsed, _duration) = read_status(runner, &worktree.root)?;
    Ok(state_from_status(
        &worktree.root,
        worktree.git_dir.clone(),
        parsed,
    ))
}

/// One collection of a worktree's Git state: the state, the warnings the
/// collection raised.
#[derive(Debug, Clone)]
pub struct Collected {
    pub state: GitState,
    pub warnings: Vec<String>,
}

/// What a collection reads from refs and history rather than from `status`:
/// the base, the merge-base and the branch diff from it, the recent commits,
/// ahead/behind counted against the base, and whether the repository is
/// shallow.
#[derive(Debug, Clone)]
struct RefFacts {
    base: Option<String>,
    merge_base: Option<String>,
    branch_files: Vec<ChangedFile>,
    branch_diff_coverage: Coverage,
    recent_commits: Vec<RecentCommit>,
    /// Ahead/behind counted against the base, when `status` counted none.
    base_counted: Option<((u32, u32), String)>,
    shallow: bool,
}

fn upstream_counted(parsed: &ParsedStatus) -> bool {
    parsed.ahead_behind.is_some() && parsed.upstream.is_some()
}

/// Discover the worktree at `cwd` and collect its whole Git state: what the
/// tests read a repository with.
#[cfg(test)]
pub fn collect_git_state(
    runner: &impl CommandRunner,
    cwd: &Path,
) -> Result<(GitState, Vec<String>), ToimijaError> {
    let collected = collect(runner, &discover(runner, cwd)?)?;
    Ok((collected.state, collected.warnings))
}

/// The whole Git state of `worktree`: one `status`, then the refs and history
/// the packet shows (`read_ref_facts`).
pub fn collect(
    runner: &impl CommandRunner,
    worktree: &Worktree,
) -> Result<Collected, ToimijaError> {
    Ok(complete(
        runner,
        worktree,
        read_status_of(runner, worktree)?,
    ))
}

/// The whole Git state from a `status` already read: the refs and history
/// are read now, `status` is not read again.
pub fn complete(runner: &impl CommandRunner, worktree: &Worktree, status: Status) -> Collected {
    let refs = read_ref_facts(runner, &status.repo_root, &status.parsed, worktree.shallow);
    assemble(status, refs)
}

/// Read the refs and history the packet shows, for HEAD and the upstream as
/// `parsed` names them: the base, the merge-base with it, the branch diff
/// from there, the last ten commits, ahead/behind against the base when no
/// upstream counts them, and — unless the discovery just read it — whether the
/// repository is shallow.
fn read_ref_facts(
    runner: &impl CommandRunner,
    repo_root: &Path,
    parsed: &ParsedStatus,
    shallow: Option<bool>,
) -> RefFacts {
    let head_oid = parsed.head_oid.as_deref();
    let discovered = discover_base(runner, repo_root);
    let base = discovered.as_ref().map(|base| base.name.clone());
    let merge_base = if head_oid.is_some() {
        base.as_deref().and_then(|base| {
            run_git_ok(runner, repo_root, &["merge-base", "HEAD", base])
                .map(|text| text.trim().to_string())
        })
    } else {
        None
    };
    if merge_base.is_none() {
        debug!(
            event = "toimija.git.merge_base.unavailable",
            base = base.as_deref().unwrap_or("none"),
            head_available = head_oid.is_some()
        );
    }
    let (branch_files, branch_diff_coverage) = match (merge_base.as_deref(), head_oid) {
        (Some(base_oid), Some(head)) if base_oid != head => {
            match run_git_observed(
                runner,
                repo_root,
                &["diff", "--name-status", "-z", base_oid, head],
            ) {
                Ok(bytes) => (
                    parse_name_status_bytes(&bytes),
                    Coverage::available(BRANCH_DIFF_SOURCE),
                ),
                Err(reason) => (Vec::new(), Coverage::failed(BRANCH_DIFF_SOURCE, reason)),
            }
        }
        // Merge-base equals HEAD: the branch adds no commits, so an empty list
        // is a real observation, not a skipped one.
        (Some(_), Some(_)) => (Vec::new(), Coverage::available(BRANCH_DIFF_SOURCE)),
        _ => (
            Vec::new(),
            Coverage::degraded(
                BRANCH_DIFF_SOURCE,
                "no merge-base; branch surface unavailable",
            ),
        ),
    };
    // An unborn HEAD has no history: `git log` could only fail.
    let recent_commits = head_oid
        .and_then(|_| run_git_ok(runner, repo_root, &["log", "-n", "10", "--oneline"]))
        .map(|text| parse_recent_commits(&text))
        .unwrap_or_default();
    // Ahead/behind come from Git's own `branch.ab` line when the upstream
    // exists. Without one (or with one that is gone), they are counted against
    // the remote default branch — never against a local branch, and never
    // after a fetch: toimija makes no network calls.
    let base_counted = if upstream_counted(parsed) {
        None
    } else {
        match (head_oid, discovered.filter(|base| base.remote)) {
            (Some(_), Some(Base { name, .. })) => {
                count_ahead_behind(runner, repo_root, &name).map(|counts| (counts, name))
            }
            _ => None,
        }
    };
    let shallow = shallow.unwrap_or_else(|| {
        run_git_ok(runner, repo_root, &["rev-parse", "--is-shallow-repository"])
            .is_some_and(|text| text.trim() == "true")
    });
    RefFacts {
        base,
        merge_base,
        branch_files,
        branch_diff_coverage,
        recent_commits,
        base_counted,
        shallow,
    }
}

/// The Git state from one `status` and the ref facts read for it, with the
/// warnings both raise. Starts no process.
fn assemble(status: Status, refs: RefFacts) -> Collected {
    let Status {
        repo_root,
        git_dir,
        parsed,
        duration_ms,
    } = status;
    let mut warnings = Vec::new();
    if parsed.head_oid.is_none() {
        if parsed.branch.is_some() {
            warnings.push("HEAD unavailable; branch has no commits yet.".to_string());
        } else {
            warnings.push("HEAD unavailable.".to_string());
        }
    }
    if refs.merge_base.is_none() {
        match refs.base.as_deref() {
            Some(base) => warnings.push(format!(
                "No merge-base with {base}. Branch surface is unavailable."
            )),
            None => {
                warnings.push("No base branch detected. Branch surface is unavailable.".to_string())
            }
        }
    }
    if refs.branch_diff_coverage.status == crate::coverage::CoverageStatus::Failed {
        warnings.push(
            "Branch diff collection failed. Branch surface is unknown, not empty.".to_string(),
        );
    }
    let (ahead_behind, ahead_behind_ref) = match (parsed.ahead_behind, &parsed.upstream) {
        (Some(counts), Some(upstream)) => (Some(counts), Some(upstream.clone())),
        _ => match &refs.base_counted {
            Some((counts, base)) => (Some(*counts), Some(base.clone())),
            None => (None, None),
        },
    };
    // A shallow repository has only part of its history, and Git counts only
    // what it has: a cut can hide the commits both sides share, which are
    // then counted as one side's (`+2 -1` for one commit ahead). The counts
    // are left unknown there; the ref they would be counted against is named.
    let ahead_behind = ahead_behind.filter(|_| !refs.shallow);
    if parsed.upstream.is_none() {
        warnings.push(match &ahead_behind_ref {
            Some(base) => format!("No upstream detected. Ahead/behind are counted against {base}."),
            None => "No upstream detected. Branch divergence is unavailable.".to_string(),
        });
    }
    if refs.shallow {
        warnings.push(
            "Shallow repository. Merge-base may be incomplete; ahead/behind are not counted."
                .to_string(),
        );
    }
    let detached = parsed.detached || (parsed.branch.is_none() && parsed.head_oid.is_some());
    if detached {
        warnings.push("Detached HEAD. Broad agent work is risky here.".to_string());
    }
    if !parsed.conflicted_files.is_empty() {
        warnings.push("Conflicted files exist. Resolve conflicts before continuing.".to_string());
    }
    // Toimija's own files, and the other generated namespaces, are not the
    // repository's work: in a repository that does not ignore them Git lists
    // every marker and handover, and they would warn on every packet.
    if work_files(&parsed.unstaged_files).next().is_some() {
        warnings.push("Working tree has unstaged changes.".to_string());
    }
    if work_paths(&parsed.untracked_files).next().is_some() {
        warnings
            .push("Untracked files exist. They will not be committed unless added.".to_string());
    }
    let dirty = is_dirty(&parsed);
    debug!(
        event = "toimija.git.state.collected",
        repo_root = %repo_root.display(),
        branch = parsed.branch.as_deref().unwrap_or("none"),
        detached,
        merge_base_available = refs.merge_base.is_some(),
        status_duration_ms = duration_ms
    );
    let state = GitState {
        repo_root,
        git_dir,
        branch: parsed.branch,
        head_oid: parsed.head_oid,
        upstream: parsed.upstream,
        ahead: ahead_behind.map(|(ahead, _)| ahead),
        behind: ahead_behind.map(|(_, behind)| behind),
        ahead_behind_ref,
        base: refs.base.clone(),
        merge_base: refs.merge_base.clone(),
        detached,
        dirty,
        staged_files: parsed.staged_files,
        unstaged_files: parsed.unstaged_files,
        untracked_files: parsed.untracked_files,
        conflicted_files: parsed.conflicted_files,
        conflict_entries: parsed.conflict_entries,
        branch_files: refs.branch_files,
        branch_diff_coverage: refs.branch_diff_coverage,
        recent_commits: refs.recent_commits,
    };
    Collected { state, warnings }
}

/// `(ahead, behind)` of HEAD against `other`, from local refs only.
fn count_ahead_behind(
    runner: &impl CommandRunner,
    repo_root: &Path,
    other: &str,
) -> Option<(u32, u32)> {
    let range = format!("HEAD...{other}");
    let text = run_git_ok(
        runner,
        repo_root,
        &["rev-list", "--left-right", "--count", &range, "--"],
    )?;
    let mut counts = text.split_whitespace().map(str::parse::<u32>);
    match (counts.next(), counts.next(), counts.next()) {
        (Some(Ok(ahead)), Some(Ok(behind)), None) => Some((ahead, behind)),
        _ => None,
    }
}

/// Parse the payload of porcelain v2's `# branch.ab +<ahead> -<behind>`.
fn parse_ahead_behind(text: &str) -> Option<(u32, u32)> {
    let (ahead, behind) = text.trim().split_once(' ')?;
    Some((
        ahead.strip_prefix('+')?.parse().ok()?,
        behind.strip_prefix('-')?.parse().ok()?,
    ))
}

/// The branch the branch surface compares against, and whether it is a
/// remote-tracking branch (`<remote>/<branch>`) rather than a local one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Base {
    name: String,
    remote: bool,
}

fn discover_base(runner: &impl CommandRunner, repo_root: &Path) -> Option<Base> {
    // Branch surface compares against the integration trunk, not the branch's
    // push/pull upstream. A pushed feature branch commonly tracks
    // origin/<feature>, which would make merge-base collapse to HEAD.
    if let Some(default_remote) = run_git_ok(
        runner,
        repo_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .map(|text| text.trim().to_string())
    .filter(|text| !text.is_empty())
    {
        return Some(Base {
            name: default_remote,
            remote: true,
        });
    }
    // No `origin/HEAD`: one read names every remote's `HEAD`, `main` and
    // `master`, so a remote not named `origin` is found too.
    if let Some(name) = run_git_ok(
        runner,
        repo_root,
        &[
            "for-each-ref",
            "--format=%(refname) %(symref)",
            "refs/remotes/*/HEAD",
            "refs/remotes/*/main",
            "refs/remotes/*/master",
        ],
    )
    .and_then(|listing| remote_default_branch(&listing))
    {
        return Some(Base { name, remote: true });
    }
    for candidate in ["main", "master"] {
        if run_git_ok(runner, repo_root, &["rev-parse", "--verify", candidate]).is_some() {
            return Some(Base {
                name: candidate.to_string(),
                remote: false,
            });
        }
    }
    None
}

/// The remote default branch in a `for-each-ref` listing of remotes' `HEAD`,
/// `main` and `master` (`<refname> <symref target>` per line): `origin/main`,
/// then `origin/master`; with no `origin` among them and exactly one other
/// remote, what its `HEAD` points at, then its `main`, then its `master`.
/// Toimija never picks between two remotes that are not `origin`. The `*` of
/// the listing's patterns does not cross a `/`, so a remote whose name holds
/// one is not listed.
fn remote_default_branch(listing: &str) -> Option<String> {
    let mut remotes: BTreeMap<&str, [Option<String>; 3]> = BTreeMap::new();
    for line in listing.lines() {
        let (refname, target) = line.split_once(' ').unwrap_or((line, ""));
        let Some(short) = refname.strip_prefix("refs/remotes/") else {
            continue;
        };
        for (slot, branch) in ["HEAD", "main", "master"].into_iter().enumerate() {
            let Some(remote) = short.strip_suffix(branch).and_then(|r| r.strip_suffix('/')) else {
                continue;
            };
            let name = if branch == "HEAD" {
                match target.strip_prefix("refs/remotes/") {
                    Some(target) => target.to_string(),
                    None => continue,
                }
            } else {
                short.to_string()
            };
            remotes.entry(remote).or_default()[slot] = Some(name);
        }
    }
    let [_, main, master] = remotes.get("origin").cloned().unwrap_or_default();
    if let Some(name) = main.or(master) {
        return Some(name);
    }
    if remotes.len() != 1 {
        return None;
    }
    let [head, main, master] = remotes.into_values().next()?;
    head.or(main).or(master)
}

fn run_git_required_bytes(
    runner: &impl CommandRunner,
    cwd: &Path,
    args: &[&str],
) -> Result<Vec<u8>, ToimijaError> {
    let output = runner
        .run(cwd, "git", args)
        .map_err(|_| ToimijaError::CommandNotFound("git".to_string()))?;
    if output.status != 0 {
        return Err(ToimijaError::GitCommand {
            command: format!("git {}", args.join(" ")),
            status: output.status,
        });
    }
    Ok(output.stdout)
}

/// Run a git command, returning its stdout only on success.
pub(crate) fn run_git_ok(runner: &impl CommandRunner, cwd: &Path, args: &[&str]) -> Option<String> {
    run_git_ok_bytes(runner, cwd, args).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn run_git_ok_bytes(runner: &impl CommandRunner, cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = runner.run(cwd, "git", args).ok()?;
    (output.status == 0).then_some(output.stdout)
}

/// The coverage source label for the merge-base branch diff.
const BRANCH_DIFF_SOURCE: &str = "git diff --name-status <merge-base> HEAD";

/// Run a git command for a stage whose failure affects a product claim:
/// unlike [`run_git_ok_bytes`], spawn failure and a nonzero exit come back as
/// distinct, safe diagnostics instead of collapsing into `None`.
pub(crate) fn run_git_observed(
    runner: &impl CommandRunner,
    cwd: &Path,
    args: &[&str],
) -> Result<Vec<u8>, String> {
    match runner.run(cwd, "git", args) {
        Ok(output) if output.status == 0 => Ok(output.stdout),
        Ok(output) => Err(format!("git exited with status {}", output.status)),
        Err(error) => Err(format!("git failed to spawn: {}", error.kind())),
    }
}

/// Line-delimited porcelain v2, for tests. Git collection uses
/// [`parse_porcelain_v2_bytes`] so paths remain lossless.
#[cfg(test)]
fn parse_porcelain_v2(text: &str) -> ParsedStatus {
    let nul = text.replace(['\n', '\t'], "\0");
    parse_porcelain_v2_bytes(nul.as_bytes())
}

pub(crate) fn parse_porcelain_v2_bytes(bytes: &[u8]) -> ParsedStatus {
    let mut parsed = ParsedStatus::default();
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if let Some(rest) = record.strip_prefix(b"# branch.oid ") {
            let rest = String::from_utf8_lossy(rest);
            if rest != "(initial)" {
                parsed.head_oid = Some(rest.into_owned());
            }
            continue;
        }
        if let Some(rest) = record.strip_prefix(b"# branch.head ") {
            let rest = String::from_utf8_lossy(rest);
            if rest == "(detached)" {
                parsed.detached = true;
            } else {
                parsed.branch = Some(rest.into_owned());
            }
            continue;
        }
        if let Some(rest) = record.strip_prefix(b"# branch.upstream ") {
            parsed.upstream = Some(String::from_utf8_lossy(rest).into_owned());
            continue;
        }
        if let Some(rest) = record.strip_prefix(b"# branch.ab ") {
            parsed.ahead_behind = parse_ahead_behind(&String::from_utf8_lossy(rest));
            continue;
        }
        if let Some(rest) = record.strip_prefix(b"? ") {
            parsed
                .untracked_files
                .push(crate::path_codec::from_git_bytes(rest));
            continue;
        }
        if record.starts_with(b"u ") {
            let parts = splitn_bytes(record, 11);
            if let Some(path) = parts.get(10) {
                let path = crate::path_codec::from_git_bytes(path);
                parsed.conflicted_files.push(path.clone());
                if parts.len() == 11 {
                    parsed.conflict_entries.push(ConflictEntry {
                        path,
                        stages: (0..3)
                            .map(|index| ConflictStage {
                                stage: index as u8 + 1,
                                mode: String::from_utf8_lossy(parts[index + 3]).into_owned(),
                                oid: String::from_utf8_lossy(parts[index + 7]).into_owned(),
                            })
                            .collect(),
                    });
                }
            }
            continue;
        }
        if record.starts_with(b"1 ") {
            parse_changed_record(record, None, &mut parsed);
        } else if record.starts_with(b"2 ") {
            parse_changed_record(record, records.next(), &mut parsed);
        }
    }
    dedupe(&mut parsed);
    parsed
}

fn parse_changed_record(record: &[u8], original: Option<&[u8]>, parsed: &mut ParsedStatus) {
    let (xy, index_entry, path, similarity) = if record.starts_with(b"2 ") {
        let parts = splitn_bytes(record, 10);
        if parts.len() < 10 {
            return;
        }
        (
            parts[1],
            Some(format!(
                "{} {}",
                String::from_utf8_lossy(parts[4]),
                String::from_utf8_lossy(parts[7])
            )),
            crate::path_codec::from_git_bytes(parts[9]),
            similarity_score(parts[8]),
        )
    } else {
        let parts = splitn_bytes(record, 9);
        if parts.len() < 9 {
            return;
        }
        (
            parts[1],
            Some(format!(
                "{} {}",
                String::from_utf8_lossy(parts[4]),
                String::from_utf8_lossy(parts[7])
            )),
            crate::path_codec::from_git_bytes(parts[8]),
            None,
        )
    };
    let staged = xy.first().copied().unwrap_or(b'.') as char;
    let unstaged = xy.get(1).copied().unwrap_or(b'.') as char;
    let original_path = original.map(crate::path_codec::from_git_bytes);
    if staged != '.' {
        parsed.staged_files.push(ChangedFile {
            path: path.clone(),
            status: status_from_char(staged),
            index_entry: index_entry.clone(),
            original_path: original_path.clone(),
            similarity,
        });
    }
    if unstaged != '.' {
        parsed.unstaged_files.push(ChangedFile {
            path,
            status: status_from_char(unstaged),
            index_entry: None,
            original_path,
            similarity,
        });
    }
}

fn status_from_char(value: char) -> ChangeStatus {
    match value {
        'A' => ChangeStatus::Added,
        'M' => ChangeStatus::Modified,
        'D' => ChangeStatus::Deleted,
        'R' => ChangeStatus::Renamed,
        'C' => ChangeStatus::Copied,
        '?' => ChangeStatus::Untracked,
        other => ChangeStatus::Unknown(other.to_string()),
    }
}

/// Parse `git diff --name-status` output. Each line is a status column
/// followed by tab-separated path(s); renames and copies (`R100`, `C075`)
/// carry an old and a new path, and we keep the new one.
/// Line-delimited name-status, for tests. Git collection uses
/// [`parse_name_status_bytes`] so paths remain lossless.
#[cfg(test)]
fn parse_name_status(text: &str) -> Vec<ChangedFile> {
    let nul = text.replace(['\n', '\t'], "\0");
    parse_name_status_bytes(nul.as_bytes())
}

pub(crate) fn parse_name_status_bytes(bytes: &[u8]) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    while let Some(status) = fields.next() {
        let Some(code) = status.first().copied().map(char::from) else {
            continue;
        };
        let first = match fields.next() {
            Some(path) => path,
            None => break,
        };
        let (original_path, path) = if matches!(code, 'R' | 'C') {
            let Some(new_path) = fields.next() else { break };
            (Some(crate::path_codec::from_git_bytes(first)), new_path)
        } else {
            (None, first)
        };
        files.push(ChangedFile {
            path: crate::path_codec::from_git_bytes(path),
            status: status_from_char(code),
            index_entry: None,
            original_path,
            similarity: similarity_score(status),
        });
    }
    files
}

fn splitn_bytes(bytes: &[u8], count: usize) -> Vec<&[u8]> {
    bytes.splitn(count, |byte| *byte == b' ').collect()
}

fn similarity_score(status: &[u8]) -> Option<u8> {
    matches!(status.first(), Some(b'R' | b'C'))
        .then(|| std::str::from_utf8(status.get(1..)?).ok()?.parse().ok())
        .flatten()
}

fn trim_ascii_end(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\n' | b'\r' | 0)) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn parse_recent_commits(text: &str) -> Vec<RecentCommit> {
    text.lines()
        .filter_map(|line| {
            let (oid, summary) = line.split_once(' ')?;
            Some(RecentCommit {
                oid: oid.to_string(),
                summary: summary.to_string(),
            })
        })
        .collect()
}

fn dedupe(parsed: &mut ParsedStatus) {
    parsed.untracked_files = dedupe_paths(&parsed.untracked_files);
    parsed.conflicted_files = dedupe_paths(&parsed.conflicted_files);
}

fn dedupe_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    paths
        .iter()
        .filter(|path| seen.insert((*path).clone()))
        .cloned()
        .collect()
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
        fn with(mut self, args: &[&str], status: i32, stdout: &str) -> Self {
            self.outputs.insert(
                args.iter().map(|arg| arg.to_string()).collect(),
                CommandOutput {
                    status,
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

    #[test]
    fn parses_branch_and_changes() {
        let text = "\
# branch.oid abc
# branch.head main
# branch.upstream origin/main
1 M. N... 100644 100644 100644 a b src/lib.rs
1 .M N... 100644 100644 100644 a b README.md
? notes.txt
";
        let parsed = parse_porcelain_v2(text);
        assert_eq!(parsed.branch.as_deref(), Some("main"));
        assert_eq!(parsed.upstream.as_deref(), Some("origin/main"));
        assert_eq!(parsed.staged_files[0].path, PathBuf::from("src/lib.rs"));
        assert_eq!(
            parsed.staged_files[0].index_entry.as_deref(),
            Some("100644 b")
        );
        assert_eq!(parsed.unstaged_files[0].path, PathBuf::from("README.md"));
        assert_eq!(parsed.unstaged_files[0].index_entry, None);
        assert_eq!(parsed.untracked_files[0], PathBuf::from("notes.txt"));
    }

    #[test]
    fn parses_ahead_and_behind_the_upstream() {
        let text = "\
# branch.oid abc
# branch.head feature
# branch.upstream origin/feature
# branch.ab +2 -3
";
        let parsed = parse_porcelain_v2(text);
        assert_eq!(parsed.ahead_behind, Some((2, 3)));

        // An upstream that is configured but gone has no `branch.ab` line.
        let gone = parse_porcelain_v2("# branch.head feature\n# branch.upstream origin/gone\n");
        assert_eq!(gone.ahead_behind, None);
    }

    #[test]
    fn parses_detached_and_conflict() {
        let text = "\
# branch.head (detached)
u UU N... 100644 100644 100644 100644 a b c src/lib.rs
";
        let parsed = parse_porcelain_v2(text);
        assert!(parsed.detached);
        assert_eq!(parsed.conflicted_files[0], PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn parses_rename_and_copy_paths_from_porcelain_v2() {
        let text = "\
2 R. N... 100644 100644 100644 a b R100 src/new name.rs\tsrc/old name.rs
2 C. N... 100644 100644 100644 a b C75 src/copied.rs\tsrc/source.rs
";
        let parsed = parse_porcelain_v2(text);
        assert_eq!(parsed.staged_files[0].status, ChangeStatus::Renamed);
        assert_eq!(
            parsed.staged_files[0].path,
            PathBuf::from("src/new name.rs")
        );
        assert_eq!(
            parsed.staged_files[0].original_path.as_deref(),
            Some(Path::new("src/old name.rs"))
        );
        assert_eq!(parsed.staged_files[0].similarity, Some(100));
        assert_eq!(parsed.staged_files[1].status, ChangeStatus::Copied);
        assert_eq!(parsed.staged_files[1].path, PathBuf::from("src/copied.rs"));
        assert_eq!(
            parsed.staged_files[1].original_path.as_deref(),
            Some(Path::new("src/source.rs"))
        );
        assert_eq!(parsed.staged_files[1].similarity, Some(75));
    }

    #[test]
    fn nul_porcelain_preserves_control_bytes_and_conflict_spaces() {
        let bytes = b"2 R. N... 100644 100644 100644 a b R87 new\tname.rs\0old\nname.rs\0u UU N... 100644 100644 100644 100644 a b c conflict path.rs\0? line\nname.txt\0";
        let parsed = parse_porcelain_v2_bytes(bytes);

        assert_eq!(parsed.staged_files[0].path, PathBuf::from("new\tname.rs"));
        assert_eq!(
            parsed.staged_files[0].original_path.as_deref(),
            Some(Path::new("old\nname.rs"))
        );
        assert_eq!(parsed.staged_files[0].similarity, Some(87));
        assert_eq!(
            parsed.conflicted_files[0],
            PathBuf::from("conflict path.rs")
        );
        assert_eq!(
            parsed.conflict_entries[0].path,
            PathBuf::from("conflict path.rs")
        );
        assert_eq!(parsed.conflict_entries[0].stages[0].stage, 1);
        assert_eq!(parsed.conflict_entries[0].stages[0].mode, "100644");
        assert_eq!(parsed.conflict_entries[0].stages[0].oid, "a");
        assert_eq!(parsed.conflict_entries[0].stages[2].stage, 3);
        assert_eq!(parsed.conflict_entries[0].stages[2].oid, "c");
        assert_eq!(parsed.untracked_files[0], PathBuf::from("line\nname.txt"));
    }

    #[test]
    fn nul_name_status_preserves_rename_and_copy_origins() {
        let files =
            parse_name_status_bytes(b"R094\0old\nname.rs\0new\tname.rs\0C75\0source.rs\0copy.rs\0");

        assert_eq!(files[0].status, ChangeStatus::Renamed);
        assert_eq!(files[0].path, PathBuf::from("new\tname.rs"));
        assert_eq!(files[0].original_path, Some(PathBuf::from("old\nname.rs")));
        assert_eq!(files[0].similarity, Some(94));
        assert_eq!(files[1].status, ChangeStatus::Copied);
        assert_eq!(files[1].original_path, Some(PathBuf::from("source.rs")));
        assert_eq!(files[1].similarity, Some(75));
    }

    #[test]
    fn name_status_keeps_new_path_for_renames() {
        let text = "\
M\tsrc/lib.rs
A\tsrc/new.rs
D\tsrc/gone.rs
R094\tsrc/old.rs\tsrc/renamed.rs
";
        let files = parse_name_status(text);
        assert_eq!(
            files,
            vec![
                ChangedFile {
                    path: PathBuf::from("src/lib.rs"),
                    status: ChangeStatus::Modified,
                    index_entry: None,
                    original_path: None,
                    similarity: None,
                },
                ChangedFile {
                    path: PathBuf::from("src/new.rs"),
                    status: ChangeStatus::Added,
                    index_entry: None,
                    original_path: None,
                    similarity: None,
                },
                ChangedFile {
                    path: PathBuf::from("src/gone.rs"),
                    status: ChangeStatus::Deleted,
                    index_entry: None,
                    original_path: None,
                    similarity: None,
                },
                ChangedFile {
                    path: PathBuf::from("src/renamed.rs"),
                    status: ChangeStatus::Renamed,
                    index_entry: None,
                    original_path: Some(PathBuf::from("src/old.rs")),
                    similarity: Some(94),
                },
            ]
        );
    }

    /// A runner whose spawn always fails, for injecting the "git missing"
    /// failure mode into coverage tests.
    struct SpawnFailRunner;

    impl CommandRunner for SpawnFailRunner {
        fn run(
            &self,
            _cwd: &Path,
            _program: &str,
            _args: &[&str],
        ) -> std::io::Result<CommandOutput> {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        }
    }

    #[test]
    fn run_git_observed_distinguishes_exit_and_spawn_failure() {
        // FakeRunner answers unknown args with exit status 1.
        let exit = run_git_observed(&FakeRunner::default(), Path::new("."), &["status"]);
        assert_eq!(exit, Err("git exited with status 1".to_string()));

        let spawn = run_git_observed(&SpawnFailRunner, Path::new("."), &["status"]);
        assert_eq!(
            spawn,
            Err("git failed to spawn: entity not found".to_string())
        );
    }

    /// What [`DISCOVERY`] prints at the root of a plain checkout at `/repo`.
    const DISCOVERED: &str = "/repo\n\n.git\n/repo/.git\n.git\nfalse\n";

    const STATUS: [&str; 6] = [
        "--no-optional-locks",
        "status",
        "--porcelain=v2",
        "--branch",
        "--untracked-files=all",
        "-z",
    ];

    /// A runner primed with everything `collect_git_state` needs up to the
    /// branch diff, so tests can vary only the diff outcome.
    fn runner_with_merge_base() -> FakeRunner {
        FakeRunner::default()
            .with(&DISCOVERY, 0, DISCOVERED)
            .with(&STATUS, 0, "# branch.oid headoid\0# branch.head feat/x\0")
            .with(
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
                0,
                "origin/main\n",
            )
            .with(&["merge-base", "HEAD", "origin/main"], 0, "baseoid\n")
    }

    #[test]
    fn empty_branch_diff_is_an_available_observation() {
        let runner = runner_with_merge_base().with(
            &["diff", "--name-status", "-z", "baseoid", "headoid"],
            0,
            "",
        );
        let (state, warnings) = collect_git_state(&runner, Path::new("/repo")).unwrap();
        assert!(state.branch_files.is_empty());
        assert!(state.branch_diff_coverage.is_available());
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("Branch diff collection failed"))
        );
    }

    #[test]
    fn failed_branch_diff_is_a_blind_spot_not_an_empty_observation() {
        // No diff output primed: FakeRunner answers it with exit status 1.
        let runner = runner_with_merge_base();
        let (state, warnings) = collect_git_state(&runner, Path::new("/repo")).unwrap();
        assert!(state.branch_files.is_empty());
        assert_eq!(
            state.branch_diff_coverage.status,
            crate::coverage::CoverageStatus::Failed
        );
        assert_eq!(
            state.branch_diff_coverage.reason.as_deref(),
            Some("git exited with status 1")
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("Branch surface is unknown, not empty")),
            "expected a branch-diff failure warning, got {warnings:?}"
        );
    }

    #[test]
    fn missing_merge_base_degrades_branch_diff_coverage() {
        // No base branch resolvable at all: coverage says why the surface is
        // unavailable instead of presenting an empty list as an observation.
        let runner = FakeRunner::default().with(&DISCOVERY, 0, DISCOVERED).with(
            &STATUS,
            0,
            "# branch.oid headoid\0# branch.head feat/x\0",
        );
        let (state, _warnings) = collect_git_state(&runner, Path::new("/repo")).unwrap();
        assert_eq!(
            state.branch_diff_coverage.status,
            crate::coverage::CoverageStatus::Degraded
        );
        assert_eq!(
            state.branch_diff_coverage.reason.as_deref(),
            Some("no merge-base; branch surface unavailable")
        );
    }

    const REMOTE_DEFAULTS: [&str; 5] = [
        "for-each-ref",
        "--format=%(refname) %(symref)",
        "refs/remotes/*/HEAD",
        "refs/remotes/*/main",
        "refs/remotes/*/master",
    ];

    fn remote(name: &str) -> Option<Base> {
        Some(Base {
            name: name.to_string(),
            remote: true,
        })
    }

    #[test]
    fn base_discovery_ignores_feature_upstream_and_prefers_trunk() {
        // Without `origin/HEAD`, `origin/main` is the trunk; a feature branch
        // the upstream tracks is not listed, so it cannot be picked.
        let runner = FakeRunner::default().with(
            &REMOTE_DEFAULTS,
            0,
            "refs/remotes/origin/main \nrefs/remotes/upstream/HEAD refs/remotes/upstream/dev\n",
        );
        assert_eq!(
            discover_base(&runner, Path::new(".")),
            remote("origin/main")
        );
    }

    #[test]
    fn base_discovery_uses_origin_head_before_fallbacks() {
        let runner = FakeRunner::default().with(
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
            0,
            "origin/trunk\n",
        );
        assert_eq!(
            discover_base(&runner, Path::new(".")),
            remote("origin/trunk")
        );
    }

    #[test]
    fn base_discovery_falls_back_to_common_branch_names() {
        let runner = FakeRunner::default().with(&REMOTE_DEFAULTS, 0, "").with(
            &["rev-parse", "--verify", "main"],
            0,
            "abc\n",
        );
        assert_eq!(
            discover_base(&runner, Path::new(".")),
            Some(Base {
                name: "main".to_string(),
                remote: false,
            })
        );
    }

    #[test]
    fn a_single_remote_not_named_origin_names_the_default_branch() {
        // Its `HEAD` first, then `main`, then `master`.
        for (listing, expected) in [
            (
                "refs/remotes/upstream/HEAD refs/remotes/upstream/trunk\nrefs/remotes/upstream/main \n",
                Some("upstream/trunk"),
            ),
            (
                "refs/remotes/fork/master \nrefs/remotes/fork/main \n",
                Some("fork/main"),
            ),
            // `origin` first, whatever else is there.
            (
                "refs/remotes/origin/master \nrefs/remotes/upstream/HEAD refs/remotes/upstream/main\n",
                Some("origin/master"),
            ),
            // Two remotes, neither `origin`: not toimija's to choose.
            ("refs/remotes/a/main \nrefs/remotes/b/main \n", None),
            ("", None),
        ] {
            assert_eq!(
                remote_default_branch(listing).as_deref(),
                expected,
                "{listing:?}"
            );
        }
    }

    #[test]
    fn one_rev_parse_names_the_whole_worktree() {
        let (worktree, at_root) = parse_discovery(DISCOVERED.as_bytes()).unwrap();
        assert!(at_root);
        assert_eq!(
            worktree,
            Worktree {
                root: PathBuf::from("/repo"),
                git_dir: Some(PathBuf::from(".git")),
                absolute_git_dir: Some(PathBuf::from("/repo/.git")),
                common_dir: Some(PathBuf::from("/repo/.git")),
                shallow: Some(false),
            }
        );

        // A linked worktree: its own git dir, absolute, and the main
        // checkout's common dir; a shallow clone says so.
        let linked = b"/wt\n\n/main/.git/worktrees/wt\n/main/.git/worktrees/wt\n/main/.git\ntrue\n";
        let (worktree, _) = parse_discovery(linked).unwrap();
        assert_eq!(
            worktree.git_dir.as_deref(),
            Some(Path::new("/main/.git/worktrees/wt"))
        );
        assert_eq!(
            worktree.common_dir.as_deref(),
            Some(Path::new("/main/.git"))
        );
        assert_eq!(worktree.shallow, Some(true));

        // Below the root: the relative answers are this directory's.
        let below = b"/repo\nsrc/\n/repo/.git\n/repo/.git\n../.git\nfalse\n";
        assert!(!parse_discovery(below).unwrap().1);
    }

    #[test]
    fn an_answer_that_cannot_be_split_is_not_guessed_at() {
        for stdout in [
            // A root with a newline in it: seven lines for six questions.
            &b"/re\npo\n\n.git\n/re\npo/.git\n.git\nfalse\n"[..],
            // A git that does not know a question echoes it back.
            b"/repo\n\n.git\n/repo/.git\n.git\n--is-shallow-repository\n",
            // Not one answer per line at all.
            b"/repo\n",
            b"",
            // A root that is not a path from the filesystem root.
            b"repo\n\n.git\n/repo/.git\n.git\nfalse\n",
        ] {
            assert_eq!(parse_discovery(stdout), None, "{stdout:?}");
        }

        // Discovery then reads the root alone and leaves the
        // rest to the readers that need it.
        let runner = FakeRunner::default()
            .with(
                &DISCOVERY,
                0,
                "/re\npo\n\n.git\n/re\npo/.git\n.git\nfalse\n",
            )
            .with(&["rev-parse", "--show-toplevel"], 0, "/re\npo\n");
        assert_eq!(
            discover(&runner, Path::new("/re\npo")).unwrap(),
            Worktree::at_root(PathBuf::from("/re\npo"))
        );
    }

    /// Answers by the directory git runs in, and records every call.
    #[derive(Default)]
    struct WhereRunner {
        answers: HashMap<(PathBuf, Vec<String>), String>,
        calls: std::cell::RefCell<Vec<(PathBuf, Vec<String>)>>,
    }

    impl WhereRunner {
        fn with(mut self, cwd: &str, args: &[&str], stdout: &str) -> Self {
            self.answers.insert(
                (
                    PathBuf::from(cwd),
                    args.iter().map(|arg| arg.to_string()).collect(),
                ),
                stdout.to_string(),
            );
            self
        }
    }

    impl CommandRunner for WhereRunner {
        fn run(&self, cwd: &Path, _program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let key = (
                cwd.to_path_buf(),
                args.iter().map(|arg| arg.to_string()).collect(),
            );
            self.calls.borrow_mut().push(key.clone());
            Ok(match self.answers.get(&key) {
                Some(stdout) => CommandOutput {
                    status: 0,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: Vec::new(),
                },
                None => CommandOutput {
                    status: 1,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
            })
        }
    }

    #[test]
    fn from_a_subdirectory_the_root_is_asked_again() {
        let runner = WhereRunner::default()
            .with(
                "/repo/src",
                &DISCOVERY,
                "/repo\nsrc/\n/repo/.git\n/repo/.git\n../.git\nfalse\n",
            )
            .with("/repo", &DISCOVERY, DISCOVERED);
        let worktree = discover(&runner, Path::new("/repo/src")).unwrap();
        assert_eq!(worktree.git_dir.as_deref(), Some(Path::new(".git")));
        assert_eq!(
            worktree.common_dir.as_deref(),
            Some(Path::new("/repo/.git"))
        );
        assert_eq!(runner.calls.borrow().len(), 2);

        // A root that does not answer as the root keeps what held from below
        // and leaves the relative answers to their readers.
        let runner = WhereRunner::default().with(
            "/repo/src",
            &DISCOVERY,
            "/repo\nsrc/\n/repo/.git\n/repo/.git\n../.git\nfalse\n",
        );
        let worktree = discover(&runner, Path::new("/repo/src")).unwrap();
        assert_eq!(worktree.git_dir, None);
        assert_eq!(worktree.common_dir, None);
        assert_eq!(
            worktree.absolute_git_dir.as_deref(),
            Some(Path::new("/repo/.git"))
        );
    }

    #[test]
    fn outside_a_repository_discovery_says_so_with_one_process() {
        let runner = WhereRunner::default();
        assert!(matches!(
            discover(&runner, Path::new("/tmp")),
            Err(ToimijaError::NotGitRepo)
        ));
        assert_eq!(runner.calls.borrow().len(), 1);
    }

    #[test]
    fn an_unborn_head_has_no_history_to_read() {
        let runner = FakeRunner::default().with(&DISCOVERY, 0, DISCOVERED).with(
            &STATUS,
            0,
            "# branch.oid (initial)\0# branch.head main\0",
        );
        let recording = Recording(runner, Default::default());
        let (state, warnings) = collect_git_state(&recording, Path::new("/repo")).unwrap();
        assert!(state.recent_commits.is_empty());
        assert!(warnings.contains(&"HEAD unavailable; branch has no commits yet.".to_string()));
        assert!(
            !recording.1.borrow().iter().any(|args| args[0] == "log"),
            "{:?}",
            recording.1.borrow()
        );
    }

    struct Recording(FakeRunner, std::cell::RefCell<Vec<Vec<String>>>);

    impl CommandRunner for Recording {
        fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.1
                .borrow_mut()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            self.0.run(cwd, program, args)
        }
    }
}

#[cfg(test)]
mod real_git_tests {
    use std::path::Path;
    use std::process::Command;

    use super::{Worktree, collect_git_state, status_state};
    use crate::command::ProcessCommandRunner;

    /// `git` pinned to `dir`, with the variables that would point it at
    /// another repository removed and an identity of its own.
    fn git(dir: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        command.current_dir(dir).args(args);
        for var in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
        ] {
            command.env_remove(var);
        }
        let output = command
            .env("GIT_AUTHOR_NAME", "Toimija Test")
            .env("GIT_AUTHOR_EMAIL", "toimija@example.invalid")
            .env("GIT_COMMITTER_NAME", "Toimija Test")
            .env("GIT_COMMITTER_EMAIL", "toimija@example.invalid")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// `dirty` leaves out generated tool state: a session marker, a handover
    /// or a gate's receipt in `.sykli/` does not make the state dirty, whether
    /// this machine's ignore rules hide it or not. `.toimija/repo.yaml` is
    /// work. The one-`status` read `run` makes at exit agrees with the full one.
    #[test]
    fn dirty_leaves_out_generated_tool_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "-q", "-m", "init"]);
        // No ignore file from this machine: the exclude file below decides
        // what git can see.
        git(root, &["config", "core.excludesFile", "/dev/null"]);
        let runner = ProcessCommandRunner;
        let read = || {
            let (full, _warnings) = collect_git_state(&runner, root).unwrap();
            let lean = status_state(&runner, &Worktree::at_root(full.repo_root.clone())).unwrap();
            assert_eq!(lean.dirty, full.dirty);
            assert_eq!(lean.branch, full.branch);
            assert_eq!(lean.head_oid, full.head_oid);
            full
        };
        assert!(!read().dirty);

        for generated in [
            ".sykli/receipt.json",
            ".toimija/handover.json",
            ".toimija/sessions/100-1/session.json",
        ] {
            let path = root.join(generated);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "generated\n").unwrap();
        }
        // Hidden by this machine's ignore rules, and visible to git.
        std::fs::write(root.join(".git/info/exclude"), ".sykli/\n").unwrap();
        let hidden = read();
        std::fs::write(root.join(".git/info/exclude"), "").unwrap();
        let visible = read();
        assert!(
            visible
                .untracked_files
                .contains(&std::path::PathBuf::from(".sykli/receipt.json")),
            "git lists the receipt: {:?}",
            visible.untracked_files
        );
        for state in [&hidden, &visible] {
            assert!(!state.dirty, "{:?}", state.untracked_files);
        }

        std::fs::write(root.join(".toimija/repo.yaml"), "repo:\n  name: x\n").unwrap();
        assert!(read().dirty, "repo.yaml is the repository's own");
    }
}
