use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::ToimijaError;

pub const DEFAULT_BUDGET: usize = 4000;
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

/// Build-output directories Toimija can safely omit from the changed surface
/// for a detected repo kind. Conservative: only well-known build dirs, nothing
/// that could hide real source. Empty for `Generic` — we do not guess.
fn default_safe_to_ignore(kind: crate::repo::RepoKind) -> Vec<PathBuf> {
    use crate::repo::RepoKind;
    match kind {
        RepoKind::Rust => vec![PathBuf::from("target/")],
        RepoKind::Elixir => vec![PathBuf::from("_build/"), PathBuf::from("deps/")],
        RepoKind::Generic => Vec::new(),
    }
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
        let mut config = RepoConfig::default_for(repo_root);
        // Seed safe_to_ignore with the detected kind's build dirs so a fresh
        // repo ignores the obvious noise without hand-tuning.
        config.context.safe_to_ignore =
            default_safe_to_ignore(crate::repo::detect_repo_kind(repo_root));
        let text = serde_yaml::to_string(&config).expect("config serializes");
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
    }

    let agents_path = repo_root.join("AGENTS.md");
    if agents_path.exists() {
        report.existing.push(agents_path);
    } else {
        fs::write(&agents_path, AGENTS_TEMPLATE).map_err(|source| ToimijaError::WriteFile {
            path: agents_path.clone(),
            source,
        })?;
        report.created.push(agents_path);
    }

    let claude_path = repo_root.join("CLAUDE.md");
    if claude_path.exists() {
        report.existing.push(claude_path);
    } else {
        fs::write(&claude_path, CLAUDE_TEMPLATE).map_err(|source| ToimijaError::WriteFile {
            path: claude_path.clone(),
            source,
        })?;
        report.created.push(claude_path);
    }

    Ok(report)
}

pub const AGENTS_TEMPLATE: &str = r#"# Agent guide

This repo supports Toimija.

Before making changes, run:

```bash
toimija context --for-diff --budget 4000
```

Before claiming completion, run:

```bash
toimija validate --plan
```

Use `.toimija/repo.yaml` for repo boundaries and validation hints.
"#;

pub const CLAUDE_TEMPLATE: &str = r#"@AGENTS.md

## Claude Code notes

Work in small changes.
Do not add MCP, Tree-sitter, LSP, SCIP, Sykli, Vaisto, Kerto, or Terraform support unless a human explicitly moves it into scope.
Run formatting, tests, and clippy before claiming completion.
"#;

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
