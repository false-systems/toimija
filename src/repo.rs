use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::RepoConfig;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RepoKind {
    Rust,
    Elixir,
    // Minimum-pack languages: data rows in `adapters::pack`, not modules.
    Go,
    Python,
    Node,
    Terraform,
    Generic,
}

impl RepoKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Elixir => "Elixir",
            Self::Go => "Go",
            Self::Python => "Python",
            Self::Node => "JS/TS",
            Self::Terraform => "Terraform",
            Self::Generic => "Generic",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedLanguage {
    pub kind: RepoKind,
    #[serde(with = "crate::path_codec::one")]
    pub root: PathBuf,
    #[serde(with = "crate::path_codec::one")]
    pub manifest: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SecondaryScan {
    pub languages: Vec<DetectedLanguage>,
    pub truncated: bool,
    pub incomplete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoInfo {
    pub name: String,
    pub role: Option<String>,
    pub not_roles: Vec<String>,
    pub important_docs: Vec<PathBuf>,
    pub boundaries: Vec<String>,
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
    crate::adapters::detect(repo_root)
}

const SECONDARY_MAX_DEPTH: usize = 3;
const SECONDARY_ENTRY_BUDGET: usize = 4096;
const SECONDARY_HARD_SKIPS: &[&str] =
    &[".git", "node_modules", "target", "vendor", "_build", "deps"];

/// Find recognized manifests outside the primary language without turning
/// secondary observations into validation claims. Paths come only from
/// `read_dir` child names, symlinks are never followed, and both depth and
/// visited entries are capped.
pub fn detect_secondary_langs(
    repo_root: &Path,
    primary: RepoKind,
    is_excluded: impl Fn(&Path) -> bool,
) -> SecondaryScan {
    detect_secondary_langs_with_budget(repo_root, primary, is_excluded, SECONDARY_ENTRY_BUDGET)
}

fn detect_secondary_langs_with_budget(
    repo_root: &Path,
    primary: RepoKind,
    is_excluded: impl Fn(&Path) -> bool,
    entry_budget: usize,
) -> SecondaryScan {
    let mut scan = SecondaryScan::default();
    let mut queue = VecDeque::from([(PathBuf::new(), 0)]);
    let mut visited = 0;

    'walk: while let Some((relative, depth)) = queue.pop_front() {
        for (kind, manifest) in manifest_claims(repo_root, &relative) {
            push_language(&mut scan, primary, kind, &relative, manifest);
        }
        let entries = match std::fs::read_dir(repo_root.join(&relative)) {
            Ok(entries) => entries,
            Err(_) => {
                scan.incomplete = true;
                continue;
            }
        };
        for entry in entries {
            if visited == entry_budget {
                scan.truncated = true;
                break 'walk;
            }
            visited += 1;
            let Ok(entry) = entry else {
                scan.incomplete = true;
                continue;
            };
            let name = entry.file_name();
            let child = relative.join(&name);
            if SECONDARY_HARD_SKIPS
                .iter()
                .any(|skip| name == std::ffi::OsStr::new(skip))
                || is_excluded(&child)
            {
                continue;
            }
            match std::fs::symlink_metadata(repo_root.join(&child)) {
                Ok(metadata) if metadata.file_type().is_dir() && depth < SECONDARY_MAX_DEPTH => {
                    queue.push_back((child, depth + 1));
                }
                Ok(metadata) if metadata.file_type().is_file() => {
                    let Some(extension) = child.extension().and_then(|ext| ext.to_str()) else {
                        continue;
                    };
                    for language in crate::adapters::pack::LANGUAGES {
                        if language.root_extensions.contains(&extension) {
                            push_language(
                                &mut scan,
                                primary,
                                language.kind,
                                &relative,
                                child.clone(),
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => scan.incomplete = true,
            }
        }
    }

    scan.languages.sort_by(|left, right| {
        left.root
            .cmp(&right.root)
            .then(kind_rank(left.kind).cmp(&kind_rank(right.kind)))
    });
    scan
}

fn manifest_claims(repo_root: &Path, relative: &Path) -> Vec<(RepoKind, PathBuf)> {
    let mut claims = Vec::new();
    for language in crate::adapters::pack::LANGUAGES {
        if let Some(manifest) = language.manifests.iter().find_map(|manifest| {
            let relative_manifest = relative.join(manifest);
            std::fs::symlink_metadata(repo_root.join(&relative_manifest))
                .ok()
                .filter(|metadata| metadata.file_type().is_file())
                .map(|_| relative_manifest)
        }) {
            claims.push((language.kind, manifest));
        }
    }
    claims
}

fn push_language(
    scan: &mut SecondaryScan,
    primary: RepoKind,
    kind: RepoKind,
    relative: &Path,
    manifest: PathBuf,
) {
    if kind == primary {
        return;
    }
    let root = if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative.to_path_buf()
    };
    if let Some(existing) = scan
        .languages
        .iter_mut()
        .find(|language| language.kind == kind && language.root == root)
    {
        if manifest < existing.manifest {
            existing.manifest = manifest;
        }
    } else {
        scan.languages.push(DetectedLanguage {
            kind,
            root,
            manifest,
        });
    }
}

fn kind_rank(kind: RepoKind) -> usize {
    crate::adapters::pack::LANGUAGES
        .iter()
        .position(|pack| pack.kind == kind)
        .unwrap_or(usize::MAX)
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
    fn secondary_walk_is_bounded_and_suppresses_primary_kind() {
        let temp = tempfile::tempdir().unwrap();
        for path in [
            "services/api",
            "infra",
            "xtask",
            "node_modules/pkg",
            "a/b/c/d",
        ] {
            std::fs::create_dir_all(temp.path().join(path)).unwrap();
        }
        std::fs::write(temp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(temp.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(temp.path().join("services/api/go.mod"), "module demo\n").unwrap();
        std::fs::write(temp.path().join("infra/network.tf"), "").unwrap();
        std::fs::write(temp.path().join("xtask/Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(temp.path().join("node_modules/pkg/package.json"), "{}\n").unwrap();
        std::fs::write(temp.path().join("a/b/c/d/mix.exs"), "[]\n").unwrap();

        let scan = detect_secondary_langs(temp.path(), RepoKind::Rust, |_| false);
        assert_eq!(
            scan.languages,
            vec![
                DetectedLanguage {
                    kind: RepoKind::Node,
                    root: PathBuf::from("."),
                    manifest: PathBuf::from("package.json"),
                },
                DetectedLanguage {
                    kind: RepoKind::Terraform,
                    root: PathBuf::from("infra"),
                    manifest: PathBuf::from("infra/network.tf"),
                },
                DetectedLanguage {
                    kind: RepoKind::Go,
                    root: PathBuf::from("services/api"),
                    manifest: PathBuf::from("services/api/go.mod"),
                },
            ]
        );
        assert!(!scan.truncated);
    }

    #[test]
    fn secondary_walk_honors_configured_exclusions() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("fixtures/elixir_repo")).unwrap();
        std::fs::write(temp.path().join("fixtures/elixir_repo/mix.exs"), "[]\n").unwrap();

        let scan = detect_secondary_langs(temp.path(), RepoKind::Generic, |path| {
            path.starts_with("fixtures")
        });
        assert!(scan.languages.is_empty());
    }

    #[test]
    fn secondary_walk_reports_entry_budget_exhaustion() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("one"), "").unwrap();
        std::fs::write(temp.path().join("two"), "").unwrap();

        let scan = detect_secondary_langs_with_budget(temp.path(), RepoKind::Generic, |_| false, 1);
        assert!(scan.truncated);
    }

    #[test]
    fn secondary_extension_detection_obeys_entry_budget() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("network.tf"), "").unwrap();

        let scan = detect_secondary_langs_with_budget(temp.path(), RepoKind::Generic, |_| false, 0);
        assert!(scan.truncated);
        assert!(scan.languages.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn secondary_walk_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("go.mod"), "module outside\n").unwrap();
        symlink(outside.path(), temp.path().join("linked")).unwrap();

        let scan = detect_secondary_langs(temp.path(), RepoKind::Generic, |_| false);
        assert!(scan.languages.is_empty());
    }
}
