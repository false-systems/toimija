pub mod elixir;
pub mod generic;
pub mod rust;

use std::path::{Path, PathBuf};

use crate::config::RepoConfig;
use crate::git::ChangedFile;
use crate::repo::RepoKind;
use crate::validate::ValidationPlan;

pub trait LanguageAdapter {
    fn kind(&self) -> RepoKind;
    fn detect(&self, repo_root: &Path) -> bool;
    fn validation_plan(&self, repo_root: &Path, config: &RepoConfig) -> ValidationPlan;
    fn read_first_hints(&self, repo_root: &Path, changed: &[ChangedFile])
    -> Vec<(PathBuf, String)>;
    fn companion_tests(&self, _repo_root: &Path, _path: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
}
