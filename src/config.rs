use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::ToimijaError;

/// Default character budget for the Markdown context packet. Sized so the full
/// relevance layer (Scope, Read order, Coupling, Risk) survives on a real-repo
/// packet; a tighter packet stays well under it. Raised from 4000 when the
/// relevance layer (#12) enlarged the packet (#23).
pub const DEFAULT_BUDGET: usize = 8000;
pub const CONFIG_PATH: &str = ".toimija/repo.yaml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RepoConfig {
    pub repo: RepoSection,
    pub context: ContextSection,
    pub validation: ValidationSection,
    pub boundaries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct RepoSection {
    pub name: String,
    pub role: String,
    #[serde(rename = "not")]
    pub not_roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextSection {
    pub default_budget: usize,
    pub important_docs: Vec<PathBuf>,
    pub read_first: Vec<PathBuf>,
    pub safe_to_ignore: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ValidationSection {
    pub fast: Vec<String>,
    pub strict: Vec<String>,
    pub full: Vec<String>,
    pub privileged: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    pub created: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
    /// Next steps a human must take so the packet can orient an agent. Populated
    /// only when a fresh `repo.yaml` was written (its identity fields are blank).
    pub guidance: Vec<String>,
}

impl Default for ContextSection {
    fn default() -> Self {
        Self {
            default_budget: DEFAULT_BUDGET,
            important_docs: vec![PathBuf::from("README.md")],
            read_first: Vec::new(),
            safe_to_ignore: Vec::new(),
        }
    }
}

impl RepoConfig {
    pub fn load(repo_root: &Path) -> Result<Self, ToimijaError> {
        let path = repo_root.join(CONFIG_PATH);
        let text = fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ToimijaError::MissingConfig
            } else {
                ToimijaError::ReadConfig {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        let config: Self =
            serde_yaml::from_str(&text).map_err(|source| ToimijaError::InvalidConfig {
                path: path.clone(),
                source,
            })?;
        info!(event = "toimija.config.loaded", path = %path.display());
        Ok(config)
    }

    pub fn load_optional(repo_root: &Path) -> Result<Option<Self>, ToimijaError> {
        match Self::load(repo_root) {
            Ok(config) => Ok(Some(config)),
            Err(ToimijaError::MissingConfig) => {
                debug!(
                    event = "toimija.config.missing",
                    path = %repo_root.join(CONFIG_PATH).display()
                );
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn default_for(repo_root: &Path) -> Self {
        let mut config = Self::default();
        config.repo.name = repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        config
    }

    pub fn has_validation(&self) -> bool {
        !self.validation.fast.is_empty()
            || !self.validation.strict.is_empty()
            || !self.validation.full.is_empty()
            || !self.validation.privileged.is_empty()
    }
}

pub fn init_repo(repo_root: &Path) -> Result<InitReport, ToimijaError> {
    let mut report = InitReport {
        created: Vec::new(),
        existing: Vec::new(),
        guidance: Vec::new(),
    };
    let dir = repo_root.join(".toimija");
    if dir.exists() {
        report.existing.push(dir.clone());
    } else {
        fs::create_dir_all(&dir).map_err(|source| ToimijaError::WriteFile {
            path: dir.clone(),
            source,
        })?;
        report.created.push(dir.clone());
    }

    let config_path = repo_root.join(CONFIG_PATH);
    if config_path.exists() {
        report.existing.push(config_path);
    } else {
        // Build/vendor noise is filtered live at context time (see
        // BUILTIN_ARTIFACT_DIRS), so the fresh config is left clean —
        // safe_to_ignore is for the repo's own additions only.
        let text =
            serde_yaml::to_string(&RepoConfig::default_for(repo_root)).expect("config serializes");
        fs::write(&config_path, text).map_err(|source| ToimijaError::WriteFile {
            path: config_path.clone(),
            source,
        })?;
        report.created.push(config_path);
        // A fresh config has blank identity fields; point the human at them.
        report.guidance.push(
            "Fill role, not, and boundaries in .toimija/repo.yaml so the packet can orient an agent."
                .to_string(),
        );
        // Toimija owns `.toimija/`, not root agent policy. It suggests preflight
        // text for AGENTS.md/CLAUDE.md via `toimija snippet`, but never writes
        // those human-owned files itself.
        report.guidance.push(
            "Run `toimija snippet preflight` and paste it into your AGENTS.md/CLAUDE.md so agents run `toimija enter` first."
                .to_string(),
        );
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_config_with_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".toimija");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("repo.yaml"),
            "repo:\n  name: demo\n  role: local tool\n",
        )
        .unwrap();

        let config = RepoConfig::load(temp.path()).unwrap();
        assert_eq!(config.repo.name, "demo");
        assert_eq!(config.context.default_budget, DEFAULT_BUDGET);
        assert_eq!(
            config.context.important_docs,
            vec![PathBuf::from("README.md")]
        );
        assert!(config.context.read_first.is_empty());
        assert!(config.context.safe_to_ignore.is_empty());
    }

    #[test]
    fn loads_config_context_rules() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".toimija");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("repo.yaml"),
            "repo:\n  name: demo\ncontext:\n  read_first:\n    - docs/architecture.md\n  safe_to_ignore:\n    - target/\n    - _build/\n",
        )
        .unwrap();

        let config = RepoConfig::load(temp.path()).unwrap();
        assert_eq!(
            config.context.read_first,
            vec![PathBuf::from("docs/architecture.md")]
        );
        assert_eq!(
            config.context.safe_to_ignore,
            vec![PathBuf::from("target/"), PathBuf::from("_build/")]
        );
    }

    #[test]
    fn invalid_yaml_is_actionable() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".toimija");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("repo.yaml"), "repo: [").unwrap();

        let error = RepoConfig::load(temp.path()).unwrap_err();
        assert!(matches!(error, ToimijaError::InvalidConfig { .. }));
    }
}
