use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::command::CommandRunner;
use crate::error::ToimijaError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitState {
    pub repo_root: PathBuf,
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
    pub untracked_files: Vec<PathBuf>,
    pub conflicted_files: Vec<PathBuf>,
    pub recent_commits: Vec<RecentCommit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub status: ChangeStatus,
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
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub detached: bool,
    pub staged_files: Vec<ChangedFile>,
    pub unstaged_files: Vec<ChangedFile>,
    pub untracked_files: Vec<PathBuf>,
    pub conflicted_files: Vec<PathBuf>,
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
    Ok(PathBuf::from(output.stdout.trim()))
}

pub fn collect_git_state(
    runner: &impl CommandRunner,
    cwd: &Path,
) -> Result<(GitState, Vec<String>), ToimijaError> {
    let mut warnings = Vec::new();
    let repo_root = discover_repo_root(runner, cwd)?;
    let git_dir = run_git_ok(runner, &repo_root, &["rev-parse", "--git-dir"])
        .map(|text| PathBuf::from(text.trim()));
    let head_oid =
        run_git_ok(runner, &repo_root, &["rev-parse", "HEAD"]).map(|text| text.trim().to_string());
    let branch = run_git_ok(
        runner,
        &repo_root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .map(|text| text.trim().to_string());
    let status_output = run_git_required(
        runner,
        &repo_root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
        ],
    )?;
    let mut parsed = parse_porcelain_v2(&status_output);
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
    let base = discover_base(runner, &repo_root, parsed.upstream.as_deref());
    let merge_base = if head_oid.is_some() {
        base.as_deref().and_then(|base| {
            run_git_ok(runner, &repo_root, &["merge-base", "HEAD", base])
                .map(|text| text.trim().to_string())
        })
    } else {
        None
    };
    if merge_base.is_none() {
        match base.as_deref() {
            Some(base) if head_oid.is_some() => {
                warnings.push(format!("No merge-base detected against {base}."));
            }
            Some(base) => warnings.push(format!(
                "Merge-base unavailable against {base}; HEAD is unavailable."
            )),
            None => warnings.push("No base branch detected. Merge-base unavailable.".to_string()),
        }
    }
    let recent_commits = run_git_ok(runner, &repo_root, &["log", "-n", "10", "--oneline"])
        .map(|text| parse_recent_commits(&text))
        .unwrap_or_default();
    if parsed.upstream.is_none() {
        if let Some(base) = &base {
            warnings.push(format!(
                "No upstream detected; using {base} as base candidate."
            ));
        } else {
            warnings.push("No upstream detected.".to_string());
        }
    }
    if run_git_ok(
        runner,
        &repo_root,
        &["rev-parse", "--is-shallow-repository"],
    )
    .map(|text| text.trim() == "true")
    .unwrap_or(false)
    {
        warnings.push("Shallow repository detected; merge-base may be incomplete.".to_string());
    }
    if detached {
        warnings.push("Detached HEAD detected.".to_string());
    }
    if !parsed.conflicted_files.is_empty() {
        warnings.push("Conflicted files detected.".to_string());
    }
    let dirty = !parsed.staged_files.is_empty()
        || !parsed.unstaged_files.is_empty()
        || !parsed.untracked_files.is_empty()
        || !parsed.conflicted_files.is_empty();
    if dirty {
        warnings.push("Working tree has local changes.".to_string());
    }
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
            recent_commits,
        },
        warnings,
    ))
}

fn discover_base(
    runner: &impl CommandRunner,
    repo_root: &Path,
    upstream: Option<&str>,
) -> Option<String> {
    if let Some(upstream) = upstream {
        return Some(upstream.to_string());
    }
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

fn run_git_required(
    runner: &impl CommandRunner,
    cwd: &Path,
    args: &[&str],
) -> Result<String, ToimijaError> {
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

fn run_git_ok(runner: &impl CommandRunner, cwd: &Path, args: &[&str]) -> Option<String> {
    let output = runner.run(cwd, "git", args).ok()?;
    (output.status == 0).then_some(output.stdout)
}

pub fn parse_porcelain_v2(text: &str) -> ParsedStatus {
    let mut parsed = ParsedStatus::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            if rest == "(detached)" {
                parsed.detached = true;
            } else {
                parsed.branch = Some(rest.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            parsed.upstream = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("? ") {
            parsed.untracked_files.push(PathBuf::from(rest));
            continue;
        }
        if let Some(rest) = line.strip_prefix("u ") {
            if let Some(path) = rest.split_whitespace().last() {
                parsed.conflicted_files.push(PathBuf::from(path));
            }
            continue;
        }
        if line.starts_with("1 ") || line.starts_with("2 ") {
            parse_changed_line(line, &mut parsed);
        }
    }
    dedupe(&mut parsed);
    parsed
}

fn parse_changed_line(line: &str, parsed: &mut ParsedStatus) {
    let (xy, path) = if line.starts_with("2 ") {
        let parts: Vec<&str> = line.splitn(10, ' ').collect();
        if parts.len() < 10 {
            return;
        }
        let path = parts[9].split_once('\t').map_or(parts[9], |(path, _)| path);
        (parts[1], PathBuf::from(path))
    } else {
        let parts: Vec<&str> = line.splitn(9, ' ').collect();
        if parts.len() < 9 {
            return;
        }
        (parts[1], PathBuf::from(parts[8]))
    };
    let mut chars = xy.chars();
    let staged = chars.next().unwrap_or('.');
    let unstaged = chars.next().unwrap_or('.');
    if staged != '.' {
        parsed.staged_files.push(ChangedFile {
            path: path.clone(),
            status: status_from_char(staged),
        });
    }
    if unstaged != '.' {
        parsed.unstaged_files.push(ChangedFile {
            path,
            status: status_from_char(unstaged),
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
        assert_eq!(parsed.unstaged_files[0].path, PathBuf::from("README.md"));
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
        assert_eq!(parsed.staged_files[1].status, ChangeStatus::Copied);
        assert_eq!(parsed.staged_files[1].path, PathBuf::from("src/copied.rs"));
    }

    #[test]
    fn base_discovery_prefers_upstream() {
        let runner = FakeRunner::default();
        let base = discover_base(&runner, Path::new("."), Some("origin/feature"));
        assert_eq!(base.as_deref(), Some("origin/feature"));
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
        let base = discover_base(&runner, Path::new("."), None);
        assert_eq!(base.as_deref(), Some("origin/trunk"));
    }

    #[test]
    fn base_discovery_falls_back_to_common_branch_names() {
        let runner = FakeRunner::default()
            .with(&["rev-parse", "--verify", "origin/main"], 1, "")
            .with(&["rev-parse", "--verify", "origin/master"], 1, "")
            .with(&["rev-parse", "--verify", "main"], 0, "abc\n");
        let base = discover_base(&runner, Path::new("."), None);
        assert_eq!(base.as_deref(), Some("main"));
    }
}
