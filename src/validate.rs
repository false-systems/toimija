use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::RepoConfig;
use crate::repo::RepoKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationPlan {
    pub source: ValidationSource,
    pub fast: Vec<String>,
    pub strict: Vec<String>,
    pub full: Vec<String>,
    pub privileged: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ValidationSource {
    RepoConfig,
    Inferred,
    Unknown,
}

pub fn validation_plan(_repo_root: &Path, config: &RepoConfig, kind: RepoKind) -> ValidationPlan {
    if config.has_validation() {
        return ValidationPlan {
            source: ValidationSource::RepoConfig,
            fast: config.validation.fast.clone(),
            strict: config.validation.strict.clone(),
            full: config.validation.full.clone(),
            privileged: config.validation.privileged.clone(),
            notes: Vec::new(),
        };
    }
    match kind {
        RepoKind::Rust => {
            info!(event = "toimija.validation.inferred", repo_kind = "rust");
            ValidationPlan {
                source: ValidationSource::Inferred,
                fast: vec!["cargo test --workspace".to_string()],
                strict: vec![
                    "cargo fmt --check".to_string(),
                    "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
                ],
                full: vec!["cargo test --workspace".to_string()],
                privileged: Vec::new(),
                notes: vec![
                    "validation inferred from Rust repo shape, not proven by Sykli".to_string(),
                ],
            }
        }
        RepoKind::Elixir => {
            info!(event = "toimija.validation.inferred", repo_kind = "elixir");
            ValidationPlan {
                source: ValidationSource::Inferred,
                fast: vec!["mix test".to_string()],
                strict: vec!["mix format --check-formatted".to_string()],
                full: vec!["mix test".to_string()],
                privileged: Vec::new(),
                notes: vec![
                    "validation inferred from Elixir repo shape, not proven by Sykli".to_string(),
                ],
            }
        }
        RepoKind::Generic => ValidationPlan {
            source: ValidationSource::Unknown,
            fast: Vec::new(),
            strict: Vec::new(),
            full: Vec::new(),
            privileged: Vec::new(),
            notes: vec!["validation unavailable for generic repo without repo config".to_string()],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_plan_from_config() {
        let mut config = RepoConfig::default();
        config.validation.fast = vec!["just test".to_string()];
        let plan = validation_plan(Path::new("."), &config, RepoKind::Generic);
        assert_eq!(plan.source, ValidationSource::RepoConfig);
        assert_eq!(plan.fast, vec!["just test"]);
    }

    #[test]
    fn validation_plan_inferred_for_rust() {
        let plan = validation_plan(Path::new("."), &RepoConfig::default(), RepoKind::Rust);
        assert_eq!(plan.source, ValidationSource::Inferred);
        assert!(plan.strict.iter().any(|cmd| cmd.contains("cargo clippy")));
    }

    #[test]
    fn validation_plan_inferred_for_elixir() {
        let plan = validation_plan(Path::new("."), &RepoConfig::default(), RepoKind::Elixir);
        assert_eq!(plan.source, ValidationSource::Inferred);
        assert!(plan.fast.iter().any(|cmd| cmd == "mix test"));
    }
}
