use std::collections::{BTreeSet, hash_map::RandomState};
use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

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
    /// Coverage of the merge-base diff behind `branch_files` (R10, TA-005):
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentIdentity {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub detached: bool,
    pub staged_files: Vec<ChangedFile>,
    pub unstaged_files: Vec<ChangedFile>,
    pub untracked_files: Vec<PathBuf>,
    pub conflicted_files: Vec<PathBuf>,
    pub conflict_entries: Vec<ConflictEntry>,
}

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

/// Read or mint the clone identity in Git's common directory. `--git-common-dir`
/// makes linked worktrees share the same identity without putting it in the
/// worktree or inventing another registry.
pub fn ensure_environment_identity(
    runner: &impl CommandRunner,
    repo_root: &Path,
) -> Result<EnvironmentIdentity, String> {
    let common_dir = run_git_ok(runner, repo_root, &["rev-parse", "--git-common-dir"])
        .map(|path| PathBuf::from(path.trim()))
        .ok_or_else(|| "Git common directory is unavailable".to_string())?;
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        repo_root.join(common_dir)
    };
    let path = common_dir.join("toimija-id");
    if path.exists() {
        return fs::read(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .map_err(|error| format!("invalid {}: {error}", path.display()))
            });
    }

    let identity = EnvironmentIdentity {
        id: random_id(repo_root),
        label: hostname_label(),
    };
    let bytes = serde_json::to_vec_pretty(&identity)
        .map_err(|error| format!("failed to serialize clone identity: {error}"))?;
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            if let Err(error) = file.write_all(&bytes).and_then(|_| file.write_all(b"\n")) {
                let _ = fs::remove_file(&path);
                return Err(format!("failed to write {}: {error}", path.display()));
            }
            Ok(identity)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => fs::read(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .map_err(|error| format!("invalid {}: {error}", path.display()))
            }),
        Err(error) => Err(format!("failed to create {}: {error}", path.display())),
    }
}

fn random_id(repo_root: &Path) -> String {
    let mut hasher = RandomState::new().build_hasher();
    repo_root.hash(&mut hasher);
    SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    format!("{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

fn hostname_label() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "unknown-host".to_string())
}

