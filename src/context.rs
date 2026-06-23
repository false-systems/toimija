use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::adapters::LanguageAdapter;
use crate::adapters::elixir::ElixirAdapter;
use crate::adapters::generic::GenericAdapter;
use crate::adapters::rust::RustAdapter;
use crate::command::CommandRunner;
use crate::config::RepoConfig;
use crate::git::{self, ChangedFile, GitState};
use crate::repo::{self, RepoInfo, RepoKind};
use crate::validate::ValidationPlan;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextPacket {
    pub repo: RepoInfo,
    pub repo_kind: RepoKind,
    pub git: GitState,
    pub changed_surface: Vec<PathBuf>,
    pub read_first: Vec<ReadFirstEntry>,
    pub safe_to_ignore: Vec<PathBuf>,
    pub validation_plan: ValidationPlan,
    pub warnings: Vec<String>,
    pub for_diff: bool,
    pub default_budget: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFirstEntry {
    pub path: PathBuf,
    pub reason: String,
}

impl ContextPacket {
    pub fn config_default_budget(&self) -> Option<usize> {
        self.default_budget
    }
}

pub fn build_context_packet(
    runner: &impl CommandRunner,
    cwd: &Path,
    for_diff: bool,
) -> anyhow::Result<ContextPacket> {
    let (git, mut warnings) = git::collect_git_state(runner, cwd)?;
    let mut git = git;
    normalize_git_state(&mut git);
    let config = match RepoConfig::load_optional(&git.repo_root)? {
        Some(config) => config,
        None => {
            warnings.push("Missing repo.yaml; run `toimija init`.".to_string());
            RepoConfig::default_for(&git.repo_root)
        }
    };
    let repo_kind = repo::detect_repo_kind(&git.repo_root);
    if repo_kind == RepoKind::Generic {
        warnings.push("Repo kind is generic; Rust and Elixir markers were not found.".to_string());
    }
    if config.repo.role.trim().is_empty() {
        warnings.push("Missing product role in .toimija/repo.yaml.".to_string());
    }
    if config.boundaries.is_empty() {
        warnings.push("Missing product boundaries in .toimija/repo.yaml.".to_string());
    }
    if matches!(repo_kind, RepoKind::Rust) && git.repo_root.join("mix.exs").exists() {
        warnings.push(
            "Both Rust and Elixir markers detected; Rust chosen deterministically.".to_string(),
        );
    }
    let adapter = adapter_for(repo_kind);
    let validation_plan = adapter.validation_plan(&git.repo_root, &config);
    for note in &validation_plan.notes {
        if note.contains("inferred") {
            warnings.push(note.clone());
        }
    }
    warnings = stable_strings(warnings);
    let changed = all_changed(&git);
    let changed_surface = changed.iter().map(|file| file.path.clone()).collect();
    let read_first = read_first(&git.repo_root, &config, adapter.as_ref(), &changed);
    let packet = ContextPacket {
        repo: RepoInfo::from_config(&config, &git.repo_root),
        repo_kind,
        git,
        changed_surface,
        read_first,
        safe_to_ignore: Vec::new(),
        validation_plan,
        warnings,
        for_diff,
        default_budget: Some(config.context.default_budget),
    };
    info!(
        event = "toimija.context.rendered",
        repo_kind = ?packet.repo_kind,
        warnings = packet.warnings.len()
    );
    Ok(packet)
}

fn adapter_for(kind: RepoKind) -> Box<dyn LanguageAdapter> {
    match kind {
        RepoKind::Rust => Box::new(RustAdapter),
        RepoKind::Elixir => Box::new(ElixirAdapter),
        RepoKind::Generic => Box::new(GenericAdapter),
    }
}

fn all_changed(git: &GitState) -> Vec<ChangedFile> {
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    for file in git
        .staged_files
        .iter()
        .chain(git.unstaged_files.iter())
        .chain(git.branch_files.iter())
        .cloned()
        .chain(git.untracked_files.iter().cloned().map(|path| ChangedFile {
            path,
            status: git::ChangeStatus::Untracked,
        }))
    {
        if seen.insert(file.path.clone()) {
            files.push(file);
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

fn normalize_git_state(git: &mut GitState) {
    sort_changed_files(&mut git.staged_files);
    sort_changed_files(&mut git.unstaged_files);
    sort_changed_files(&mut git.branch_files);
    git.untracked_files = stable_paths(std::mem::take(&mut git.untracked_files));
    git.conflicted_files = stable_paths(std::mem::take(&mut git.conflicted_files));
}

fn sort_changed_files(files: &mut Vec<ChangedFile>) {
    files.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(status_rank(&left.status).cmp(&status_rank(&right.status)))
    });
    files.dedup_by(|left, right| left.path == right.path && left.status == right.status);
}

fn status_rank(status: &git::ChangeStatus) -> u8 {
    match status {
        git::ChangeStatus::Added => 0,
        git::ChangeStatus::Modified => 1,
        git::ChangeStatus::Deleted => 2,
        git::ChangeStatus::Renamed => 3,
        git::ChangeStatus::Copied => 4,
        git::ChangeStatus::Untracked => 5,
        git::ChangeStatus::Unknown(_) => 6,
    }
}

fn stable_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn stable_strings(strings: Vec<String>) -> Vec<String> {
    strings
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn read_first(
    repo_root: &Path,
    config: &RepoConfig,
    adapter: &dyn LanguageAdapter,
    changed: &[ChangedFile],
) -> Vec<ReadFirstEntry> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for file in changed {
        if seen.insert(file.path.clone()) {
            entries.push(ReadFirstEntry {
                path: file.path.clone(),
                reason: "changed".to_string(),
            });
        }
    }
    let mut hints = adapter.read_first_hints(repo_root, changed);
    hints.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    for (path, reason) in hints {
        if seen.insert(path.clone()) {
            entries.push(ReadFirstEntry { path, reason });
        }
    }
    for doc in &config.context.important_docs {
        if repo_root.join(doc).exists() && seen.insert(doc.clone()) {
            entries.push(ReadFirstEntry {
                path: doc.clone(),
                reason: "important doc".to_string(),
            });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{ChangeStatus, ChangedFile};

    #[test]
    fn context_packet_serializes_to_json() {
        let git = GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: Some(PathBuf::from(".git")),
            branch: Some("main".to_string()),
            head_oid: Some("abc".to_string()),
            upstream: None,
            base: None,
            merge_base: None,
            detached: false,
            dirty: false,
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            conflicted_files: Vec::new(),
            branch_files: Vec::new(),
            recent_commits: Vec::new(),
        };
        let packet = ContextPacket {
            repo: RepoInfo {
                name: "repo".to_string(),
                role: None,
                not_roles: Vec::new(),
                important_docs: vec![PathBuf::from("README.md")],
                boundaries: Vec::new(),
            },
            repo_kind: RepoKind::Generic,
            git,
            changed_surface: Vec::new(),
            read_first: Vec::new(),
            safe_to_ignore: Vec::new(),
            validation_plan: crate::validate::validation_plan(
                Path::new("/repo"),
                &RepoConfig::default(),
                RepoKind::Generic,
            ),
            warnings: Vec::new(),
            for_diff: false,
            default_budget: Some(4000),
        };
        let json = serde_json::to_string(&packet).unwrap();
        assert!(json.contains("\"repo\""));
    }

    #[test]
    fn read_first_includes_changed_and_docs() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("README.md"), "docs").unwrap();
        let mut config = RepoConfig::default();
        config.context.important_docs = vec![PathBuf::from("README.md")];
        let changed = vec![ChangedFile {
            path: PathBuf::from("src/lib.rs"),
            status: ChangeStatus::Modified,
        }];
        let entries = read_first(temp.path(), &config, &GenericAdapter, &changed);
        assert_eq!(entries[0].path, PathBuf::from("src/lib.rs"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == Path::new("README.md"))
        );
    }

    #[test]
    fn context_ordering_rules_are_stable() {
        let mut git = GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: None,
            branch: Some("main".to_string()),
            head_oid: Some("abc".to_string()),
            upstream: None,
            base: None,
            merge_base: None,
            detached: false,
            dirty: true,
            staged_files: vec![
                ChangedFile {
                    path: PathBuf::from("src/z.rs"),
                    status: ChangeStatus::Modified,
                },
                ChangedFile {
                    path: PathBuf::from("src/a.rs"),
                    status: ChangeStatus::Added,
                },
            ],
            unstaged_files: vec![ChangedFile {
                path: PathBuf::from("src/a.rs"),
                status: ChangeStatus::Modified,
            }],
            untracked_files: vec![PathBuf::from("notes.md"), PathBuf::from("notes.md")],
            conflicted_files: vec![PathBuf::from("b.txt"), PathBuf::from("a.txt")],
            branch_files: vec![
                ChangedFile {
                    path: PathBuf::from("src/z.rs"),
                    status: ChangeStatus::Modified,
                },
                ChangedFile {
                    path: PathBuf::from("src/a.rs"),
                    status: ChangeStatus::Added,
                },
            ],
            recent_commits: Vec::new(),
        };

        normalize_git_state(&mut git);
        assert_eq!(
            git.branch_files
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("src/a.rs"), Path::new("src/z.rs")]
        );
        assert_eq!(
            git.staged_files
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("src/a.rs"), Path::new("src/z.rs")]
        );
        assert_eq!(
            git.conflicted_files,
            vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")]
        );

        let changed = all_changed(&git);
        assert_eq!(
            changed
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![
                Path::new("notes.md"),
                Path::new("src/a.rs"),
                Path::new("src/z.rs")
            ]
        );
        assert_eq!(
            stable_strings(vec!["z".to_string(), "a".to_string(), "a".to_string()]),
            vec!["a".to_string(), "z".to_string()]
        );
    }
}
