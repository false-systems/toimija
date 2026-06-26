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
    /// Commands that exercise only the changed surface — a fast first pass above
    /// the whole-repo tiers. Derived purely from changed paths plus on-disk
    /// manifest locations; never network or source parsing. Default: no focused
    /// tier, so a language adapter opts in rather than every kind paying for it.
    fn focused_validation(&self, _repo_root: &Path, _changed: &[PathBuf]) -> Vec<String> {
        Vec::new()
    }
    fn read_first_hints(&self, repo_root: &Path, changed: &[ChangedFile])
    -> Vec<(PathBuf, String)>;
    fn companion_tests(&self, _repo_root: &Path, _path: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
}
