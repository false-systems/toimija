use std::path::{Path, PathBuf};

use crate::adapters::LanguageAdapter;
use crate::config::RepoConfig;
use crate::git::ChangedFile;
use crate::repo::RepoKind;
use crate::validate::{self, ValidationPlan};

#[derive(Debug, Clone, Copy)]
pub struct GenericAdapter;

impl LanguageAdapter for GenericAdapter {
    fn kind(&self) -> RepoKind {
        RepoKind::Generic
    }

    fn detect(&self, _repo_root: &Path) -> bool {
        true
    }

    fn validation_plan(&self, repo_root: &Path, config: &RepoConfig) -> ValidationPlan {
        validate::validation_plan(repo_root, config, RepoKind::Generic)
    }

    fn read_first_hints(
        &self,
        _repo_root: &Path,
        _changed: &[ChangedFile],
    ) -> Vec<(PathBuf, String)> {
        Vec::new()
    }
}