pub fn collect_git_state(
    runner: &impl CommandRunner,
    cwd: &Path,
) -> Result<(GitState, Vec<String>), ToimijaError> {
    let mut warnings = Vec::new();
    let repo_root = discover_repo_root(runner, cwd)?;
    let git_dir = run_git_ok_bytes(runner, &repo_root, &["rev-parse", "--git-dir"])
        .map(|bytes| crate::path_codec::from_git_bytes(trim_ascii_end(&bytes)));
    let head_oid =
        run_git_ok(runner, &repo_root, &["rev-parse", "HEAD"]).map(|text| text.trim().to_string());
    let branch = run_git_ok(
        runner,
        &repo_root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .map(|text| text.trim().to_string());
    let status_started = Instant::now();
    // Native watchers see Git's optional index-refresh lock. Disable it for
    // this read-only status so refreshing a packet cannot trigger itself.
    let status_output = run_git_required_bytes(
        runner,
        &repo_root,
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
    let mut parsed = parse_porcelain_v2_bytes(&status_output);
    if parsed.branch.is_none() {
        parsed.branch = branch.clone();
    }
    if head_oid.is_none() {
        if parsed.branch.is_some() || branch.is_some() {
            warnings.push("HEAD unavailable; branch has no commits yet.".to_string());
        } else {
            warnings.push("HEAD unavailable.".to_string());
        }
    }
    let detached = parsed.detached || (branch.is_none() && head_oid.is_some());
    let base = discover_base(runner, &repo_root);
    let merge_base = if head_oid.is_some() {
        base.as_deref().and_then(|base| {
            run_git_ok(runner, &repo_root, &["merge-base", "HEAD", base])
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
        match base.as_deref() {
            Some(base) => warnings.push(format!(
                "No merge-base with {base}. Branch surface is unavailable."
            )),
            None => {
                warnings.push("No base branch detected. Branch surface is unavailable.".to_string())
            }
        }
    }
    let (branch_files, branch_diff_coverage) = match (merge_base.as_deref(), head_oid.as_deref()) {
        (Some(base_oid), Some(head)) if base_oid != head => {
            match run_git_observed(
                runner,
                &repo_root,
                &["diff", "--name-status", "-z", base_oid, head],
            ) {
                Ok(bytes) => (
                    parse_name_status_bytes(&bytes),
                    Coverage::available(BRANCH_DIFF_SOURCE),
                ),
                Err(reason) => {
                    warnings.push(
                        "Branch diff collection failed. Branch surface is unknown, not empty."
                            .to_string(),
                    );
                    (Vec::new(), Coverage::failed(BRANCH_DIFF_SOURCE, reason))
                }
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
    let recent_commits = run_git_ok(runner, &repo_root, &["log", "-n", "10", "--oneline"])
        .map(|text| parse_recent_commits(&text))
        .unwrap_or_default();
    if parsed.upstream.is_none() {
        warnings.push("No upstream detected. Branch divergence is unavailable.".to_string());
    }
    if run_git_ok(
        runner,
        &repo_root,
        &["rev-parse", "--is-shallow-repository"],
    )
    .map(|text| text.trim() == "true")
    .unwrap_or(false)
    {
        warnings.push("Shallow repository. Merge-base may be incomplete.".to_string());
    }
    if detached {
        warnings.push("Detached HEAD. Broad agent work is risky here.".to_string());
    }
    if !parsed.conflicted_files.is_empty() {
        warnings.push("Conflicted files exist. Resolve conflicts before continuing.".to_string());
    }
    if !parsed.unstaged_files.is_empty() {
        warnings.push("Working tree has unstaged changes.".to_string());
    }
    if !parsed.untracked_files.is_empty() {
        warnings
            .push("Untracked files exist. They will not be committed unless added.".to_string());
    }
    let dirty = !parsed.staged_files.is_empty()
        || !parsed.unstaged_files.is_empty()
        || !parsed.untracked_files.is_empty()
        || !parsed.conflicted_files.is_empty();
    debug!(
        event = "toimija.git.state.collected",
        repo_root = %repo_root.display(),
        branch = parsed.branch.as_deref().or(branch.as_deref()).unwrap_or("none"),
        detached,
        merge_base_available = merge_base.is_some(),
        status_duration_ms = status_duration_ms
    );
    Ok((
        GitState {
            repo_root,
            git_dir,
            branch: parsed.branch.or(branch),
            head_oid,
            upstream: parsed.upstream,
            base,
            merge_base,
            detached,
            dirty,
            staged_files: parsed.staged_files,
            unstaged_files: parsed.unstaged_files,
            untracked_files: parsed.untracked_files,
            conflicted_files: parsed.conflicted_files,
            conflict_entries: parsed.conflict_entries,
            branch_files,
            branch_diff_coverage,
            recent_commits,
        },
        warnings,
    ))
}

fn discover_base(runner: &impl CommandRunner, repo_root: &Path) -> Option<String> {
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
        return Some(default_remote);
    }
    for candidate in ["origin/main", "origin/master", "main", "master"] {
        if run_git_ok(runner, repo_root, &["rev-parse", "--verify", candidate]).is_some() {
            return Some(candidate.to_string());
        }
    }
    None
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

/// Run a git command, returning its stdout only on success. Shared with the
/// relevance layer for bounded history mining.
pub(crate) fn run_git_ok(runner: &impl CommandRunner, cwd: &Path, args: &[&str]) -> Option<String> {
    run_git_ok_bytes(runner, cwd, args).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn run_git_ok_bytes(runner: &impl CommandRunner, cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = runner.run(cwd, "git", args).ok()?;
    (output.status == 0).then_some(output.stdout)
}

/// The coverage source label for the merge-base branch diff.
const BRANCH_DIFF_SOURCE: &str = "git diff --name-status <merge-base> HEAD";

/// Run a git command for a stage whose failure affects a product claim (R10):
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

/// Compatibility parser for callers that provide line-delimited porcelain v2.
/// Git collection uses [`parse_porcelain_v2_bytes`] so paths remain lossless.
pub fn parse_porcelain_v2(text: &str) -> ParsedStatus {
    let nul = text.replace(['\n', '\t'], "\0");
    parse_porcelain_v2_bytes(nul.as_bytes())
}

pub(crate) fn parse_porcelain_v2_bytes(bytes: &[u8]) -> ParsedStatus {
    let mut parsed = ParsedStatus::default();
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
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
/// Compatibility parser for callers that provide line-delimited name-status.
/// Git collection uses [`parse_name_status_bytes`] so paths remain lossless.
pub fn parse_name_status(text: &str) -> Vec<ChangedFile> {
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

    #[test]
    fn clone_identity_is_minted_once_in_the_common_git_dir() {
        let temp = tempfile::tempdir().unwrap();
        let common = temp.path().join("common.git");
        let worktree = temp.path().join("worktree");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let common_text = format!("{}\n", common.display());
        let runner =
            FakeRunner::default().with(&["rev-parse", "--git-common-dir"], 0, &common_text);

        let first = ensure_environment_identity(&runner, &worktree).unwrap();
        let second = ensure_environment_identity(&runner, &worktree).unwrap();

        assert_eq!(first, second);
        assert!(!first.id.is_empty());
        assert!(!first.label.is_empty());
        assert!(common.join("toimija-id").is_file());
        assert!(!worktree.join("toimija-id").exists());
    }

    /// A runner primed with everything `collect_git_state` needs up to the
    /// branch diff, so tests can vary only the diff outcome.
    fn runner_with_merge_base() -> FakeRunner {
        FakeRunner::default()
            .with(&["rev-parse", "--show-toplevel"], 0, "/repo\n")
            .with(&["rev-parse", "--git-dir"], 0, ".git\n")
            .with(&["rev-parse", "HEAD"], 0, "headoid\n")
            .with(
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                0,
                "feat/x\n",
            )
            .with(
                &[
                    "--no-optional-locks",
                    "status",
                    "--porcelain=v2",
                    "--branch",
                    "--untracked-files=all",
                    "-z",
                ],
                0,
                "",
            )
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
        let runner = FakeRunner::default()
            .with(&["rev-parse", "--show-toplevel"], 0, "/repo\n")
            .with(&["rev-parse", "HEAD"], 0, "headoid\n")
            .with(
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                0,
                "feat/x\n",
            )
            .with(
                &[
                    "--no-optional-locks",
                    "status",
                    "--porcelain=v2",
                    "--branch",
                    "--untracked-files=all",
                    "-z",
                ],
                0,
                "",
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

    #[test]
    fn base_discovery_ignores_feature_upstream_and_prefers_trunk() {
        let runner = FakeRunner::default()
            .with(&["rev-parse", "--verify", "origin/main"], 0, "abc\n")
            .with(&["rev-parse", "--verify", "origin/feature"], 0, "def\n");
        let base = discover_base(&runner, Path::new("."));
        assert_eq!(base.as_deref(), Some("origin/main"));
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
        let base = discover_base(&runner, Path::new("."));
        assert_eq!(base.as_deref(), Some("origin/trunk"));
    }

    #[test]
    fn base_discovery_falls_back_to_common_branch_names() {
        let runner = FakeRunner::default()
            .with(&["rev-parse", "--verify", "origin/main"], 1, "")
            .with(&["rev-parse", "--verify", "origin/master"], 1, "")
            .with(&["rev-parse", "--verify", "main"], 0, "abc\n");
        let base = discover_base(&runner, Path::new("."));
        assert_eq!(base.as_deref(), Some("main"));
    }
}
