//! Minimum language packs: the floor every recognized language gets —
//! detection by manifest, inferred test + lint tiers, companion-test hints.
//! One engine, one table row per language. Full adapters (Rust, Elixir) stay
//! the only per-language *code*; a new language here is data, not a module.
//! No source parsing — manifests and path templates only — and `repo.yaml`
//! validation config still beats everything inferred from a row.

use std::path::{Path, PathBuf};

use crate::adapters::LanguageAdapter;
use crate::config::RepoConfig;
use crate::git::ChangedFile;
use crate::repo::RepoKind;
use crate::validate::{self, ValidationPlan};

pub struct MinimumLang {
    pub kind: RepoKind,
    /// Honest display label for notes and telemetry (e.g. "Go").
    pub name: &'static str,
    /// Any of these at the repo root claims the repo (checked in table order,
    /// after the full adapters have had their chance).
    pub manifests: &'static [&'static str],
    /// Fast-tier (test) commands. Also the Full tier, mirroring the full
    /// adapters' shape.
    pub fast: &'static [&'static str],
    /// Strict-tier (lint/vet) commands.
    pub strict: &'static [&'static str],
    /// Source extensions the companion templates apply to.
    pub extensions: &'static [&'static str],
    /// Companion-test path templates: `{dir}`, `{stem}`, `{ext}` expand from
    /// the changed file. Candidates are existence-filtered, same as the full
    /// adapters — a hint never points at a file that is not there.
    pub companions: &'static [&'static str],
    /// A changed file whose name contains one of these is already a test;
    /// tests do not get companion hints for themselves.
    pub test_markers: &'static [&'static str],
}

pub const MINIMUMS: &[MinimumLang] = &[
    MinimumLang {
        kind: RepoKind::Go,
        name: "Go",
        manifests: &["go.mod"],
        fast: &["go test ./..."],
        strict: &["go vet ./..."],
        extensions: &["go"],
        companions: &["{dir}/{stem}_test.go"],
        test_markers: &["_test"],
    },
    MinimumLang {
        kind: RepoKind::Python,
        name: "Python",
        manifests: &["pyproject.toml", "setup.py"],
        fast: &["pytest"],
        strict: &["ruff check ."],
        extensions: &["py"],
        companions: &["{dir}/test_{stem}.py", "tests/test_{stem}.py"],
        test_markers: &["test_"],
    },
    MinimumLang {
        kind: RepoKind::Node,
        name: "JS/TS",
        manifests: &["package.json"],
        fast: &["npm test"],
        strict: &["npm run lint"],
        extensions: &["ts", "tsx", "js", "jsx"],
        companions: &["{dir}/{stem}.test.{ext}", "{dir}/{stem}.spec.{ext}"],
        test_markers: &[".test.", ".spec."],
    },
];

/// The first minimum language whose manifest exists at the repo root. Called
/// by repo-kind detection after the full adapters (Rust, Elixir) declined.
pub fn detect(repo_root: &Path) -> Option<&'static MinimumLang> {
    MINIMUMS
        .iter()
        .find(|lang| lang.manifests.iter().any(|m| repo_root.join(m).exists()))
}

pub fn for_kind(kind: RepoKind) -> Option<&'static MinimumLang> {
    MINIMUMS.iter().find(|lang| lang.kind == kind)
}

/// One adapter serves every table row; which row is fixed at construction.
#[derive(Clone, Copy)]
pub struct MinimumAdapter(pub &'static MinimumLang);

impl LanguageAdapter for MinimumAdapter {
    fn kind(&self) -> RepoKind {
        self.0.kind
    }

    fn detect(&self, repo_root: &Path) -> bool {
        self.0
            .manifests
            .iter()
            .any(|manifest| repo_root.join(manifest).exists())
    }

    fn validation_plan(&self, repo_root: &Path, config: &RepoConfig) -> ValidationPlan {
        validate::validation_plan(repo_root, config, self.0.kind)
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
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            return Vec::new();
        };
        if !self.0.extensions.contains(&ext) {
            return Vec::new();
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Vec::new();
        };
        if self
            .0
            .test_markers
            .iter()
            .any(|marker| name.contains(marker))
        {
            return Vec::new();
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            return Vec::new();
        };
        let dir = path
            .parent()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.0
            .companions
            .iter()
            .map(|template| expand_template(template, &dir, stem, ext))
            .filter(|candidate| repo_root.join(candidate).exists())
            .collect()
    }
}

/// Expand a companion template. `{dir}` is the changed file's parent
/// (repo-root-relative; empty at the root — a leading `{dir}/` collapses so
/// root-level files do not produce `/foo_test.go`).
fn expand_template(template: &str, dir: &str, stem: &str, ext: &str) -> PathBuf {
    let expanded = template
        .replace("{dir}", dir)
        .replace("{stem}", stem)
        .replace("{ext}", ext);
    PathBuf::from(expanded.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(kind: RepoKind) -> MinimumAdapter {
        MinimumAdapter(for_kind(kind).unwrap())
    }

    #[test]
    fn detection_claims_by_manifest_in_table_order() {
        let temp = tempfile::tempdir().unwrap();
        assert!(detect(temp.path()).is_none());
        std::fs::write(temp.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect(temp.path()).unwrap().kind, RepoKind::Node);
        // Go outranks Node when both manifests exist (table order).
        std::fs::write(temp.path().join("go.mod"), "module demo\n").unwrap();
        assert_eq!(detect(temp.path()).unwrap().kind, RepoKind::Go);
    }

    #[test]
    fn companions_expand_exist_filter_and_skip_tests_themselves() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("pkg")).unwrap();
        std::fs::write(temp.path().join("pkg/parser_test.go"), "").unwrap();

        let go = adapter(RepoKind::Go);
        // Candidate exists → hinted.
        assert_eq!(
            go.companion_tests(temp.path(), Path::new("pkg/parser.go")),
            vec![PathBuf::from("pkg/parser_test.go")]
        );
        // Candidate missing → no hint (existence filter).
        assert!(
            go.companion_tests(temp.path(), Path::new("pkg/lexer.go"))
                .is_empty()
        );
        // A test file is not hinted at itself.
        assert!(
            go.companion_tests(temp.path(), Path::new("pkg/parser_test.go"))
                .is_empty()
        );
        // Wrong extension → not this language's business.
        assert!(
            go.companion_tests(temp.path(), Path::new("pkg/README.md"))
                .is_empty()
        );
    }

    #[test]
    fn python_checks_both_sibling_and_tests_dir() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("tests")).unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("tests/test_engine.py"), "").unwrap();
        let python = adapter(RepoKind::Python);
        assert_eq!(
            python.companion_tests(temp.path(), Path::new("src/engine.py")),
            vec![PathBuf::from("tests/test_engine.py")]
        );
    }

    #[test]
    fn node_hints_test_and_spec_siblings_at_repo_root_too() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("app.test.ts"), "").unwrap();
        std::fs::write(temp.path().join("app.spec.ts"), "").unwrap();
        let node = adapter(RepoKind::Node);
        // Root-level file: `{dir}` is empty, the leading slash collapses.
        assert_eq!(
            node.companion_tests(temp.path(), Path::new("app.ts")),
            vec![PathBuf::from("app.test.ts"), PathBuf::from("app.spec.ts")]
        );
        // Test files themselves are skipped via the `.test.` / `.spec.` markers.
        assert!(
            node.companion_tests(temp.path(), Path::new("app.test.ts"))
                .is_empty()
        );
    }
}
