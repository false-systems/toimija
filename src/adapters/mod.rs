pub mod agents;
pub mod elixir;
pub mod generic;
pub mod pack;
pub mod rust;

use std::path::{Path, PathBuf};

use crate::config::RepoConfig;
use crate::git::ChangedFile;
use crate::repo::RepoKind;
use crate::validate::ValidationPlan;

pub trait LanguageAdapter {
    fn kind(&self) -> RepoKind;
    fn validation_plan(&self, repo_root: &Path, config: &RepoConfig) -> ValidationPlan {
        crate::validate::validation_plan(repo_root, config, self.kind())
    }
    /// Commands that exercise only the changed surface — a fast first pass above
    /// the whole-repo tiers. Derived purely from changed paths plus on-disk
    /// manifest locations; never network or source parsing. Default: no focused
    /// tier, so a language adapter opts in rather than every kind paying for it.
    fn focused_validation(&self, _repo_root: &Path, _changed: &[PathBuf]) -> Vec<String> {
        Vec::new()
    }
    fn read_first_hints(
        &self,
        repo_root: &Path,
        changed: &[ChangedFile],
    ) -> Vec<(PathBuf, String)> {
        let mut hints = Vec::new();
        for file in changed {
            for candidate in self.companion_tests(repo_root, &file.path) {
                hints.push((candidate, "matching test heuristic".to_string()));
            }
        }
        hints.sort_by(|left, right| left.0.cmp(&right.0));
        hints.dedup_by(|left, right| left.0 == right.0);
        hints
    }
    fn companion_tests(&self, repo_root: &Path, path: &Path) -> Vec<PathBuf> {
        pack::for_kind(self.kind())
            .map(|pack| pack.companion_tests(repo_root, path))
            .unwrap_or_default()
    }
}

pub fn detect(repo_root: &Path) -> RepoKind {
    pack::detect(repo_root)
        .map(|pack| pack.kind)
        .unwrap_or(RepoKind::Generic)
}

pub fn for_kind(kind: RepoKind) -> Box<dyn LanguageAdapter> {
    match kind {
        RepoKind::Rust => Box::new(rust::RustAdapter),
        RepoKind::Elixir => Box::new(elixir::ElixirAdapter),
        RepoKind::Generic => Box::new(generic::GenericAdapter),
        packed_kind => match pack::for_kind(packed_kind) {
            Some(pack) => Box::new(pack::PackAdapter(pack)),
            None => Box::new(generic::GenericAdapter),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_every_pack_and_preserves_smart_rust_behavior() {
        for pack in pack::LANGUAGES {
            assert_eq!(for_kind(pack.kind).kind(), pack.kind);
        }

        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        assert_eq!(
            for_kind(RepoKind::Rust)
                .focused_validation(temp.path(), &[PathBuf::from("src/lib.rs")]),
            vec!["cargo test -p demo"]
        );
    }
}
