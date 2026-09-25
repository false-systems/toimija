use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::ToimijaError;

/// Default UTF-8 byte budget for the Markdown context packet. Sized so the full
/// relevance layer (Scope, Read order, Coupling, Risk) survives on a real-repo
/// packet; a tighter packet stays well under it. Raised from 4000 when the
/// relevance layer (#12) enlarged the packet (#23).
pub const DEFAULT_BUDGET: usize = 8000;
pub const CONFIG_PATH: &str = ".toimija/repo.yaml";
pub const DAY_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RepoConfig {
    pub repo: RepoSection,
    pub context: ContextSection,
    pub validation: ValidationSection,
    pub stewardship: StewardshipSection,
    pub agents: BTreeMap<String, AgentPromptConfig>,
    pub boundaries: Vec<String>,
}

/// How a named agent CLI accepts the composed launch prompt (proposal 0006
/// Part C): `positional`, or a literal flag such as `--message`. Names resolve
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
    pub fast: Vec<String>,
    pub strict: Vec<String>,
    pub full: Vec<String>,
    pub privileged: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StewardshipSection {
    pub auto_sweep: bool,
    pub sessions_retention_days: u64,
    pub runs_retention_days: u64,
    pub history_max_records: usize,
}

impl Default for StewardshipSection {
    fn default() -> Self {
        Self {
            auto_sweep: true,
            sessions_retention_days: crate::sessions::DEFAULT_RETENTION_SECS / DAY_SECS,
            runs_retention_days: crate::footprint::DEFAULT_RUNS_RETENTION_SECS / DAY_SECS,
            history_max_records: crate::footprint::DEFAULT_HISTORY_MAX_RECORDS,
        }
    }
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
        config.validate_stewardship(&path)?;
        config.validate_agents(&path)?;
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

    fn validate_stewardship(&self, path: &Path) -> Result<(), ToimijaError> {
        for (name, value) in [
            (
                "sessions_retention_days",
                self.stewardship.sessions_retention_days,
            ),
            ("runs_retention_days", self.stewardship.runs_retention_days),
        ] {
            if value == 0 || value > u64::MAX / DAY_SECS {
                return Err(ToimijaError::InvalidStewardship {
                    path: path.to_path_buf(),
                    message: format!(
                        "stewardship.{name} must be between 1 and {}",
                        u64::MAX / DAY_SECS
                    ),
                });
            }
        }
        if self.stewardship.history_max_records == 0 {
            return Err(ToimijaError::InvalidStewardship {
                path: path.to_path_buf(),
                message: "stewardship.history_max_records must be greater than zero".to_string(),
            });
        }
        Ok(())
    }

    /// `agents:` entries teach `run` how a user-named CLI accepts the composed
    /// launch prompt: `positional`, or a flag starting with `-`. Anything else
    /// would build a broken argv at launch, so it is rejected here, named —
    /// the same fail-fast contract as the stewardship checks above.
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
}

/// Seed `.toimija/repo.yaml`, never overwriting one and never writing
/// through a symlink.
pub fn init_repo(repo_root: &Path) -> anyhow::Result<InitReport> {
    use anyhow::Context as _;
    let mut report = InitReport {
        created: Vec::new(),
        existing: Vec::new(),
        guidance: Vec::new(),
    };
    let dir = repo_root.join(crate::owned::TOIMIJA_DIR);
    if crate::owned::dir_exists(repo_root, &dir).context("failed to initialize")? {
        report.existing.push(dir.clone());
    } else {
        crate::owned::ensure_dir(repo_root, &dir).context("failed to initialize")?;
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
        use std::io::Write as _;
        crate::owned::open(
            repo_root,
            &config_path,
            fs::OpenOptions::new().write(true).create_new(true),
        )
        .and_then(|mut file| Ok(file.write_all(text.as_bytes())?))
        .context("failed to initialize")?;
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

    /// Repositories adopted before gates and runtime providers moved out
    /// still carry `gates:`, `gate_groups:` and `composition:`. Toimija no
    /// longer reads them, and must not refuse to load a config because they
    /// are there.
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
             composition:\n  work: teko\n  require_work: false\n",
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
        assert_eq!(config.stewardship, StewardshipSection::default());
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
    fn loads_stewardship_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".toimija");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("repo.yaml"),
            "stewardship:\n  auto_sweep: false\n  sessions_retention_days: 2\n  runs_retention_days: 3\n  history_max_records: 4\n",
        )
        .unwrap();

        let config = RepoConfig::load(temp.path()).unwrap();
        assert!(!config.stewardship.auto_sweep);
        assert_eq!(config.stewardship.sessions_retention_days, 2);
        assert_eq!(config.stewardship.runs_retention_days, 3);
        assert_eq!(config.stewardship.history_max_records, 4);
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

    #[test]
    fn zero_stewardship_values_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".toimija");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("repo.yaml"),
            "stewardship:\n  sessions_retention_days: 0\n",
        )
        .unwrap();

        let error = RepoConfig::load(temp.path()).unwrap_err();
        assert!(matches!(error, ToimijaError::InvalidStewardship { .. }));
        assert!(error.to_string().contains("must be between 1 and"));
    }
}
