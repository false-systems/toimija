use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::RepoConfig;
use crate::repo::RepoKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationPlan {
    pub source: ValidationSource,
    /// Commands derived from the changed surface (e.g. `cargo test -p <crate>`
    /// for the crates that actually changed). Advisory and additive — a fast
    /// first pass, never a substitute for the authoritative tiers below. Always
    /// empty when `validation_plan` is built, then enriched once the changed
    /// surface is known (see `context::build_context_packet`).
    pub focused: Vec<String>,
    pub fast: Vec<String>,
    pub strict: Vec<String>,
    pub full: Vec<String>,
    pub privileged: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSource {
    RepoConfig,
    Inferred,
    InferredAndRepoConfig,
    Unknown,
}

pub fn validation_plan(repo_root: &Path, config: &RepoConfig, kind: RepoKind) -> ValidationPlan {
    if config.has_validation() && (config.validation.replace_inferred || kind == RepoKind::Generic)
    {
        return configured_plan(config, config.validation.replace_inferred);
    }
    let mut inferred = inferred_plan(repo_root, kind);
    if !config.has_validation() {
        return inferred;
    }
    extend_unique(&mut inferred.fast, &config.validation.fast);
    extend_unique(&mut inferred.strict, &config.validation.strict);
    extend_unique(&mut inferred.full, &config.validation.full);
    extend_unique(&mut inferred.privileged, &config.validation.privileged);
    inferred.source = ValidationSource::InferredAndRepoConfig;
    inferred
}

fn inferred_plan(repo_root: &Path, kind: RepoKind) -> ValidationPlan {
    if kind == RepoKind::Generic {
        return ValidationPlan {
            source: ValidationSource::Unknown,
            focused: Vec::new(),
            fast: Vec::new(),
            strict: Vec::new(),
            full: Vec::new(),
            privileged: Vec::new(),
            notes: vec!["validation unavailable for generic repo without repo config".to_string()],
        };
    }

    let Some(pack) = crate::adapters::pack::for_kind(kind) else {
        // A recognized kind without a registry row is a wiring bug; degrade
        // honestly to the generic (unknown) plan.
        return inferred_plan(repo_root, RepoKind::Generic);
    };
    info!(event = "toimija.validation.inferred", repo_kind = pack.name);
    let mut notes = vec![match pack.tier {
        crate::adapters::pack::LanguageTier::Full => format!(
            "Validation inferred from {} repo shape, not proven by Sykli",
            pack.name
        ),
        crate::adapters::pack::LanguageTier::Minimum => format!(
            "Validation inferred from {} repo shape (minimum language pack), not proven by Sykli",
            pack.name
        ),
    }];
    let commands = crate::adapters::pack::proven_commands(repo_root, pack);
    notes.extend(commands.notes);
    ValidationPlan {
        source: ValidationSource::Inferred,
        focused: Vec::new(),
        full: commands.fast.clone(),
        fast: commands.fast,
        strict: commands.strict,
        privileged: Vec::new(),
        notes,
    }
}

fn configured_plan(config: &RepoConfig, replaces_inferred: bool) -> ValidationPlan {
    ValidationPlan {
        source: ValidationSource::RepoConfig,
        focused: Vec::new(),
        fast: config.validation.fast.clone(),
        strict: config.validation.strict.clone(),
        full: config.validation.full.clone(),
        privileged: config.validation.privileged.clone(),
        notes: replaces_inferred
            .then(|| "Repository config explicitly replaces inferred validation.".to_string())
            .into_iter()
            .collect(),
    }
}

fn extend_unique(target: &mut Vec<String>, additions: &[String]) {
    for command in additions {
        if !target.contains(command) {
            target.push(command.clone());
        }
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
    fn repository_validation_is_additive_and_deduplicated() {
        let mut config = RepoConfig::default();
        config.validation.fast = vec![
            "cargo test --workspace".to_string(),
            "just test-fast".to_string(),
        ];
        config.validation.privileged = vec!["just integration".to_string()];

        let plan = validation_plan(Path::new("."), &config, RepoKind::Rust);

        assert_eq!(plan.source, ValidationSource::InferredAndRepoConfig);
        assert_eq!(
            serde_json::to_value(&plan).unwrap()["source"],
            "inferred_and_repo_config"
        );
        assert_eq!(plan.fast, vec!["cargo test --workspace", "just test-fast"]);
        assert!(plan.strict.iter().any(|cmd| cmd.contains("cargo clippy")));
        assert_eq!(plan.privileged, vec!["just integration"]);
    }

    #[test]
    fn explicit_replacement_omits_inferred_validation() {
        let mut config = RepoConfig::default();
        config.validation.replace_inferred = true;
        config.validation.fast = vec!["just test".to_string()];

        let plan = validation_plan(Path::new("."), &config, RepoKind::Rust);

        assert_eq!(plan.source, ValidationSource::RepoConfig);
        assert_eq!(
            serde_json::to_value(&plan).unwrap()["source"],
            "repo_config"
        );
        assert_eq!(plan.fast, vec!["just test"]);
        assert!(plan.strict.is_empty());
        assert!(plan.notes[0].contains("explicitly replaces"));
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
