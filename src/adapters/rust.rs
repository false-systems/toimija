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

    fn focused_validation(&self, repo_root: &Path, changed: &[PathBuf]) -> Vec<String> {
        // Only Rust source warrants a crate test. Manifest/doc/config churn maps
        // to no `cargo test -p`, which is itself honest: "no crate source
        // changed; the full suite below is the proof, not a focused run."
        let mut crates: Vec<String> = changed
            .iter()
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
            .filter_map(|path| owning_crate_name(repo_root, path))
            .collect();
        crates.sort();
        crates.dedup();
        // Cap and fall back: a change spanning many crates is broad enough that
        // per-crate commands stop saving time. Emit nothing and let the
        // whole-repo Fast tier stand as the authoritative run.
        if crates.is_empty() || crates.len() > FOCUSED_CRATE_CAP {
            return Vec::new();
        }
        crates
            .into_iter()
            .map(|name| format!("cargo test -p {name}"))
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
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            return Vec::new();
        }
        rust_test_candidates(path)
            .into_iter()
            .filter(|candidate| repo_root.join(candidate).exists())
            .collect()
    }
}

/// Distinct crates allowed in the focused tier before it defers to the full
/// suite. Past this, per-crate runs no longer beat one whole-repo run.
const FOCUSED_CRATE_CAP: usize = 4;

/// Walk from a changed file up to the repo root, returning the package name of
/// the nearest enclosing `Cargo.toml`. Pure filesystem: a manifest's location is
/// the crate boundary, and its `[package].name` is the `-p` argument we need.
/// `file` is repo-root-relative (e.g. `xtask/src/main.rs` → `xtask`).
fn owning_crate_name(repo_root: &Path, file: &Path) -> Option<String> {
    let mut current = file.parent();
    loop {
        let dir = current.unwrap_or_else(|| Path::new(""));
        let manifest = repo_root.join(dir).join("Cargo.toml");
        if manifest.is_file() {
            let contents = std::fs::read_to_string(&manifest).ok()?;
            return package_name(&contents);
        }
        // `Some("")` is the repo root; its parent is `None`, so one more loop
        // checked the root manifest above before we stop here.
        current = current?.parent();
    }
}

/// Extract the `name` of the `[package]` table from a `Cargo.toml`'s text.
///
/// We have no `toml` crate in the dependency tree (and the v0.0 boundary favours
/// staying a small, dependency-light CLI), so this is a deliberately minimal,
/// section-aware scan rather than a full parse. It must return the package name
/// ONLY when found inside the `[package]` table — a `name = ...` under
/// `[dependencies]`, `[[bin]]`, or any other table must NOT be mistaken for it.
///
/// Returns `None` for a manifest with no `[package]` table (e.g. a virtual
/// workspace root) — there is no crate to scope a `cargo test -p` to.
fn package_name(contents: &str) -> Option<String> {
    let mut in_package = false;
    for line in contents.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            // A new table starts; we only trust `name` inside `[package]`.
            // `[package]` and `[package.metadata...]` differ — match exactly.
            in_package = header.strip_suffix(']').map(str::trim) == Some("package");
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "name" {
            // Strip the surrounding quotes; ignore any trailing inline comment.
            let value = value.trim();
            let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            return value[1..].split(quote).next().map(str::to_string);
        }
    }
    None
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
                index_entry: None,
            },
            ChangedFile {
                path: PathBuf::from("src/foo/bar.rs"),
                status: ChangeStatus::Modified,
                index_entry: None,
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

    fn write_manifest(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn focused_maps_changed_source_to_owning_crate_test() {
        let temp = tempfile::tempdir().unwrap();
        write_manifest(temp.path(), "toimija");
        write_manifest(&temp.path().join("xtask"), "xtask");

        let changed = vec![
            PathBuf::from("src/adapters/rust.rs"),
            PathBuf::from("src/render.rs"),
            PathBuf::from("xtask/src/main.rs"),
        ];
        let focused = RustAdapter.focused_validation(temp.path(), &changed);

        // Deduped to one command per owning crate, sorted, scoped to `-p`.
        assert_eq!(
            focused,
            vec![
                "cargo test -p toimija".to_string(),
                "cargo test -p xtask".to_string(),
            ]
        );
    }

    #[test]
    fn focused_is_empty_when_only_non_source_changed() {
        let temp = tempfile::tempdir().unwrap();
        write_manifest(temp.path(), "toimija");

        // Manifest + docs only: no `.rs` churn, so no focused crate test — the
        // honest signal that the full suite is the proof, not a focused run.
        let changed = vec![PathBuf::from("README.md"), PathBuf::from("Cargo.toml")];
        assert!(
            RustAdapter
                .focused_validation(temp.path(), &changed)
                .is_empty()
        );
    }

    #[test]
    fn package_name_reads_only_the_package_table() {
        // `name` under `[dependencies]` must not be mistaken for the package.
        let manifest = "\
[package]
name = \"toimija\"  # the real one
version = \"0.0.0\"

[dependencies]
serde = { version = \"1\", features = [\"derive\"] }
name = \"not-this\"
";
        assert_eq!(package_name(manifest), Some("toimija".to_string()));
    }

    #[test]
    fn package_name_is_none_for_virtual_workspace() {
        // A workspace root with no `[package]` table owns no crate.
        let manifest = "[workspace]\nmembers = [\"a\", \"b\"]\n";
        assert_eq!(package_name(manifest), None);
    }

    #[test]
    fn focused_caps_and_falls_back_to_full_suite() {
        let temp = tempfile::tempdir().unwrap();
        let changed: Vec<PathBuf> = (0..=FOCUSED_CRATE_CAP)
            .map(|i| {
                let name = format!("crate{i}");
                write_manifest(&temp.path().join(&name), &name);
                PathBuf::from(&name).join("src/lib.rs")
            })
            .collect();

        // More distinct crates than the cap → no focused tier; the whole-repo
        // Fast command stands as the authoritative run.
        assert!(
            RustAdapter
                .focused_validation(temp.path(), &changed)
                .is_empty()
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
            index_entry: None,
        }];
        let hints = RustAdapter.read_first_hints(temp.path(), &changed);
        assert_eq!(hints[0].0, PathBuf::from("tests/foo_test.rs"));
    }
}
