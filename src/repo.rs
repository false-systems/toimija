use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::command::{CommandRunner, command_available};
use crate::{config::RepoConfig, git};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RepoKind {
    Rust,
    Elixir,
    Generic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoInfo {
    pub name: String,
    pub role: Option<String>,
    pub not_roles: Vec<String>,
    pub important_docs: Vec<PathBuf>,
    pub boundaries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub serious: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warning,
    Fail,
}

impl RepoInfo {
    pub fn from_config(config: &RepoConfig, repo_root: &Path) -> Self {
        let name = if config.repo.name.trim().is_empty() {
            repo_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string()
        } else {
            config.repo.name.clone()
        };
        Self {
            name,
            role: non_empty(&config.repo.role),
            not_roles: config.repo.not_roles.clone(),
            important_docs: config.context.important_docs.clone(),
            boundaries: config.boundaries.clone(),
        }
    }
}

pub fn detect_repo_kind(repo_root: &Path) -> RepoKind {
    let rust = repo_root.join("Cargo.toml").exists();
    let elixir = repo_root.join("mix.exs").exists();
    match (rust, elixir) {
        (true, _) => RepoKind::Rust,
        (false, true) => RepoKind::Elixir,
        (false, false) => RepoKind::Generic,
    }
}

pub fn doctor(runner: &impl CommandRunner, cwd: &Path) -> DoctorReport {
    let mut checks = Vec::new();
    let git_available = command_available("git");
    checks.push(check(
        "git installed",
        if git_available {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        },
        if git_available {
            "git found"
        } else {
            "git not found"
        },
    ));
    let repo_root = match git::discover_repo_root(runner, cwd) {
        Ok(root) => {
            checks.push(check(
                "inside git repo",
                DoctorStatus::Ok,
                "git repo detected",
            ));
            root
        }
        Err(_) => {
            checks.push(check(
                "inside git repo",
                DoctorStatus::Fail,
                "not a Git repository",
            ));
            return DoctorReport {
                serious: true,
                checks,
            };
        }
    };
    let config = match RepoConfig::load_optional(&repo_root) {
        Ok(Some(config)) => {
            checks.push(check("toimija config", DoctorStatus::Ok, "config found"));
            config
        }
        Ok(None) => {
            checks.push(check(
                "toimija config",
                DoctorStatus::Warning,
                "missing config; run `toimija init`",
            ));
            RepoConfig::default_for(&repo_root)
        }
        Err(error) => {
            checks.push(check(
                "toimija config",
                DoctorStatus::Fail,
                &format!("{error}"),
            ));
            return DoctorReport {
                serious: true,
                checks,
            };
        }
    };
    checks.push(check(
        "repo name",
        if config.repo.name.trim().is_empty() {
            DoctorStatus::Warning
        } else {
            DoctorStatus::Ok
        },
        if config.repo.name.trim().is_empty() {
            "repo name is empty"
        } else {
            "repo name present"
        },
    ));
    checks.push(check(
        "repo role",
        if config.repo.role.trim().is_empty() {
            DoctorStatus::Warning
        } else {
            DoctorStatus::Ok
        },
        if config.repo.role.trim().is_empty() {
            "repo role is empty"
        } else {
            "repo role present"
        },
    ));
    checks.push(check(
        "boundaries",
        if config.boundaries.is_empty() {
            DoctorStatus::Warning
        } else {
            DoctorStatus::Ok
        },
        if config.boundaries.is_empty() {
            "boundaries are empty"
        } else {
            "boundaries present"
        },
    ));
    let kind = detect_repo_kind(&repo_root);
    checks.push(check(
        "repo kind",
        DoctorStatus::Ok,
        match kind {
            RepoKind::Rust => "Rust repo detected",
            RepoKind::Elixir => "Elixir repo detected",
            RepoKind::Generic => "generic repo",
        },
    ));
    checks.push(check(
        "validation plan",
        if config.has_validation() || kind != RepoKind::Generic {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warning
        },
        if config.has_validation() {
            "validation configured"
        } else if kind != RepoKind::Generic {
            "validation can be inferred"
        } else {
            "validation unknown"
        },
    ));
    checks.push(file_check(&repo_root, "AGENTS.md", "AGENTS.md"));
    let claude = repo_root.join("CLAUDE.md");
    if claude.exists() {
        let text = std::fs::read_to_string(&claude).unwrap_or_default();
        checks.push(check(
            "CLAUDE.md",
            if text.contains("AGENTS.md") {
                DoctorStatus::Ok
            } else {
                DoctorStatus::Warning
            },
            if text.contains("AGENTS.md") {
                "CLAUDE.md references AGENTS.md"
            } else {
                "CLAUDE.md exists but does not reference AGENTS.md"
            },
        ));
    } else {
        checks.push(check("CLAUDE.md", DoctorStatus::Ok, "CLAUDE.md absent"));
    }
    let serious = checks
        .iter()
        .any(|check| check.status == DoctorStatus::Fail);
    DoctorReport { serious, checks }
}

fn file_check(repo_root: &Path, name: &str, path: &str) -> DoctorCheck {
    let present = repo_root.join(path).exists();
    check(
        name,
        if present {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warning
        },
        if present {
            "file present"
        } else {
            "file missing"
        },
    )
}

fn check(name: &str, status: DoctorStatus, message: &str) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status,
        message: message.to_string(),
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_repo() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/rust_repo");
        assert_eq!(detect_repo_kind(&root), RepoKind::Rust);
    }

    #[test]
    fn detects_elixir_repo() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/elixir_repo");
        assert_eq!(detect_repo_kind(&root), RepoKind::Elixir);
    }
}
