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

    fn focused_validation(&self, repo_root: &Path, changed: &[PathBuf]) -> Vec<String> {
        // A focused run only earns its place in an umbrella: a single-app mix
        // repo has one mix.exs at the root, so every change maps to the whole
        // app and `mix test` already is the focused run. Stay quiet otherwise —
        // the same "no useful scoping → emit nothing" discipline as Rust.
        if !is_umbrella(repo_root) {
            return Vec::new();
        }
        let mut apps: Vec<String> = changed
            .iter()
            .filter(|path| is_elixir_source(path))
            .filter_map(|path| owning_app(repo_root, path))
            .collect();
        apps.sort();
        apps.dedup();
        if apps.is_empty() || apps.len() > FOCUSED_APP_CAP {
            return Vec::new();
        }
        apps.into_iter()
            .map(|app| format!("mix test apps/{app}/test"))
            .collect()
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
        elixir_test_candidates(path)
            .into_iter()
            .filter(|candidate| repo_root.join(candidate).exists())
            .collect()
    }
}

/// Distinct umbrella apps allowed in the focused tier before it defers to the
/// full suite. Past this, per-app runs no longer beat one whole-repo `mix test`.
const FOCUSED_APP_CAP: usize = 4;

fn is_elixir_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("ex" | "exs")
    )
}

/// An umbrella is an `apps/` directory holding at least one child with its own
/// `mix.exs`. Pure filesystem — no `mix` execution, no parsing.
fn is_umbrella(repo_root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(repo_root.join("apps")) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|entry| entry.path().join("mix.exs").is_file())
}

/// The owning umbrella app for a changed file: the `<name>` in `apps/<name>/…`,
/// confirmed by an `apps/<name>/mix.exs`. The directory *is* the unit, and
/// `mix test` takes a path, so we never need the app atom from `mix.exs` — no
/// parsing, just path components plus a filesystem check.
fn owning_app(repo_root: &Path, path: &Path) -> Option<String> {
    let mut components = path.components();
    if components.next()?.as_os_str() != "apps" {
        return None;
    }
    let app = components.next()?.as_os_str().to_str()?.to_string();
    repo_root
        .join("apps")
        .join(&app)
        .join("mix.exs")
        .is_file()
        .then_some(app)
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

    fn write_app(root: &Path, name: &str) {
        let app = root.join("apps").join(name);
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("mix.exs"), "").unwrap();
    }

    #[test]
    fn focused_maps_changed_source_to_owning_umbrella_app() {
        let temp = tempfile::tempdir().unwrap();
        write_app(temp.path(), "accounts");
        write_app(temp.path(), "billing");

        let changed = vec![
            PathBuf::from("apps/accounts/lib/accounts.ex"),
            PathBuf::from("apps/accounts/test/accounts_test.exs"),
            PathBuf::from("apps/billing/lib/billing.ex"),
            PathBuf::from("config/config.exs"), // umbrella root, not an app
        ];
        let focused = ElixirAdapter.focused_validation(temp.path(), &changed);

        // One path-scoped command per owning app, deduped and sorted; the root
        // config maps to no app and is dropped.
        assert_eq!(
            focused,
            vec![
                "mix test apps/accounts/test".to_string(),
                "mix test apps/billing/test".to_string(),
            ]
        );
    }

    #[test]
    fn focused_is_empty_for_single_app_repo() {
        let temp = tempfile::tempdir().unwrap();
        // No `apps/` dir: a plain mix project. `mix test` already is the run.
        std::fs::write(temp.path().join("mix.exs"), "").unwrap();

        let changed = vec![PathBuf::from("lib/foo.ex")];
        assert!(
            ElixirAdapter
                .focused_validation(temp.path(), &changed)
                .is_empty()
        );
    }

    #[test]
    fn focused_caps_and_falls_back_to_full_suite() {
        let temp = tempfile::tempdir().unwrap();
        let changed: Vec<PathBuf> = (0..=FOCUSED_APP_CAP)
            .map(|i| {
                let name = format!("app{i}");
                write_app(temp.path(), &name);
                PathBuf::from("apps").join(&name).join("lib/mod.ex")
            })
            .collect();

        // More distinct apps than the cap → no focused tier; whole-repo stands.
        assert!(
            ElixirAdapter
                .focused_validation(temp.path(), &changed)
                .is_empty()
        );
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
