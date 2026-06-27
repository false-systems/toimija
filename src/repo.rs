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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorMode {
    /// Advisory: setup gaps surface as warnings and the command still exits 0.
    Default,
    /// Gate: every actionable warning is escalated to a failure so CI/precommit
    /// can block drift. Used as `toimija doctor --strict`.
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub serious: bool,
    pub strict: bool,
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

pub fn doctor(runner: &impl CommandRunner, cwd: &Path, mode: DoctorMode) -> DoctorReport {
    let strict = mode == DoctorMode::Strict;
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
                strict,
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
                strict,
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

    // Drift guard: the agent docs are the contract a coding agent reads. If they
    // stop referring people to the preflight, or stop reflecting the configured
    // boundaries, the contract has silently rotted even though every file still
    // parses. These checks are advisory by default and gate under `--strict`.
    let docs_text = read_canonical_docs(&repo_root, &config);
    checks.push(check(
        "preflight mention",
        if docs_text.contains(PREFLIGHT_MARKER) {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warning
        },
        if docs_text.contains(PREFLIGHT_MARKER) {
            "agent docs point at `toimija context`"
        } else {
            "no agent doc mentions the `toimija context` preflight"
        },
    ));

    let missing_docs: Vec<&PathBuf> = config
        .context
        .important_docs
        .iter()
        .filter(|doc| !repo_root.join(doc).exists())
        .collect();
    checks.push(check(
        "important docs",
        if missing_docs.is_empty() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warning
        },
        &if missing_docs.is_empty() {
            "important_docs paths exist".to_string()
        } else {
            format!("missing on disk: {}", join_paths(&missing_docs))
        },
    ));

    if !config.boundaries.is_empty() {
        let unmirrored: Vec<&String> = config
            .boundaries
            .iter()
            .filter(|boundary| !boundary_present(boundary, &docs_text))
            .collect();
        checks.push(check(
            "boundaries mirrored",
            if unmirrored.is_empty() {
                DoctorStatus::Ok
            } else {
                DoctorStatus::Warning
            },
            &if unmirrored.is_empty() {
                "boundaries appear in the agent docs".to_string()
            } else {
                format!(
                    "{} boundary line(s) missing from agent docs: {}",
                    unmirrored.len(),
                    unmirrored
                        .iter()
                        .map(|b| format!("{b:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        ));
    }

    // Strict turns every actionable warning into a failure so a CI/precommit
    // gate exits non-zero. Checks that are already Ok (e.g. an absent CLAUDE.md)
    // stay Ok — strict raises the floor, it does not invent new requirements.
    if strict {
        for check in &mut checks {
            if check.status == DoctorStatus::Warning {
                check.status = DoctorStatus::Fail;
            }
        }
    }

    let serious = checks
        .iter()
        .any(|check| check.status == DoctorStatus::Fail);
    DoctorReport {
        serious,
        strict,
        checks,
    }
}

/// Substring the agent docs must contain to prove they still route work through
/// the context preflight. Deliberately the command stem, not a full sentence, so
/// wording around it can vary.
const PREFLIGHT_MARKER: &str = "toimija context";

/// Concatenated bodies of the docs that form the agent contract: AGENTS.md,
/// CLAUDE.md, and whatever `important_docs` nominates as load-bearing. Unreadable
/// or absent files contribute nothing rather than erroring — a missing AGENTS.md
/// is already caught by its own check.
fn read_canonical_docs(repo_root: &Path, config: &RepoConfig) -> String {
    let mut names = vec![PathBuf::from("AGENTS.md"), PathBuf::from("CLAUDE.md")];
    names.extend(config.context.important_docs.iter().cloned());
    names
        .iter()
        .filter_map(|name| std::fs::read_to_string(repo_root.join(name)).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drift test for one boundary: does its normalized form still appear somewhere
/// in the normalized doc text? Normalization (lowercase, whitespace collapsed,
/// trailing sentence punctuation trimmed) absorbs casing, re-wrapping, and a
/// stray full stop while still failing when the sentence is genuinely gone. This
/// is intentionally lexical, not semantic — a boring drift guard, not a model.
fn boundary_present(boundary: &str, docs_text: &str) -> bool {
    let needle = normalize(boundary);
    !needle.is_empty() && normalize(docs_text).contains(&needle)
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .trim_end_matches(['.', ',', ';', ':', '!', '?'])
        .to_string()
}

fn join_paths(paths: &[&PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
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

    #[test]
    fn boundary_matches_through_casing_and_trailing_period() {
        let boundary = "Keep v0.0 as one Rust crate.";
        let docs = "# Agent guide\n\nkeep v0.0 as one rust crate at all times.\n";
        assert!(boundary_present(boundary, docs));
    }

    #[test]
    fn boundary_matches_through_line_wrapping() {
        // A boundary re-wrapped across lines in the doc still normalizes to the
        // same single-spaced form, so the drift guard does not fire on layout.
        let boundary = "Do not add network calls or background tasks.";
        let docs = "Do not add network\ncalls or background\ntasks for now.";
        assert!(boundary_present(boundary, docs));
    }

    #[test]
    fn missing_boundary_is_detected() {
        let boundary = "Keep v0.0 as one Rust crate.";
        let docs = "# Agent guide\n\nRun the tests before committing.\n";
        assert!(!boundary_present(boundary, docs));
    }

    #[test]
    fn blank_boundary_never_counts_as_present() {
        // A whitespace-only boundary normalizes to "" — guard against it
        // trivially matching every document.
        assert!(!boundary_present("   ", "any document text"));
    }
}
