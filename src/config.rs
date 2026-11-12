use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::ToimijaError;

/// Default UTF-8 byte budget for the Markdown context packet, when repo.yaml
/// names none. A real repository's packet stays well under it.
pub const DEFAULT_BUDGET: usize = 8000;
pub const CONFIG_PATH: &str = ".toimija/repo.yaml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RepoConfig {
    pub repo: RepoSection,
    pub context: ContextSection,
    pub validation: ValidationSection,
    pub agents: BTreeMap<String, AgentPromptConfig>,
    pub boundaries: Vec<String>,
}

/// How a named agent CLI accepts the composed launch prompt:
/// `positional`, or a literal flag such as `--message`. Names resolve
/// by basename at launch; the executable itself is always named by the user
/// after `--` — configuration describes delivery, never paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPromptConfig {
    pub prompt: String,
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
    pub replace_inferred: bool,
    /// The repository's declared gate (`sykli.json`). `replace_inferred`
    /// drops inferred commands, never the gate; only this setting changes it.
    #[serde(skip_serializing_if = "GateSetting::is_default")]
    pub gate: GateSetting,
    pub fast: Vec<String>,
    pub strict: Vec<String>,
    pub full: Vec<String>,
    pub privileged: Vec<String>,
}

/// `validation.gate` in `.toimija/repo.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum GateSetting {
    /// `true` (the default) names the gate of a repository with a
    /// `sykli.json`; `false` names none.
    Enabled(bool),
    /// The command to name for that gate instead of the standard one, for a
    /// repository that runs sykli another way (`mise exec -- sykli …`).
    Command(String),
}

impl Default for GateSetting {
    fn default() -> Self {
        Self::Enabled(true)
    }
}

impl GateSetting {
    fn is_default(&self) -> bool {
        *self == Self::default()
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
        config.validate_agents(&path)?;
        config.validate_gate(&path)?;
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
        self.validation.replace_inferred
            || !self.validation.fast.is_empty()
            || !self.validation.strict.is_empty()
            || !self.validation.full.is_empty()
            || !self.validation.privileged.is_empty()
    }

    /// `agents:` entries teach `run` how a user-named CLI accepts the composed
    /// launch prompt: `positional`, or a flag starting with `-`. Anything else
    /// would build a broken argv at launch, so it is rejected here, named —
    /// the same fail-fast contract as a file that does not parse.
    fn validate_agents(&self, path: &Path) -> Result<(), ToimijaError> {
        for (name, agent) in &self.agents {
            if let Err(message) = crate::adapters::agents::PromptStyle::from_config(&agent.prompt) {
                return Err(ToimijaError::InvalidAgents {
                    path: path.to_path_buf(),
                    message: format!("agents.{name}: {message}"),
                });
            }
        }
        Ok(())
    }

    /// A blank `validation.gate` command would be named as the gate: an empty
    /// line for the agent to run. `true`, `false` or a command, nothing else.
    fn validate_gate(&self, path: &Path) -> Result<(), ToimijaError> {
        match &self.validation.gate {
            GateSetting::Command(command) if command.trim().is_empty() => {
                Err(ToimijaError::InvalidValidation {
                    path: path.to_path_buf(),
                    message: "validation.gate is blank: give the gate command, or `true` for \
                              the standard gate, or `false` for none"
                        .to_string(),
                })
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration may carry sections an earlier toimija read (`gates:`,
    /// `stewardship:`, `specs:`). Toimija does not read them, and must not
    /// refuse to load a config because they are there.
    #[test]
    fn a_config_with_retired_sections_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".toimija");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("repo.yaml"),
            "repo:\n  name: legacy\n\
             gates:\n  rust-test:\n    program: cargo\n    args: [test]\n\
             gate_groups:\n  full:\n    gates: [rust-test]\n\
             composition:\n  work: widget\n  require_work: false\n\
             stewardship:\n  auto_sweep: false\n  sessions_retention_days: 0\n\
             specs: docs/adr\n",
        )
        .unwrap();
        let config = RepoConfig::load(temp.path()).unwrap();
        assert_eq!(config.repo.name, "legacy");
    }

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
        assert!(!config.validation.replace_inferred);
        assert!(config.agents.is_empty());
    }

    #[test]
    fn loads_and_validates_configured_agents() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".toimija");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("repo.yaml"),
            "repo:\n  name: demo\n  role: local tool\nagents:\n  aider:\n    prompt: --message\n  opencode:\n    prompt: positional\n",
        )
        .unwrap();

        let config = RepoConfig::load(temp.path()).unwrap();
        assert_eq!(
            config
                .agents
                .get("aider")
                .map(|agent| agent.prompt.as_str()),
            Some("--message")
        );
        assert_eq!(
            config
                .agents
                .get("opencode")
                .map(|agent| agent.prompt.as_str()),
            Some("positional")
        );

        // Anything that is not `positional` or a `-flag` would build a broken
        // argv at launch: rejected at load, named.
        std::fs::write(
            dir.join("repo.yaml"),
            "repo:\n  name: demo\nagents:\n  aider:\n    prompt: message\n",
        )
        .unwrap();
        let error = RepoConfig::load(temp.path()).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("agents.aider"), "{rendered}");
        assert!(rendered.contains("`--message`"), "{rendered}");
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
