use std::path::{Path, PathBuf};

use crate::adapters::LanguageAdapter;
use crate::config::RepoConfig;
use crate::git::ChangedFile;
use crate::repo::RepoKind;
use crate::validate::{self, ValidationPlan};

#[derive(Debug, Clone, Copy)]
pub struct ElixirAdapter;

impl LanguageAdapter for ElixirAdapter {
    fn kind(&self) -> RepoKind {
        RepoKind::Elixir
    }

    fn detect(&self, repo_root: &Path) -> bool {
        repo_root.join("mix.exs").exists()
    }

    fn validation_plan(&self, repo_root: &Path, config: &RepoConfig) -> ValidationPlan {
        validate::validation_plan(repo_root, config, RepoKind::Elixir)
    }

    fn read_first_hints(
        &self,
        repo_root: &Path,
        changed: &[ChangedFile],
    ) -> Vec<(PathBuf, String)> {
        let mut hints = Vec::new();
        for file in changed {
            let path = &file.path;
            for candidate in elixir_test_candidates(path) {
                if repo_root.join(&candidate).exists() {
                    hints.push((candidate, "matching test heuristic".to_string()));
                }
            }
        }
        hints.sort_by(|left, right| left.0.cmp(&right.0));
        hints.dedup_by(|left, right| left.0 == right.0);
        hints
    }
}

fn elixir_test_candidates(path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if path == Path::new("mix.exs") {
        candidates.push(PathBuf::from("test/test_helper.exs"));
        return candidates;
    }
    if path.extension().and_then(|ext| ext.to_str()) != Some("ex") {
        return candidates;
    }
    let Ok(rest) = path.strip_prefix("lib") else {
        return candidates;
    };

    let mut test_path = PathBuf::from("test").join(rest);
    test_path.set_extension("exs");
    if let Some(stem) = test_path.file_stem().and_then(|stem| stem.to_str()) {
        test_path.set_file_name(format!("{stem}_test.exs"));
    }
    candidates.push(test_path);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{ChangeStatus, ChangedFile};

    #[test]
    fn elixir_read_first_maps_lib_file_to_test_file() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("test/foo")).unwrap();
        std::fs::write(temp.path().join("test/foo/bar_test.exs"), "").unwrap();

        let changed = vec![ChangedFile {
            path: PathBuf::from("lib/foo/bar.ex"),
            status: ChangeStatus::Modified,
        }];
        let hints = ElixirAdapter.read_first_hints(temp.path(), &changed);
        assert_eq!(hints[0].0, PathBuf::from("test/foo/bar_test.exs"));
    }

    #[test]
    fn elixir_read_first_includes_test_helper_for_mix_file() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("test")).unwrap();
        std::fs::write(temp.path().join("test/test_helper.exs"), "").unwrap();

        let changed = vec![ChangedFile {
            path: PathBuf::from("mix.exs"),
            status: ChangeStatus::Modified,
        }];
        let hints = ElixirAdapter.read_first_hints(temp.path(), &changed);
        assert_eq!(hints[0].0, PathBuf::from("test/test_helper.exs"));
    }
}
