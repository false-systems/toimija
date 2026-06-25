use std::path::{Path, PathBuf};

use crate::adapters::LanguageAdapter;
use crate::config::RepoConfig;
use crate::git::ChangedFile;
use crate::repo::RepoKind;
use crate::validate::{self, ValidationPlan};

#[derive(Debug, Clone, Copy)]
pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn kind(&self) -> RepoKind {
        RepoKind::Rust
    }

    fn detect(&self, repo_root: &Path) -> bool {
        repo_root.join("Cargo.toml").exists()
    }

    fn validation_plan(&self, repo_root: &Path, config: &RepoConfig) -> ValidationPlan {
        validate::validation_plan(repo_root, config, RepoKind::Rust)
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
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            return Vec::new();
        }
        rust_test_candidates(path)
            .into_iter()
            .filter(|candidate| repo_root.join(candidate).exists())
            .collect()
    }
}

fn rust_test_candidates(path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return candidates;
    };

    candidates.push(PathBuf::from("tests").join(format!("{stem}.rs")));
    candidates.push(PathBuf::from("tests").join(format!("{stem}_test.rs")));

    if let Ok(src_relative) = path.strip_prefix("src") {
        candidates.push(PathBuf::from("tests").join(src_relative));

        if stem == "mod"
            && let Some(parent) = src_relative.parent()
            && let Some(module) = parent.file_name().and_then(|name| name.to_str())
        {
            candidates.push(PathBuf::from("tests").join(format!("{module}.rs")));
            candidates.push(PathBuf::from("tests").join(format!("{module}_test.rs")));
        }

        let mut sibling_tests = PathBuf::from("src").join(src_relative);
        sibling_tests.set_file_name("tests.rs");
        candidates.push(sibling_tests);

        let mut sibling_test = PathBuf::from("src").join(src_relative);
        sibling_test.set_file_name(format!("{stem}_test.rs"));
        candidates.push(sibling_test);
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{ChangeStatus, ChangedFile};

    #[test]
    fn rust_read_first_finds_integration_and_sibling_tests() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("tests/foo")).unwrap();
        std::fs::create_dir_all(temp.path().join("src/foo")).unwrap();
        std::fs::write(temp.path().join("tests/foo.rs"), "").unwrap();
        std::fs::write(temp.path().join("tests/foo/bar.rs"), "").unwrap();
        std::fs::write(temp.path().join("src/foo/bar_test.rs"), "").unwrap();

        let changed = vec![
            ChangedFile {
                path: PathBuf::from("src/foo.rs"),
                status: ChangeStatus::Modified,
            },
            ChangedFile {
                path: PathBuf::from("src/foo/bar.rs"),
                status: ChangeStatus::Modified,
            },
        ];
        let hints = RustAdapter.read_first_hints(temp.path(), &changed);
        assert_eq!(
            hints
                .iter()
                .map(|(path, _)| path.as_path())
                .collect::<Vec<_>>(),
            vec![
                Path::new("src/foo/bar_test.rs"),
                Path::new("tests/foo/bar.rs"),
                Path::new("tests/foo.rs")
            ]
        );
    }

    #[test]
    fn rust_mod_file_maps_to_module_integration_test() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("tests")).unwrap();
        std::fs::write(temp.path().join("tests/foo_test.rs"), "").unwrap();

        let changed = vec![ChangedFile {
            path: PathBuf::from("src/foo/mod.rs"),
            status: ChangeStatus::Modified,
        }];
        let hints = RustAdapter.read_first_hints(temp.path(), &changed);
        assert_eq!(hints[0].0, PathBuf::from("tests/foo_test.rs"));
    }
}
